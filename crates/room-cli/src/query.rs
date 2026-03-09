//! Query filter for the `room query` subcommand.
//!
//! [`QueryFilter`] determines whether a given message should be included in
//! query output. All fields are optional — a missing field means "no constraint
//! on that dimension". A message passes [`QueryFilter::matches`] only when
//! every present constraint is satisfied (logical AND).

use chrono::{DateTime, Utc};
use regex::Regex;
use room_protocol::Message;

/// Filter criteria for the `room query` subcommand (née `room poll`).
///
/// Constructed by the CLI flag parser and evaluated per-message. The `limit`
/// and `ascending` fields control result set size and ordering; they are
/// applied externally by the caller after filtering, not inside `matches`.
#[derive(Debug, Clone, Default)]
pub struct QueryFilter {
    /// Only include messages from these rooms. Empty = all rooms.
    pub rooms: Vec<String>,
    /// Only include messages sent by these users. Empty = all users.
    pub users: Vec<String>,
    /// Only include messages whose content contains this substring
    /// (case-sensitive).
    pub content_search: Option<String>,
    /// Only include messages whose content matches this regex pattern.
    ///
    /// Stored as `String` to keep the struct `Clone`-able; compiled inside
    /// [`matches`][Self::matches] on each call. An invalid pattern causes the
    /// message to be excluded (treated as "no match").
    pub content_regex: Option<String>,
    /// Only include messages whose sequence number is strictly greater than
    /// this value. Tuple is `(room_id, seq)`. The constraint is skipped for
    /// messages whose `room_id` differs from the filter room.
    pub after_seq: Option<(String, u64)>,
    /// Only include messages whose sequence number is strictly less than this
    /// value. Tuple is `(room_id, seq)`. Skipped for messages from other rooms.
    pub before_seq: Option<(String, u64)>,
    /// Only include messages with a timestamp strictly after this instant.
    pub after_ts: Option<DateTime<Utc>>,
    /// Only include messages with a timestamp strictly before this instant.
    pub before_ts: Option<DateTime<Utc>>,
    /// Only include messages that @mention this username.
    pub mention_user: Option<String>,
    /// Exclude `DirectMessage` variants (public-channel filter).
    pub public_only: bool,
    /// Only include the single message with this exact `(room_id, seq)`.
    ///
    /// When set, all other seq-based filters are ignored; the match is exact.
    /// DM privacy is still enforced externally by the caller.
    pub target_id: Option<(String, u64)>,
    /// Maximum number of messages to return. Applied externally by the caller.
    pub limit: Option<usize>,
    /// If `true`, return messages oldest-first. If `false`, newest-first.
    /// Applied externally by the caller.
    pub ascending: bool,
}

impl QueryFilter {
    /// Returns `true` if `msg` satisfies all constraints in this filter.
    ///
    /// `room_id` is the room in which `msg` arrived; it is used when comparing
    /// against `after_seq`/`before_seq` (which carry their own room component).
    pub fn matches(&self, msg: &Message, room_id: &str) -> bool {
        // ── room filter ───────────────────────────────────────────────────────
        if !self.rooms.is_empty() && !self.rooms.iter().any(|r| r == room_id) {
            return false;
        }

        // ── user filter ───────────────────────────────────────────────────────
        if !self.users.is_empty() && !self.users.iter().any(|u| u == msg.user()) {
            return false;
        }

        // ── public_only: skip DirectMessage variants ──────────────────────────
        if self.public_only {
            if let Message::DirectMessage { .. } = msg {
                return false;
            }
        }

        // ── content_search: substring match ───────────────────────────────────
        if let Some(ref needle) = self.content_search {
            match msg.content() {
                Some(content) if content.contains(needle.as_str()) => {}
                _ => return false,
            }
        }

        // ── content_regex: regex match ─────────────────────────────────────────
        if let Some(ref pattern) = self.content_regex {
            match Regex::new(pattern) {
                Ok(re) => match msg.content() {
                    Some(content) if re.is_match(content) => {}
                    _ => return false,
                },
                Err(_) => return false,
            }
        }

        // ── mention filter ────────────────────────────────────────────────────
        if let Some(ref user) = self.mention_user {
            if !msg.mentions().contains(user) {
                return false;
            }
        }

        // ── target_id: exact (room, seq) match ───────────────────────────────
        if let Some((ref target_room, target_seq)) = self.target_id {
            if room_id != target_room {
                return false;
            }
            match msg.seq() {
                Some(seq) if seq == target_seq => {}
                _ => return false,
            }
            // When target_id is set, skip the range seq filters below.
            return true;
        }

        // ── seq range filter ──────────────────────────────────────────────────
        // Constraints only apply when the message's room matches the filter room.
        if let Some((ref filter_room, filter_seq)) = self.after_seq {
            if room_id == filter_room {
                match msg.seq() {
                    Some(seq) if seq > filter_seq => {}
                    _ => return false,
                }
            }
        }

        if let Some((ref filter_room, filter_seq)) = self.before_seq {
            if room_id == filter_room {
                match msg.seq() {
                    Some(seq) if seq < filter_seq => {}
                    _ => return false,
                }
            }
        }

        // ── timestamp range filter ────────────────────────────────────────────
        if let Some(after) = self.after_ts {
            if msg.ts() <= &after {
                return false;
            }
        }

        if let Some(before) = self.before_ts {
            if msg.ts() >= &before {
                return false;
            }
        }

        true
    }
}

