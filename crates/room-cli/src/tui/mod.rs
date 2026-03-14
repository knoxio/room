use std::collections::HashMap;

use tokio::sync::mpsc;

use room_protocol::SubscriptionTier;

use crate::message::Message;
use input::{
    parse_kick_broadcast, parse_status_broadcast, parse_subscription_broadcast,
    seed_online_users_from_who,
};
use render::{assign_color, ColorMap};

mod colors;
mod display;
mod dm;
mod event_loop;
mod frame;
mod input;
mod markdown;
mod panel;
mod parse;
mod render;
mod render_bots;
mod widgets;

pub use event_loop::run;

/// Maximum visible content lines in the input box before it stops growing.
const MAX_INPUT_LINES: usize = 6;

/// Per-room state for the tabbed TUI. Each tab owns its message buffer,
/// online user list, status map, and inbound message channel.
struct RoomTab {
    room_id: String,
    messages: Vec<Message>,
    online_users: Vec<String>,
    user_statuses: HashMap<String, String>,
    subscription_tiers: HashMap<String, SubscriptionTier>,
    unread_count: usize,
    scroll_offset: usize,
    msg_rx: mpsc::UnboundedReceiver<Message>,
    write_half: tokio::net::unix::OwnedWriteHalf,
}

/// Result of draining messages from a tab's channel.
enum DrainResult {
    /// Channel still open, messages drained.
    Ok,
    /// Channel closed — broker disconnected.
    Disconnected,
}

impl RoomTab {
    /// Process a single inbound message, updating online_users, statuses, and
    /// the color map. Pushes the message into the buffer and increments unread
    /// if `is_active` is false.
    fn process_message(&mut self, msg: Message, color_map: &mut ColorMap, is_active: bool) {
        match &msg {
            Message::Join { user, .. } if !self.online_users.contains(user) => {
                assign_color(user, color_map);
                self.online_users.push(user.clone());
            }
            Message::Leave { user, .. } => {
                self.online_users.retain(|u| u != user);
                self.user_statuses.remove(user);
                self.subscription_tiers.remove(user);
            }
            Message::Message { user, .. } if !self.online_users.contains(user) => {
                assign_color(user, color_map);
                self.online_users.push(user.clone());
            }
            Message::Message { user, .. } => {
                assign_color(user, color_map);
            }
            Message::System { user, content, .. } if user == "broker" => {
                seed_online_users_from_who(
                    content,
                    &mut self.online_users,
                    &mut self.user_statuses,
                );
                if let Some((name, status)) = parse_status_broadcast(content) {
                    self.user_statuses.insert(name, status);
                }
                if let Some(kicked) = parse_kick_broadcast(content) {
                    self.online_users.retain(|u| u != kicked);
                    self.user_statuses.remove(kicked);
                    self.subscription_tiers.remove(kicked);
                }
                if let Some((name, tier)) = parse_subscription_broadcast(content) {
                    self.subscription_tiers.insert(name, tier);
                }
                for u in &self.online_users {
                    assign_color(u, color_map);
                }
            }
            _ => {}
        }
        if !is_active {
            self.unread_count += 1;
        }
        self.messages.push(msg);
    }

