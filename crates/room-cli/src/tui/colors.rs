//! User color assignment for the TUI.
//!
//! Assigns deterministic, collision-avoiding colors to usernames from a
//! fixed palette. Used by the message renderer and member panel.

use std::collections::{HashMap, HashSet};

use ratatui::style::Color;

/// Color palette for user names.
const PALETTE: &[Color] = &[
    Color::Yellow,
    Color::Cyan,
    Color::Green,
    Color::Magenta,
    Color::LightYellow,
    Color::LightCyan,
    Color::LightGreen,
    Color::LightMagenta,
    Color::LightRed,
    Color::LightBlue,
];

/// Persistent map of username -> assigned color. Stored in TUI state.
pub(crate) type ColorMap = HashMap<String, Color>;

/// Assign a color to a username, preferring unused palette colors.
///
/// If the user already has a color, returns it. Otherwise picks the
/// hash-preferred color if available, or the first unused palette color.
/// Falls back to the hash color when all palette slots are taken.
pub(crate) fn assign_color(username: &str, color_map: &mut ColorMap) -> Color {
    if let Some(&color) = color_map.get(username) {
        return color;
    }
    let used: HashSet<Color> = color_map.values().copied().collect();
    let hash = username.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(b as usize)
    });
    let preferred = PALETTE[hash % PALETTE.len()];
    if !used.contains(&preferred) {
        color_map.insert(username.to_owned(), preferred);
        return preferred;
    }
    // Hash color is taken — find first unused palette color.
    for &color in PALETTE {
        if !used.contains(&color) {
            color_map.insert(username.to_owned(), color);
            return color;
        }
    }
    // All palette colors used — accept collision with hash color.
    color_map.insert(username.to_owned(), preferred);
    preferred
}

/// Look up a username's color from the map, falling back to the hash-based
/// palette index if the user has not been assigned a color yet.
pub(crate) fn user_color(username: &str, color_map: &ColorMap) -> Color {
    if let Some(&color) = color_map.get(username) {
        return color;
    }
    let hash = username.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(b as usize)
    });
    PALETTE[hash % PALETTE.len()]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assign_color_returns_consistent_color() {
        let mut cm = ColorMap::new();
        let c1 = assign_color("alice", &mut cm);
        let c2 = assign_color("alice", &mut cm);
        assert_eq!(c1, c2);
    }

    #[test]
    fn assign_color_different_users_get_different_colors() {
        let mut cm = ColorMap::new();
        let c1 = assign_color("alice", &mut cm);
        let c2 = assign_color("bob", &mut cm);
        assert_ne!(c1, c2);
    }

    #[test]
    fn assign_color_avoids_collision_when_preferred_taken() {
        let mut cm = ColorMap::new();
        // "alice" gets her preferred color first.
        let alice_color = assign_color("alice", &mut cm);
        // Find another username that hashes to the same palette index.
        let mut collider = String::new();
        for i in 0u32..10_000 {
            let name = format!("u{i}");
            let hash = name.bytes().fold(0usize, |acc, b| {
                acc.wrapping_mul(31).wrapping_add(b as usize)
            });
            if PALETTE[hash % PALETTE.len()] == alice_color {
                collider = name;
                break;
            }
        }
        assert!(!collider.is_empty(), "could not find a colliding username");
        let collider_color = assign_color(&collider, &mut cm);
        // The collider should NOT get Alice's color — it should get a different unused one.
        assert_ne!(collider_color, alice_color);
    }

    #[test]
    fn assign_color_fills_all_palette_slots() {
        let mut cm = ColorMap::new();
        let mut colors = HashSet::new();
        // Assign colors to enough users to fill the palette.
        for i in 0..PALETTE.len() {
            let c = assign_color(&format!("user{i}"), &mut cm);
            colors.insert(c);
        }
        // Every palette color should be used exactly once.
        assert_eq!(colors.len(), PALETTE.len());
    }

    #[test]
    fn assign_color_accepts_collision_when_palette_exhausted() {
        let mut cm = ColorMap::new();
        // Fill all palette slots.
        for i in 0..PALETTE.len() {
            assign_color(&format!("user{i}"), &mut cm);
        }
        // The 11th user must accept a collision.
        let c = assign_color("overflow", &mut cm);
        assert!(PALETTE.contains(&c));
    }

    #[test]
    fn user_color_uses_map_when_present() {
        let mut cm = ColorMap::new();
        cm.insert("alice".to_owned(), Color::LightRed);
        assert_eq!(user_color("alice", &cm), Color::LightRed);
    }

    #[test]
    fn user_color_falls_back_to_hash_when_not_in_map() {
        let cm = ColorMap::new();
        let c = user_color("alice", &cm);
        let hash = "alice".bytes().fold(0usize, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(b as usize)
        });
        assert_eq!(c, PALETTE[hash % PALETTE.len()]);
    }
}