/// Returns `true` if `filter` contains at least one narrowing criterion.
///
/// Used to validate that the `-p/--public` flag is not used alone. The
/// narrowing criteria are: rooms, users, content_search, content_regex,
/// after_seq, before_seq, after_ts, before_ts, mention_user, target_id,
/// or a `limit`.
pub fn has_narrowing_filter(filter: &QueryFilter) -> bool {
    !filter.rooms.is_empty()
        || !filter.users.is_empty()
        || filter.content_search.is_some()
        || filter.content_regex.is_some()
        || filter.after_seq.is_some()
        || filter.before_seq.is_some()
        || filter.after_ts.is_some()
        || filter.before_ts.is_some()
        || filter.mention_user.is_some()
        || filter.target_id.is_some()
        || filter.limit.is_some()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use room_protocol::{make_dm, make_join, make_message};

    fn ts(year: i32, month: u32, day: u32, h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, h, m, s).unwrap()
    }

    fn msg_with_seq(room: &str, user: &str, content: &str, seq: u64) -> Message {
        let mut m = make_message(room, user, content);
        m.set_seq(seq);
        m
    }

    fn msg_with_ts(room: &str, user: &str, content: &str, t: DateTime<Utc>) -> Message {
        match make_message(room, user, content) {
            Message::Message {
                id,
                room,
                user,
                content,
                seq,
                ..
            } => Message::Message {
                id,
                room,
                user,
                ts: t,
                content,
                seq,
            },
            other => other,
        }
    }

    // ── default filter passes everything ─────────────────────────────────────

    #[test]
    fn default_filter_passes_message() {
        let f = QueryFilter::default();
        let msg = make_message("r", "alice", "hello");
        assert!(f.matches(&msg, "r"));
    }

    #[test]
    fn default_filter_passes_join() {
        let f = QueryFilter::default();
        let msg = make_join("r", "alice");
        assert!(f.matches(&msg, "r"));
    }

    #[test]
    fn default_filter_passes_dm() {
        let f = QueryFilter::default();
        let msg = make_dm("r", "alice", "bob", "secret");
        assert!(f.matches(&msg, "r"));
    }

    // ── rooms filter ──────────────────────────────────────────────────────────

    #[test]
    fn rooms_filter_passes_matching_room() {
        let f = QueryFilter {
            rooms: vec!["dev".into()],
            ..Default::default()
        };
        let msg = make_message("dev", "alice", "hi");
        assert!(f.matches(&msg, "dev"));
    }

    #[test]
    fn rooms_filter_rejects_other_room() {
        let f = QueryFilter {
            rooms: vec!["dev".into()],
            ..Default::default()
        };
        let msg = make_message("prod", "alice", "hi");
        assert!(!f.matches(&msg, "prod"));
    }

    #[test]
    fn rooms_filter_multiple_rooms_passes_any() {
        let f = QueryFilter {
            rooms: vec!["dev".into(), "staging".into()],
            ..Default::default()
        };
        assert!(f.matches(&make_message("dev", "u", "x"), "dev"));
        assert!(f.matches(&make_message("staging", "u", "x"), "staging"));
        assert!(!f.matches(&make_message("prod", "u", "x"), "prod"));
    }

    #[test]
    fn rooms_filter_empty_passes_all() {
        let f = QueryFilter::default();
        assert!(f.matches(&make_message("anywhere", "u", "x"), "anywhere"));
    }

    // ── users filter ──────────────────────────────────────────────────────────

    #[test]
    fn users_filter_passes_matching_user() {
        let f = QueryFilter {
            users: vec!["alice".into()],
            ..Default::default()
        };
        assert!(f.matches(&make_message("r", "alice", "hi"), "r"));
    }

    #[test]
    fn users_filter_rejects_other_user() {
        let f = QueryFilter {
            users: vec!["alice".into()],
            ..Default::default()
        };
        assert!(!f.matches(&make_message("r", "bob", "hi"), "r"));
    }

    #[test]
    fn users_filter_multiple_users() {
        let f = QueryFilter {
            users: vec!["alice".into(), "carol".into()],
            ..Default::default()
        };
        assert!(f.matches(&make_message("r", "alice", "x"), "r"));
        assert!(f.matches(&make_message("r", "carol", "x"), "r"));
        assert!(!f.matches(&make_message("r", "bob", "x"), "r"));
    }

    // ── public_only filter ────────────────────────────────────────────────────

    #[test]
    fn public_only_excludes_dm() {
        let f = QueryFilter {
            public_only: true,
            ..Default::default()
        };
        let msg = make_dm("r", "alice", "bob", "secret");
        assert!(!f.matches(&msg, "r"));
    }

    #[test]
    fn public_only_passes_regular_message() {
        let f = QueryFilter {
            public_only: true,
            ..Default::default()
        };
        assert!(f.matches(&make_message("r", "alice", "hi"), "r"));
    }

    #[test]
    fn public_only_false_passes_dm() {
        let f = QueryFilter {
            public_only: false,
            ..Default::default()
        };
        let msg = make_dm("r", "alice", "bob", "secret");
        assert!(f.matches(&msg, "r"));
    }

    // ── content_search filter ─────────────────────────────────────────────────

    #[test]
    fn content_search_passes_when_contained() {
        let f = QueryFilter {
            content_search: Some("hello".into()),
            ..Default::default()
        };
        assert!(f.matches(&make_message("r", "u", "say hello there"), "r"));
    }

    #[test]
    fn content_search_rejects_when_absent() {
        let f = QueryFilter {
            content_search: Some("hello".into()),
            ..Default::default()
        };
        assert!(!f.matches(&make_message("r", "u", "goodbye"), "r"));
    }

    #[test]
    fn content_search_rejects_join_no_content() {
        let f = QueryFilter {
            content_search: Some("hello".into()),
            ..Default::default()
        };
        assert!(!f.matches(&make_join("r", "alice"), "r"));
    }

    #[test]
    fn content_search_is_case_sensitive() {
        let f = QueryFilter {
            content_search: Some("Hello".into()),
            ..Default::default()
        };
        assert!(!f.matches(&make_message("r", "u", "hello"), "r"));
        assert!(f.matches(&make_message("r", "u", "say Hello world"), "r"));
    }

    // ── content_regex filter ──────────────────────────────────────────────────

    #[test]
    fn content_regex_passes_matching_pattern() {
        let f = QueryFilter {
            content_regex: Some(r"\d+".into()),
            ..Default::default()
        };
        assert!(f.matches(&make_message("r", "u", "issue #42 fixed"), "r"));
    }

    #[test]
    fn content_regex_rejects_non_matching() {
        let f = QueryFilter {
            content_regex: Some(r"^\d+$".into()),
            ..Default::default()
        };
        assert!(!f.matches(&make_message("r", "u", "no numbers here"), "r"));
    }

    #[test]
    fn content_regex_invalid_pattern_excludes_message() {
        let f = QueryFilter {
            content_regex: Some("[invalid".into()),
            ..Default::default()
        };
        assert!(!f.matches(&make_message("r", "u", "anything"), "r"));
    }

    #[test]
    fn content_regex_rejects_no_content() {
        let f = QueryFilter {
            content_regex: Some(".*".into()),
            ..Default::default()
        };
        // Join has no content — should be excluded.
        assert!(!f.matches(&make_join("r", "alice"), "r"));
    }

    // ── mention_user filter ────────────────────────────────────────────────────

    #[test]
    fn mention_user_passes_when_mentioned() {
        let f = QueryFilter {
            mention_user: Some("bob".into()),
            ..Default::default()
        };
        assert!(f.matches(&make_message("r", "alice", "hey @bob"), "r"));
    }

    #[test]
    fn mention_user_rejects_when_not_mentioned() {
        let f = QueryFilter {
            mention_user: Some("bob".into()),
            ..Default::default()
        };
        assert!(!f.matches(&make_message("r", "alice", "hey @carol"), "r"));
    }

    #[test]
    fn mention_user_rejects_no_content() {
        let f = QueryFilter {
            mention_user: Some("bob".into()),
            ..Default::default()
        };
        assert!(!f.matches(&make_join("r", "alice"), "r"));
    }

    // ── after_seq filter ──────────────────────────────────────────────────────

    #[test]
    fn after_seq_passes_strictly_greater() {
        let f = QueryFilter {
            after_seq: Some(("r".into(), 10)),
            ..Default::default()
        };
        assert!(f.matches(&msg_with_seq("r", "u", "x", 11), "r"));
    }

    #[test]
    fn after_seq_rejects_equal() {
        let f = QueryFilter {
            after_seq: Some(("r".into(), 10)),
            ..Default::default()
        };
        assert!(!f.matches(&msg_with_seq("r", "u", "x", 10), "r"));
    }

    #[test]
    fn after_seq_rejects_lesser() {
        let f = QueryFilter {
            after_seq: Some(("r".into(), 10)),
            ..Default::default()
        };
        assert!(!f.matches(&msg_with_seq("r", "u", "x", 5), "r"));
    }

    #[test]
    fn after_seq_skips_constraint_for_different_room() {
        // Filter room is "dev", message is in "prod" — constraint does not apply.
        let f = QueryFilter {
            after_seq: Some(("dev".into(), 10)),
            ..Default::default()
        };
        assert!(f.matches(&msg_with_seq("prod", "u", "x", 1), "prod"));
    }

    #[test]
    fn after_seq_rejects_msg_with_no_seq() {
        let f = QueryFilter {
            after_seq: Some(("r".into(), 0)),
            ..Default::default()
        };
        // Message with no seq (None) fails the constraint.
        let msg = make_message("r", "u", "x");
        assert!(!f.matches(&msg, "r"));
    }

    // ── before_seq filter ─────────────────────────────────────────────────────

    #[test]
    fn before_seq_passes_strictly_lesser() {
        let f = QueryFilter {
            before_seq: Some(("r".into(), 10)),
            ..Default::default()
        };
        assert!(f.matches(&msg_with_seq("r", "u", "x", 9), "r"));
    }

    #[test]
    fn before_seq_rejects_equal() {
        let f = QueryFilter {
            before_seq: Some(("r".into(), 10)),
            ..Default::default()
        };
        assert!(!f.matches(&msg_with_seq("r", "u", "x", 10), "r"));
    }

    #[test]
    fn before_seq_skips_for_different_room() {
        let f = QueryFilter {
            before_seq: Some(("dev".into(), 5)),
            ..Default::default()
        };
        assert!(f.matches(&msg_with_seq("prod", "u", "x", 100), "prod"));
    }

    // ── after_ts / before_ts filters ─────────────────────────────────────────

    #[test]
    fn after_ts_passes_strictly_after() {
        let cutoff = ts(2026, 3, 1, 12, 0, 0);
        let f = QueryFilter {
            after_ts: Some(cutoff),
            ..Default::default()
        };
        let msg = msg_with_ts("r", "u", "x", ts(2026, 3, 1, 13, 0, 0));
        assert!(f.matches(&msg, "r"));
    }

    #[test]
    fn after_ts_rejects_equal() {
        let cutoff = ts(2026, 3, 1, 12, 0, 0);
        let f = QueryFilter {
            after_ts: Some(cutoff),
            ..Default::default()
        };
        let msg = msg_with_ts("r", "u", "x", cutoff);
        assert!(!f.matches(&msg, "r"));
    }

    #[test]
    fn after_ts_rejects_before() {
        let cutoff = ts(2026, 3, 1, 12, 0, 0);
        let f = QueryFilter {
            after_ts: Some(cutoff),
            ..Default::default()
        };
        let msg = msg_with_ts("r", "u", "x", ts(2026, 3, 1, 11, 0, 0));
        assert!(!f.matches(&msg, "r"));
    }

    #[test]
    fn before_ts_passes_strictly_before() {
        let cutoff = ts(2026, 3, 1, 12, 0, 0);
        let f = QueryFilter {
            before_ts: Some(cutoff),
            ..Default::default()
        };
        let msg = msg_with_ts("r", "u", "x", ts(2026, 3, 1, 11, 0, 0));
        assert!(f.matches(&msg, "r"));
    }

    #[test]
    fn before_ts_rejects_equal() {
        let cutoff = ts(2026, 3, 1, 12, 0, 0);
        let f = QueryFilter {
            before_ts: Some(cutoff),
            ..Default::default()
        };
        let msg = msg_with_ts("r", "u", "x", cutoff);
        assert!(!f.matches(&msg, "r"));
    }

    // ── target_id filter ──────────────────────────────────────────────────────

    #[test]
    fn target_id_passes_exact_match() {
        let f = QueryFilter {
            target_id: Some(("r".into(), 7)),
            ..Default::default()
        };
        assert!(f.matches(&msg_with_seq("r", "u", "x", 7), "r"));
    }

    #[test]
    fn target_id_rejects_wrong_seq() {
        let f = QueryFilter {
            target_id: Some(("r".into(), 7)),
            ..Default::default()
        };
        assert!(!f.matches(&msg_with_seq("r", "u", "x", 8), "r"));
        assert!(!f.matches(&msg_with_seq("r", "u", "x", 6), "r"));
    }

    #[test]
    fn target_id_rejects_wrong_room() {
        let f = QueryFilter {
            target_id: Some(("dev".into(), 7)),
            ..Default::default()
        };
        assert!(!f.matches(&msg_with_seq("prod", "u", "x", 7), "prod"));
    }

    #[test]
    fn target_id_rejects_no_seq() {
        let f = QueryFilter {
            target_id: Some(("r".into(), 1)),
            ..Default::default()
        };
        let msg = make_message("r", "u", "no seq");
        assert!(!f.matches(&msg, "r"));
    }

    #[test]
    fn target_id_short_circuits_other_seq_filters() {
        // after_seq would reject seq=7, but target_id=7 should still pass.
        let f = QueryFilter {
            target_id: Some(("r".into(), 7)),
            after_seq: Some(("r".into(), 10)),
            ..Default::default()
        };
        assert!(f.matches(&msg_with_seq("r", "u", "x", 7), "r"));
    }

    // ── has_narrowing_filter ───────────────────────────────────────────────────

    #[test]
    fn has_narrowing_filter_empty_is_false() {
        assert!(!has_narrowing_filter(&QueryFilter::default()));
    }

    #[test]
    fn has_narrowing_filter_rooms_is_true() {
        let f = QueryFilter {
            rooms: vec!["r".into()],
            ..Default::default()
        };
        assert!(has_narrowing_filter(&f));
    }

    #[test]
    fn has_narrowing_filter_limit_is_true() {
        let f = QueryFilter {
            limit: Some(10),
            ..Default::default()
        };
        assert!(has_narrowing_filter(&f));
    }

    #[test]
    fn has_narrowing_filter_target_id_is_true() {
        let f = QueryFilter {
            target_id: Some(("r".into(), 1)),
            ..Default::default()
        };
        assert!(has_narrowing_filter(&f));
    }

    #[test]
    fn has_narrowing_filter_content_search_is_true() {
        let f = QueryFilter {
            content_search: Some("foo".into()),
            ..Default::default()
        };
        assert!(has_narrowing_filter(&f));
    }

    #[test]
    fn has_narrowing_filter_public_only_alone_is_false() {
        // public_only by itself is not a narrowing filter.
        let f = QueryFilter {
            public_only: true,
            ..Default::default()
        };
        assert!(!has_narrowing_filter(&f));
    }

    // ── combined filters ──────────────────────────────────────────────────────

    #[test]
    fn combined_room_and_user_filter() {
        let f = QueryFilter {
            rooms: vec!["dev".into()],
            users: vec!["alice".into()],
            ..Default::default()
        };
        assert!(f.matches(&make_message("dev", "alice", "x"), "dev"));
        // Wrong room.
        assert!(!f.matches(&make_message("prod", "alice", "x"), "prod"));
        // Wrong user.
        assert!(!f.matches(&make_message("dev", "bob", "x"), "dev"));
    }

    #[test]
    fn combined_content_and_mention() {
        let f = QueryFilter {
            content_search: Some("ticket".into()),
            mention_user: Some("bob".into()),
            ..Default::default()
        };
        // Both match.
        assert!(f.matches(&make_message("r", "u", "ticket #1 assigned @bob"), "r"));
        // Only content matches.
        assert!(!f.matches(&make_message("r", "u", "ticket #1"), "r"));
        // Only mention matches.
        assert!(!f.matches(&make_message("r", "u", "hey @bob"), "r"));
    }

    #[test]
    fn combined_seq_range() {
        let f = QueryFilter {
            after_seq: Some(("r".into(), 5)),
            before_seq: Some(("r".into(), 10)),
            ..Default::default()
        };
        assert!(f.matches(&msg_with_seq("r", "u", "x", 7), "r"));
        assert!(!f.matches(&msg_with_seq("r", "u", "x", 5), "r"));
        assert!(!f.matches(&msg_with_seq("r", "u", "x", 10), "r"));
    }
}
