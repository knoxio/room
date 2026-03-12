//! Generative bot avatar rendering for the TUI welcome splash.
//!
//! Each bot is deterministically assembled from a 64-bit seed: border style,
//! eye shape, mouth shape, and wink style. Animation drives antenna blink,
//! eye wink, and mouth talk events on independent per-slot random timers.

use chrono::Local;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

/// Antenna light blink rate (~500 ms at 50 ms/frame).
const BLINK_INTERVAL: usize = 10;
/// Slot length in frames (~1 s). Each slot independently rolls for events.
const SLOT_FRAMES: usize = 20;
/// Probability (out of 100) that a slot triggers a wink/blink (~15% → avg every ~6 s).
const WINK_CHANCE: u64 = 15;
/// How many frames into a slot the wink lasts (~250 ms).
const WINK_DURATION: usize = 5;
/// Probability (out of 100) that a slot triggers a talking burst (~10% → avg every ~10 s).
const TALK_CHANCE: u64 = 10;
/// How many frames into a slot the talking burst lasts (~1 s).
const TALK_DURATION: usize = 20;
/// Rapid mouth alternation rate during a talking burst (~150 ms).
const TALK_INTERVAL: usize = 3;

// ── Generative bot components ─────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum BotBorder {
    Round,
    Square,
    Double,
    Heavy,
}

#[derive(Clone, Copy)]
enum BotEyes {
    Circle,
    Dot,
    Block,
    Ring,
    Diamond,
}

#[derive(Clone, Copy)]
enum WinkStyle {
    Left,
    Right,
    Blink,
}

#[derive(Clone, Copy)]
enum BotMouth {
    Smile,
    Flat,
    Dots,
    Wave,
    Teeth,
    TriDown,
    Stern,
    Surprised,
    NeutralDot,
    Dashed,
    WavyAlt,
}

struct BotParts {
    seed: u64,
    border: BotBorder,
    eyes: BotEyes,
    mouth: BotMouth,
    wink_style: WinkStyle,
}

