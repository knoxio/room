//! Pure parsing helpers extracted from `tui/input.rs`.
//!
//! These functions are stateless — they take strings (or simple mutable
//! references to `Vec`/`HashMap`) and return parsed values. Keeping them in a
//! dedicated module makes them easy to unit-test in isolation without
//! constructing a full `InputState`.

use std::collections::HashMap;

use room_protocol::SubscriptionTier;

use super::input::Action;

/// If the char immediately before `cursor_pos` in `buf` is `\`, removes it
/// and inserts `\n`, returning the new cursor position. Returns `None` if the
/// precondition is not met (no preceding backslash or cursor at start).
///
/// This mirrors the backslash+Enter key binding in the TUI event loop.
pub(super) fn apply_backslash_enter(buf: &mut String, cursor_pos: usize) -> Option<usize> {
    if cursor_pos > 0 && buf[..cursor_pos].ends_with('\\') {
        let bs_pos = cursor_pos - 1; // '\\' is ASCII (1 byte)
        buf.remove(bs_pos);
        buf.insert(bs_pos, '\n');
        Some(bs_pos + 1)
    } else {
        None
    }
}

/// Normalize pasted text: convert `\r\n` to `\n`, then stray `\r` to `\n`.
///
/// Called on `Event::Paste` to ensure consistent newline handling regardless
/// of the clipboard's line-ending convention (Windows, old Mac, Unix).
pub(super) fn normalize_paste(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Parse `/dm <user> <message>` input into a `DmRoom` action.
///
/// Returns `Some(Action::DmRoom { .. })` when the input is a valid `/dm`
/// command with both a target user and message content. Returns `None` for
/// incomplete input (missing user or message) — those fall through to
/// `build_payload` for backwards-compatible intra-room DM handling.
pub(super) fn parse_dm_input(input: &str) -> Option<Action> {
    let rest = input.strip_prefix("/dm ")?;
    let mut parts = rest.splitn(2, ' ');
    let target_user = parts.next().filter(|s| !s.is_empty())?;
    let content = parts.next().filter(|s| !s.is_empty())?;
    Some(Action::DmRoom {
        target_user: target_user.to_owned(),
        content: content.to_owned(),
    })
}

/// Convert TUI input to a JSON envelope for the broker.
pub(super) fn build_payload(input: &str) -> String {
    // `/dm <user> <message>` — preserve spaces in the message body.
    // NOTE: This branch is now only reached when `parse_dm_input` returns
    // None (e.g. `/dm user` with no message body).
    if let Some(rest) = input.strip_prefix("/dm ") {
        let mut parts = rest.splitn(2, ' ');
        let to = parts.next().unwrap_or("").to_owned();
        let content = parts.next().unwrap_or("").to_owned();
        return serde_json::json!({ "type": "dm", "to": to, "content": content }).to_string();
    }
    if let Some(rest) = input.strip_prefix('/') {
        let mut parts = rest.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_owned();
        let params: Vec<String> = parts
            .next()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        serde_json::json!({ "type": "command", "cmd": cmd, "params": params }).to_string()
    } else {
        serde_json::json!({ "type": "message", "content": input }).to_string()
    }
}

/// Seed `online_users` and `user_statuses` from the broker's `/who` response content.
///
/// The broker sends `"online — alice, bob: away, charlie"` (or `"no users online"`).
/// Each entry is either a bare username or `username: status`.
/// Merges into the existing list without removing users added by Join events.
pub(super) fn seed_online_users_from_who(
    content: &str,
    online_users: &mut Vec<String>,
    user_statuses: &mut HashMap<String, String>,
) {
    if let Some(rest) = content.strip_prefix("online \u{2014} ") {
        for entry in rest.split(", ") {
            let (username, status) = match entry.split_once(": ") {
                Some((u, s)) => (u.trim().to_owned(), s.trim().to_owned()),
                None => (entry.trim().to_owned(), String::new()),
            };
            // Usernames never contain spaces. If a bare entry (no ": ")
            // contains whitespace, it is a status-text fragment leaked by
            // a comma in the status string — skip it (#656).
            if username.is_empty() || username.contains(' ') {
                continue;
            }
            if !online_users.contains(&username) {
                online_users.push(username.clone());
            }
            user_statuses.insert(username, status);
        }
    }
}

/// Parse a `/set_status` system broadcast into `(username, status)`.
///
/// The broker broadcasts either:
/// - `"alice set status: busy"` → `Some(("alice", "busy"))`
/// - `"alice cleared their status"` → `Some(("alice", ""))`
pub(super) fn parse_status_broadcast(content: &str) -> Option<(String, String)> {
    if let Some(rest) = content.strip_suffix(" cleared their status") {
        return Some((rest.to_owned(), String::new()));
    }
    if let Some((name, status)) = content.split_once(" set status: ") {
        return Some((name.to_owned(), status.to_owned()));
    }
    None
}

/// Parse a `/kick` system broadcast into the kicked username.
///
/// The broker broadcasts `"alice kicked bob (token invalidated)"`.
/// Returns `Some("bob")` — the target user who was kicked.
pub(super) fn parse_kick_broadcast(content: &str) -> Option<&str> {
    let rest = content.strip_suffix(" (token invalidated)")?;
    let (_issuer, target) = rest.split_once(" kicked ")?;
    if target.is_empty() {
        return None;
    }
    Some(target)
}

/// Parse a `/subscribe` system broadcast into `(username, SubscriptionTier)`.
///
/// The broker broadcasts `"alice subscribed to room-dev (tier: full)"`.
/// Returns `Some(("alice", SubscriptionTier::Full))`.
pub(super) fn parse_subscription_broadcast(content: &str) -> Option<(String, SubscriptionTier)> {
    // Strip trailing ")"
    let rest = content.strip_suffix(')')?;
    // Split on " (tier: "
    let (name_room, tier_str) = rest.split_once(" (tier: ")?;
    // Split on " subscribed to "
    let (name, _room) = name_room.split_once(" subscribed to ")?;
    if name.is_empty() {
        return None;
    }
    let tier = tier_str.parse().ok()?;
    Some((name.to_owned(), tier))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::input::Action;
    use super::*;

    // ── seed_online_users_from_who tests ─────────────────────────────────────

    #[test]
    fn seed_who_populates_users() {
        let mut users = Vec::new();
        let mut statuses = HashMap::new();
        seed_online_users_from_who(
            "online \u{2014} alice, bob, charlie",
            &mut users,
            &mut statuses,
        );
        assert_eq!(users, ["alice", "bob", "charlie"]);
        assert_eq!(statuses.get("alice").unwrap(), "");
    }

    #[test]
    fn seed_who_extracts_statuses() {
        let mut users = Vec::new();
        let mut statuses = HashMap::new();
        seed_online_users_from_who(
            "online \u{2014} alice: away, bob: coding, charlie",
            &mut users,
            &mut statuses,
        );
        assert_eq!(users, ["alice", "bob", "charlie"]);
        assert_eq!(statuses.get("alice").unwrap(), "away");
        assert_eq!(statuses.get("bob").unwrap(), "coding");
        assert_eq!(statuses.get("charlie").unwrap(), "");
    }

    #[test]
    fn seed_who_no_users_online_is_noop() {
        let mut users = Vec::new();
        let mut statuses = HashMap::new();
        seed_online_users_from_who("no users online", &mut users, &mut statuses);
        assert!(users.is_empty());
        assert!(statuses.is_empty());
    }

    #[test]
    fn seed_who_does_not_duplicate_existing_users() {
        let mut users = vec!["alice".to_owned()];
        let mut statuses = HashMap::new();
        seed_online_users_from_who("online \u{2014} alice, bob", &mut users, &mut statuses);
        assert_eq!(users, ["alice", "bob"]);
    }

    #[test]
    fn seed_who_rejects_bare_entry_with_spaces() {
        // If the broker ever leaks a raw comma in status text, the parser
        // must not treat multi-word fragments as usernames (#656).
        let mut users = Vec::new();
        let mut statuses = HashMap::new();
        seed_online_users_from_who(
            "online \u{2014} alice: PR #630 merged, #636 filed, bob",
            &mut users,
            &mut statuses,
        );
        assert_eq!(users, ["alice", "bob"]);
        assert!(
            !users.contains(&"#636 filed".to_owned()),
            "status fragment must not appear as a user"
        );
    }

    #[test]
    fn seed_who_single_word_bare_entry_accepted() {
        // A single word without spaces is a legitimate bare username.
        let mut users = Vec::new();
        let mut statuses = HashMap::new();
        seed_online_users_from_who("online \u{2014} alice, bob", &mut users, &mut statuses);
        assert_eq!(users, ["alice", "bob"]);
    }

    #[test]
    fn seed_who_unrelated_system_message_is_noop() {
        let mut users = Vec::new();
        let mut statuses = HashMap::new();
        seed_online_users_from_who("alice set status: away", &mut users, &mut statuses);
        assert!(users.is_empty());
        assert!(statuses.is_empty());
    }

    // ── parse_status_broadcast tests ─────────────────────────────────────────

    #[test]
    fn parse_status_set() {
        let result = parse_status_broadcast("alice set status: busy");
        assert_eq!(result, Some(("alice".to_owned(), "busy".to_owned())));
    }

    #[test]
    fn parse_status_cleared() {
        let result = parse_status_broadcast("alice cleared their status");
        assert_eq!(result, Some(("alice".to_owned(), String::new())));
    }

    #[test]
    fn parse_status_unrelated_message() {
        assert!(parse_status_broadcast("alice joined").is_none());
        assert!(parse_status_broadcast("hello world").is_none());
    }

    #[test]
    fn parse_status_with_spaces_in_name() {
        let result = parse_status_broadcast("my-agent set status: reviewing PR");
        assert_eq!(
            result,
            Some(("my-agent".to_owned(), "reviewing PR".to_owned()))
        );
    }

    // ── parse_kick_broadcast tests ──────────────────────────────────────────

    #[test]
    fn parse_kick_standard() {
        let result = parse_kick_broadcast("alice kicked bob (token invalidated)");
        assert_eq!(result, Some("bob"));
    }

    #[test]
    fn parse_kick_hyphenated_names() {
        let result = parse_kick_broadcast("room-host kicked my-agent (token invalidated)");
        assert_eq!(result, Some("my-agent"));
    }

    #[test]
    fn parse_kick_unrelated_message() {
        assert!(parse_kick_broadcast("alice set status: busy").is_none());
        assert!(parse_kick_broadcast("hello world").is_none());
        assert!(parse_kick_broadcast("alice cleared their status").is_none());
    }

    #[test]
    fn parse_kick_missing_suffix() {
        assert!(parse_kick_broadcast("alice kicked bob").is_none());
    }

    #[test]
    fn parse_kick_missing_target() {
        assert!(parse_kick_broadcast("alice kicked  (token invalidated)").is_none());
    }

    // ── parse_subscription_broadcast tests ────────────────────────────────────

    #[test]
    fn parse_subscription_full() {
        let result = parse_subscription_broadcast("alice subscribed to room-dev (tier: full)");
        assert_eq!(result, Some(("alice".to_owned(), SubscriptionTier::Full)));
    }

    #[test]
    fn parse_subscription_mentions_only() {
        let result =
            parse_subscription_broadcast("bob subscribed to my-room (tier: mentions_only)");
        assert_eq!(
            result,
            Some(("bob".to_owned(), SubscriptionTier::MentionsOnly))
        );
    }

    #[test]
    fn parse_subscription_unsubscribed() {
        let result = parse_subscription_broadcast("carol subscribed to test (tier: unsubscribed)");
        assert_eq!(
            result,
            Some(("carol".to_owned(), SubscriptionTier::Unsubscribed))
        );
    }

    #[test]
    fn parse_subscription_unrelated_message() {
        assert!(parse_subscription_broadcast("alice set status: busy").is_none());
        assert!(parse_subscription_broadcast("hello world").is_none());
        assert!(parse_subscription_broadcast("alice kicked bob (token invalidated)").is_none());
    }

    #[test]
    fn parse_subscription_hyphenated_username() {
        let result = parse_subscription_broadcast("my-agent subscribed to agent-room (tier: full)");
        assert_eq!(
            result,
            Some(("my-agent".to_owned(), SubscriptionTier::Full))
        );
    }

    // ── build_payload tests ───────────────────────────────────────────────────

    #[test]
    fn build_payload_plain_text_is_message_type() {
        let payload = build_payload("hello world");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"], "hello world");
    }

    #[test]
    fn build_payload_dm_command() {
        let payload = build_payload("/dm alice hey there");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["type"], "dm");
        assert_eq!(v["to"], "alice");
        assert_eq!(v["content"], "hey there");
    }

    #[test]
    fn build_payload_slash_command_becomes_command_type() {
        let payload = build_payload("/kick alice");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "kick");
    }

    #[test]
    fn build_payload_who_command() {
        let payload = build_payload("/who");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "who");
    }

    #[test]
    fn build_payload_dm_preserves_spaces_in_content() {
        let payload = build_payload("/dm bob hello   world");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["content"], "hello   world");
    }

    // ── parse_dm_input tests ────────────────────────────────────────────────

    #[test]
    fn parse_dm_input_returns_dm_room_action() {
        let action = parse_dm_input("/dm alice hello there").unwrap();
        match action {
            Action::DmRoom {
                target_user,
                content,
            } => {
                assert_eq!(target_user, "alice");
                assert_eq!(content, "hello there");
            }
            _ => panic!("expected DmRoom action"),
        }
    }

    #[test]
    fn parse_dm_input_preserves_spaces_in_content() {
        let action = parse_dm_input("/dm bob hello   world").unwrap();
        match action {
            Action::DmRoom { content, .. } => {
                assert_eq!(content, "hello   world");
            }
            _ => panic!("expected DmRoom action"),
        }
    }

    #[test]
    fn parse_dm_input_returns_none_for_missing_content() {
        assert!(parse_dm_input("/dm alice").is_none());
    }

    #[test]
    fn parse_dm_input_returns_none_for_missing_user() {
        assert!(parse_dm_input("/dm ").is_none());
    }

    #[test]
    fn parse_dm_input_returns_none_for_non_dm() {
        assert!(parse_dm_input("/who").is_none());
        assert!(parse_dm_input("hello world").is_none());
    }

    #[test]
    fn parse_dm_input_returns_none_for_user_only_with_trailing_space() {
        // "/dm alice " has empty content after splitting
        assert!(parse_dm_input("/dm alice ").is_none());
    }

    // ── apply_backslash_enter ───────────────────────────────────────────────

    #[test]
    fn backslash_enter_at_end_replaces_backslash_with_newline() {
        let mut buf = String::from("hello\\");
        let pos = apply_backslash_enter(&mut buf, 6);
        assert_eq!(pos, Some(6));
        assert_eq!(buf, "hello\n");
    }

    #[test]
    fn backslash_enter_mid_buffer_replaces_at_cursor() {
        let mut buf = String::from("foo\\bar");
        let pos = apply_backslash_enter(&mut buf, 4);
        assert_eq!(pos, Some(4));
        assert_eq!(buf, "foo\nbar");
    }

    #[test]
    fn backslash_enter_no_preceding_backslash_returns_none() {
        let mut buf = String::from("hello");
        let pos = apply_backslash_enter(&mut buf, 5);
        assert_eq!(pos, None);
        assert_eq!(buf, "hello");
    }

    #[test]
    fn backslash_enter_cursor_at_start_returns_none() {
        let mut buf = String::from("\\hello");
        let pos = apply_backslash_enter(&mut buf, 0);
        assert_eq!(pos, None);
        assert_eq!(buf, "\\hello");
    }

    #[test]
    fn backslash_enter_empty_buffer_returns_none() {
        let mut buf = String::new();
        let pos = apply_backslash_enter(&mut buf, 0);
        assert_eq!(pos, None);
    }

    #[test]
    fn backslash_enter_cursor_not_at_backslash_returns_none() {
        let mut buf = String::from("a\\b");
        let pos = apply_backslash_enter(&mut buf, 1);
        assert_eq!(pos, None);
        assert_eq!(buf, "a\\b");
    }

    #[test]
    fn backslash_enter_double_backslash_replaces_last_one() {
        let mut buf = String::from("foo\\\\");
        let pos = apply_backslash_enter(&mut buf, 5);
        assert_eq!(pos, Some(5));
        assert_eq!(buf, "foo\\\n");
    }

    // ── normalize_paste tests ───────────────────────────────────────────────

    #[test]
    fn normalize_paste_unix_newlines_unchanged() {
        assert_eq!(normalize_paste("hello\nworld"), "hello\nworld");
    }

    #[test]
    fn normalize_paste_windows_crlf_to_lf() {
        assert_eq!(normalize_paste("hello\r\nworld"), "hello\nworld");
    }

    #[test]
    fn normalize_paste_old_mac_cr_to_lf() {
        assert_eq!(normalize_paste("hello\rworld"), "hello\nworld");
    }

    #[test]
    fn normalize_paste_mixed_endings() {
        assert_eq!(normalize_paste("a\r\nb\rc\nd"), "a\nb\nc\nd");
    }

    #[test]
    fn normalize_paste_no_newlines() {
        assert_eq!(normalize_paste("plain text"), "plain text");
    }

    #[test]
    fn normalize_paste_empty() {
        assert_eq!(normalize_paste(""), "");
    }

    #[test]
    fn normalize_paste_multiple_crlf() {
        assert_eq!(normalize_paste("a\r\n\r\nb"), "a\n\nb");
    }
}