    /// Drain all pending messages from the channel into the message buffer.
    fn drain_messages(&mut self, color_map: &mut ColorMap, is_active: bool) -> DrainResult {
        loop {
            match self.msg_rx.try_recv() {
                Ok(msg) => self.process_message(msg, color_map, is_active),
                Err(mpsc::error::TryRecvError::Empty) => return DrainResult::Ok,
                Err(mpsc::error::TryRecvError::Disconnected) => return DrainResult::Disconnected,
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_msg(user: &str, content: &str) -> Message {
        Message::Message {
            id: "test-id".into(),
            room: "test-room".into(),
            user: user.into(),
            ts: Utc::now(),
            content: content.into(),
            seq: None,
        }
    }

    fn make_join(user: &str) -> Message {
        Message::Join {
            id: "test-id".into(),
            room: "test-room".into(),
            user: user.into(),
            ts: Utc::now(),
            seq: None,
        }
    }

    fn make_leave(user: &str) -> Message {
        Message::Leave {
            id: "test-id".into(),
            room: "test-room".into(),
            user: user.into(),
            ts: Utc::now(),
            seq: None,
        }
    }

    fn make_system(content: &str) -> Message {
        Message::System {
            id: "test-id".into(),
            room: "test-room".into(),
            user: "broker".into(),
            ts: Utc::now(),
            content: content.into(),
            seq: None,
            data: None,
        }
    }

    // ── RoomTab::process_message tests ────────────────────────────────────

    #[tokio::test]
    async fn process_message_adds_user_on_join() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_join("alice"), &mut cm, true);
        assert_eq!(tab.online_users, vec!["alice"]);
        assert_eq!(tab.messages.len(), 1);
    }

    #[tokio::test]
    async fn process_message_removes_user_on_leave() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: vec!["alice".into()],
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_leave("alice"), &mut cm, true);
        assert!(tab.online_users.is_empty());
    }

    #[tokio::test]
    async fn process_message_increments_unread_when_inactive() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_msg("bob", "hello"), &mut cm, false);
        assert_eq!(tab.unread_count, 1);

        tab.process_message(make_msg("bob", "world"), &mut cm, false);
        assert_eq!(tab.unread_count, 2);
    }

    #[tokio::test]
    async fn process_message_no_unread_when_active() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_msg("bob", "hello"), &mut cm, true);
        assert_eq!(tab.unread_count, 0);
    }

    #[tokio::test]
    async fn process_message_seeds_user_from_message_sender() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_msg("charlie", "hi"), &mut cm, true);
        assert_eq!(tab.online_users, vec!["charlie"]);
        assert!(cm.contains_key("charlie"));
    }

    #[tokio::test]
    async fn process_message_does_not_duplicate_existing_user() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: vec!["alice".into()],
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_msg("alice", "hi"), &mut cm, true);
        assert_eq!(tab.online_users.len(), 1);
    }

    // ── drain_messages tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn drain_messages_processes_pending() {
        let (tx, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tx.send(make_msg("bob", "one")).unwrap();
        tx.send(make_msg("bob", "two")).unwrap();

        let result = tab.drain_messages(&mut cm, true);
        assert!(matches!(result, DrainResult::Ok));
        assert_eq!(tab.messages.len(), 2);
    }

    #[tokio::test]
    async fn drain_messages_detects_disconnect() {
        let (tx, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        drop(tx);
        let result = tab.drain_messages(&mut cm, true);
        assert!(matches!(result, DrainResult::Disconnected));
    }

    #[tokio::test]
    async fn drain_messages_empty_returns_ok() {
        let (_tx, rx) = mpsc::unbounded_channel::<Message>();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        let result = tab.drain_messages(&mut cm, true);
        assert!(matches!(result, DrainResult::Ok));
        assert!(tab.messages.is_empty());
    }

    #[tokio::test]
    async fn process_system_message_parses_status() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: vec!["alice".into()],
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_system("alice set status: coding"), &mut cm, true);
        assert_eq!(tab.user_statuses.get("alice").unwrap(), "coding");
    }

    #[tokio::test]
    async fn process_subscription_broadcast_sets_tier() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: vec!["alice".into()],
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(
            make_system("alice subscribed to test (tier: mentions_only)"),
            &mut cm,
            true,
        );
        assert_eq!(
            tab.subscription_tiers.get("alice").copied(),
            Some(SubscriptionTier::MentionsOnly),
        );

        // Upgrading to Full clears non-Full indicator.
        tab.process_message(
            make_system("alice subscribed to test (tier: full)"),
            &mut cm,
            true,
        );
        assert_eq!(
            tab.subscription_tiers.get("alice").copied(),
            Some(SubscriptionTier::Full),
        );
    }

    #[tokio::test]
    async fn process_leave_clears_subscription_tier() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: vec!["alice".into()],
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::from([(
                "alice".to_owned(),
                SubscriptionTier::MentionsOnly,
            )]),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_leave("alice"), &mut cm, true);
        assert!(tab.subscription_tiers.get("alice").is_none());
    }

    // ── kick removes user from member panel (#505) ───────────────────────

    #[tokio::test]
    async fn process_kick_broadcast_removes_user() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: vec!["alice".into(), "bob".into()],
            user_statuses: HashMap::from([("bob".to_owned(), "working".to_owned())]),
            subscription_tiers: HashMap::from([("bob".to_owned(), SubscriptionTier::Full)]),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(
            make_system("alice kicked bob (token invalidated)"),
            &mut cm,
            true,
        );
        assert!(
            !tab.online_users.contains(&"bob".to_owned()),
            "kicked user must be removed from online_users"
        );
        assert!(
            tab.user_statuses.get("bob").is_none(),
            "kicked user's status must be cleared"
        );
        assert!(
            tab.subscription_tiers.get("bob").is_none(),
            "kicked user's subscription tier must be cleared"
        );
        // alice should still be online
        assert!(tab.online_users.contains(&"alice".to_owned()));
    }

    // ── #656 regression: status commas must not leak fake users ────────

    #[tokio::test]
    async fn process_message_who_with_comma_status_no_fake_users() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            subscription_tiers: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        // Simulate a /who response where alice's status contains a comma.
        // The broker should sanitize this, but even if it doesn't, the
        // parser must not treat "#636 filed" as a username.
        tab.process_message(
            make_system("online \u{2014} alice: PR #630 merged, #636 filed, bob"),
            &mut cm,
            true,
        );
        assert_eq!(
            tab.online_users.len(),
            2,
            "only alice and bob are real users"
        );
        assert!(tab.online_users.contains(&"alice".to_owned()));
        assert!(tab.online_users.contains(&"bob".to_owned()));
        assert!(
            !tab.online_users.contains(&"#636 filed".to_owned()),
            "status fragment must not appear as a user"
        );
    }
}