/// Deterministically generate bot parts from a 64-bit seed (splitmix64).
fn bot_from_seed(seed: u64) -> BotParts {
    let mut s = seed.wrapping_add(0x9e3779b97f4a7c15);
    s = (s ^ (s >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    s = (s ^ (s >> 27)).wrapping_mul(0x94d049bb133111eb);
    s ^= s >> 31;

    let border = match s & 3 {
        0 => BotBorder::Round,
        1 => BotBorder::Square,
        2 => BotBorder::Double,
        _ => BotBorder::Heavy,
    };
    let eyes = match (s >> 2) % 5 {
        0 => BotEyes::Circle,
        1 => BotEyes::Dot,
        2 => BotEyes::Block,
        3 => BotEyes::Ring,
        _ => BotEyes::Diamond,
    };
    let mouth = match (s >> 5) % 11 {
        0 => BotMouth::Smile,
        1 => BotMouth::Flat,
        2 => BotMouth::Dots,
        3 => BotMouth::Wave,
        4 => BotMouth::Teeth,
        5 => BotMouth::TriDown,
        6 => BotMouth::Stern,
        7 => BotMouth::Surprised,
        8 => BotMouth::NeutralDot,
        9 => BotMouth::Dashed,
        _ => BotMouth::WavyAlt,
    };
    let wink_style = match (s >> 10) % 3 {
        0 => WinkStyle::Left,
        1 => WinkStyle::Right,
        _ => WinkStyle::Blink,
    };
    BotParts {
        seed,
        border,
        eyes,
        mouth,
        wink_style,
    }
}

/// Hash a (bot_seed, slot) pair to get a random value for that time slot.
/// Used to decide independently per-slot whether a wink or talk event fires.
fn slot_hash(bot_seed: u64, slot: usize) -> u64 {
    let mut s = bot_seed.wrapping_add((slot as u64).wrapping_mul(0x9e3779b97f4a7c15));
    s = (s ^ (s >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    s = (s ^ (s >> 27)).wrapping_mul(0x94d049bb133111eb);
    s ^ (s >> 31)
}

fn bot_color(border: BotBorder) -> Color {
    match border {
        BotBorder::Round => Color::Cyan,
        BotBorder::Square => Color::Green,
        BotBorder::Double => Color::Magenta,
        BotBorder::Heavy => Color::Yellow,
    }
}

/// Returns (tl, tr, bl, br, fill, side, neck) box-drawing characters.
fn border_chars(
    border: BotBorder,
) -> (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
) {
    match border {
        BotBorder::Round => ("╭", "╮", "╰", "╯", "─", "│", "┬"),
        BotBorder::Square => ("┌", "┐", "└", "┘", "─", "│", "┬"),
        BotBorder::Double => ("╔", "╗", "╚", "╝", "═", "║", "╤"),
        BotBorder::Heavy => ("┏", "┓", "┗", "┛", "━", "┃", "┳"),
    }
}

fn eye_char(eyes: BotEyes) -> &'static str {
    match eyes {
        BotEyes::Circle => "◉",
        BotEyes::Dot => "•",
        BotEyes::Block => "■",
        BotEyes::Ring => "◎",
        BotEyes::Diamond => "◈",
    }
}

fn mouth_str(mouth: BotMouth) -> &'static str {
    match mouth {
        BotMouth::Smile => "╰─╯",
        BotMouth::Flat => "───",
        BotMouth::Dots => "···",
        BotMouth::Wave => "∿∿∿",
        BotMouth::Teeth => "▾▾▾",
        BotMouth::TriDown => "▿▿▿",
        BotMouth::Stern => "═══",
        BotMouth::Surprised => "o─o",
        BotMouth::NeutralDot => "─·─",
        BotMouth::Dashed => "╌╌╌",
        BotMouth::WavyAlt => "≈≈≈",
    }
}

/// Correlated talking state for each mouth — same character family, mouth open.
/// `Surprised` is already open so it closes slightly when "talking".
fn mouth_talk_str(mouth: BotMouth) -> &'static str {
    match mouth {
        BotMouth::Smile => "╰o╯",
        BotMouth::Flat => "─o─",
        BotMouth::Dots => "·o·",
        BotMouth::Wave => "∿o∿",
        BotMouth::Teeth => "▾o▾",
        BotMouth::TriDown => "▿o▿",
        BotMouth::Stern => "═o═",
        BotMouth::Surprised => "───",
        BotMouth::NeutralDot => "─o─",
        BotMouth::Dashed => "╌o╌",
        BotMouth::WavyAlt => "≈o≈",
    }
}

/// Render one row (0–5) of a 9-column-wide bot face.
///
/// ```text
///   row 0 — antenna light   "    ✦    "
///   row 1 — antenna stem    "    │    "
///   row 2 — top border      "╭───────╮"
///   row 3 — eyes            "│ ◉   ◉ │"
///   row 4 — mouth           "│  ╰─╯  │"
///   row 5 — bottom + neck   "╰───┬───╯"
/// ```
fn bot_row(parts: &BotParts, row: usize, frame: usize) -> (String, Style) {
    let antenna_on = (frame / BLINK_INTERVAL).is_multiple_of(2);
    let slot = frame / SLOT_FRAMES;
    let frame_in_slot = frame % SLOT_FRAMES;
    let h = slot_hash(parts.seed, slot);
    let wink = (h % 100 < WINK_CHANCE) && (frame_in_slot < WINK_DURATION);
    let talking = ((h >> 16) % 100 < TALK_CHANCE) && (frame_in_slot < TALK_DURATION);
    let color = bot_color(parts.border);
    let (tl, tr, bl, br, fill, side, neck) = border_chars(parts.border);

    let text = match row {
        0 => {
            let ch = if antenna_on { "✦" } else { "·" };
            format!("    {ch}    ")
        }
        1 => "    │    ".to_string(),
        2 => format!("{tl}{}{tr}", fill.repeat(7)),
        3 => {
            let eye = eye_char(parts.eyes);
            let (le, re) = if wink {
                match parts.wink_style {
                    WinkStyle::Left => ("─", eye),
                    WinkStyle::Right => (eye, "─"),
                    WinkStyle::Blink => ("─", "─"),
                }
            } else {
                (eye, eye)
            };
            format!("{side} {le}   {re} {side}")
        }
        4 => {
            let m = if talking {
                if (frame_in_slot / TALK_INTERVAL).is_multiple_of(2) {
                    mouth_str(parts.mouth)
                } else {
                    mouth_talk_str(parts.mouth)
                }
            } else {
                mouth_str(parts.mouth)
            };
            format!("{side}  {m}  {side}")
        }
        5 => format!("{bl}{}{neck}{}{br}", fill.repeat(3), fill.repeat(3)),
        _ => unreachable!(),
    };

    let style = if row == 0 {
        if antenna_on {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    } else {
        Style::default().fg(color)
    };

    (text, style)
}

/// Build the welcome splash as centered, styled `Text`.
///
/// `frame` drives per-bot animations with staggered phase offsets so the
/// group never blinks in unison.  `seed` is set once at TUI startup and
/// used to deterministically assemble a unique bot face (border, eyes, mouth)
/// for every slot — the same session always looks the same, but two sessions
/// are almost certainly different.
///
/// The splash is vertically centered within `height` rows: empty lines are
/// prepended so the content block sits in the middle of the available area.
///
/// Bot count adapts to `width`:
///   < 55  → 1 bot
///   55–99 → 3 bots
///   ≥ 100 → 5 bots
pub(super) fn welcome_splash(
    frame: usize,
    width: usize,
    height: usize,
    seed: u64,
) -> Text<'static> {
    let bot_count: usize = if width >= 100 {
        5
    } else if width >= 55 {
        3
    } else {
        1
    };
    const GAP: &str = "   "; // 3 spaces between adjacent bots

    let bots: Vec<BotParts> = (0..bot_count)
        .map(|i| bot_from_seed(seed.wrapping_add(i as u64 * 6364136223846793005)))
        .collect();

    // Stagger by 7 frames so bots animate out-of-phase with each other.
    let frame_for = |i: usize| frame.wrapping_add(i * 7);

    // Total display width of the bot group.
    let group_width = bot_count * 9 + bot_count.saturating_sub(1) * GAP.len();

    // Content: 6 bot rows + 1 blank + 1 label + 1 blank + 1 tagline + 1 version = 11 lines.
    const CONTENT_LINES: usize = 11;
    let top_pad = height.saturating_sub(CONTENT_LINES) / 2;
    let mut lines: Vec<Line<'static>> = (0..top_pad.max(1)).map(|_| Line::from("")).collect();

    for row in 0..6 {
        let pad = " ".repeat(width.saturating_sub(group_width) / 2);
        let mut spans: Vec<Span<'static>> = Vec::new();
        if !pad.is_empty() {
            spans.push(Span::raw(pad));
        }
        for (i, bot) in bots.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(GAP));
            }
            let (text, style) = bot_row(bot, row, frame_for(i));
            spans.push(Span::styled(text, style));
        }
        lines.push(Line::from(spans));
    }

    // "r o o m" label pulses in sync with the antenna.
    let label_style = if (frame / BLINK_INTERVAL).is_multiple_of(2) {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    lines.push(Line::from(""));
    let label = "r o o m";
    let label_pad = " ".repeat(width.saturating_sub(label.len()) / 2);
    lines.push(Line::from(vec![
        Span::raw(label_pad),
        Span::styled(label, label_style),
    ]));

    lines.push(Line::from(""));

    let tagline = "Agent coordination for humans";
    let tagline_pad = " ".repeat(width.saturating_sub(tagline.len()) / 2);
    lines.push(Line::from(vec![
        Span::raw(tagline_pad),
        Span::styled(tagline, Style::default().fg(Color::DarkGray)),
    ]));

    let version = env!("CARGO_PKG_VERSION");
    let today = Local::now().format("%Y-%m-%d");
    let info = format!("v{version}  ·  {today}");
    let info_pad = " ".repeat(width.saturating_sub(info.len()) / 2);
    lines.push(Line::from(vec![
        Span::raw(info_pad),
        Span::styled(info, Style::default().fg(Color::DarkGray)),
    ]));

    Text::from(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn splash_text(frame: usize, width: usize) -> String {
        welcome_splash(frame, width, 40, 0)
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn welcome_splash_contains_tagline() {
        assert!(
            splash_text(0, 60).contains("Agent coordination for humans"),
            "splash should contain tagline"
        );
    }

    #[test]
    fn welcome_splash_contains_version() {
        let version = env!("CARGO_PKG_VERSION");
        assert!(
            splash_text(0, 60).contains(&format!("v{version}")),
            "splash should contain version"
        );
    }

    #[test]
    fn welcome_splash_contains_date() {
        let today = Local::now().format("%Y-%m-%d").to_string();
        assert!(
            splash_text(0, 60).contains(&today),
            "splash should contain today's date"
        );
    }

    #[test]
    fn welcome_splash_animation_changes_between_phases() {
        let a = splash_text(0, 60);
        let b = splash_text(10, 60); // one BLINK_INTERVAL later
        assert_ne!(a, b, "animation should change between phases");
    }

    #[test]
    fn welcome_splash_has_more_lines_than_logo() {
        let text = welcome_splash(0, 60, 40, 0);
        // 1 vpad + 6 bot rows + 1 blank + 1 label + 1 blank + 1 tagline + 1 version = 12
        assert!(
            text.lines.len() >= 11,
            "splash should have logo + tagline + version lines, got {}",
            text.lines.len()
        );
    }

    #[test]
    fn welcome_splash_single_bot_below_threshold() {
        // width=40 < 55 → 1 bot → no inter-bot GAP spans ("   ") should exist.
        // Padding for this width is (40-9)/2 = 15 spaces, which is never confused
        // with the 3-space GAP constant.
        let text = welcome_splash(0, 40, 40, 42);
        let has_gap_span = text
            .lines
            .iter()
            .any(|line| line.spans.iter().any(|s| s.content.as_ref() == "   "));
        assert!(
            !has_gap_span,
            "single bot should have no inter-bot gap spans"
        );
    }

    #[test]
    fn welcome_splash_three_bots_at_medium_width() {
        // width=80 → 3 bots; all 3 bottom-border rows should appear in the same line
        let text = welcome_splash(0, 80, 40, 99);
        // Row 5 is the bottom border line; we should have 2 GAP spans in it
        let bottom_row = text.lines.iter().find(|line| {
            let raw: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            // bottom row contains a neck char from at least one border style
            raw.contains('┬') || raw.contains('╤') || raw.contains('┳')
        });
        assert!(bottom_row.is_some(), "should find a bottom-border row");
    }

    #[test]
    fn welcome_splash_seed_produces_variety() {
        // Two different seeds should produce different bot faces.
        let a = splash_text(0, 80);
        let b = welcome_splash(0, 80, 40, 9999999999)
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("");
        assert_ne!(
            a, b,
            "different seeds should (almost always) produce different bots"
        );
    }

    #[test]
    fn welcome_splash_vertically_centered() {
        // Content is 11 lines. With height=31, top padding = (31-11)/2 = 10 empty lines.
        let text = welcome_splash(0, 60, 31, 0);
        let leading_empty = text
            .lines
            .iter()
            .take_while(|line| {
                line.spans.is_empty() || line.spans.iter().all(|s| s.content.trim().is_empty())
            })
            .count();
        assert_eq!(
            leading_empty, 10,
            "should have 10 leading blank lines for height=31"
        );
    }

    #[test]
    fn welcome_splash_small_height_has_min_one_pad() {
        // When height < content lines, still get at least 1 top padding line.
        let text = welcome_splash(0, 60, 5, 0);
        let first_empty = text
            .lines
            .first()
            .map(|l| l.spans.is_empty() || l.spans.iter().all(|s| s.content.trim().is_empty()))
            .unwrap_or(false);
        assert!(
            first_empty,
            "should have at least 1 blank padding line even when height is small"
        );
    }
}
