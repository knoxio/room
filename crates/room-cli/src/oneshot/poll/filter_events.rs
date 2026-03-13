use room_protocol::EventFilter;

use crate::{broker::persistence::load_event_filter_map, message::Message, paths};

// ── Event filter lookup ──────────────────────────────────────────────────────

/// Look up a user's event filter for a room from the persisted event filter
/// map on disk.
///
/// Returns `EventFilter::All` when the file is missing, corrupt, or the user
/// has no entry — all events pass by default.
pub(in crate::oneshot) fn load_user_event_filter(room_id: &str, username: &str) -> EventFilter {
    let state_dir = paths::room_state_dir();
    let ef_path = paths::broker_event_filters_path(&state_dir, room_id);
    let map = load_event_filter_map(&ef_path);
    map.get(username).cloned().unwrap_or(EventFilter::All)
}

/// Apply event-type filtering to a message list in place.
///
/// Only [`Message::Event`] messages are affected — all other message types
/// pass through unfiltered. For Event messages, the `event_type` field is
/// checked against the filter.
pub(in crate::oneshot) fn apply_event_filter(messages: &mut Vec<Message>, filter: &EventFilter) {
    if matches!(filter, EventFilter::All) {
        return;
    }
    messages.retain(|m| match m {
        Message::Event { event_type, .. } => filter.allows(event_type),
        _ => true,
    });
}

/// Apply per-room event-filter filtering to a message list in place.
///
/// Similar to [`super::filter_tier::apply_per_room_tier_filter`] but for event types.
pub(in crate::oneshot) fn apply_per_room_event_filter(
    messages: &mut Vec<Message>,
    room_ids: &[String],
    username: &str,
) {
    use std::collections::HashMap;
    let filters: HashMap<&str, EventFilter> = room_ids
        .iter()
        .map(|r| (r.as_str(), load_user_event_filter(r, username)))
        .collect();

    messages.retain(|m| match m {
        Message::Event {
            room, event_type, ..
        } => {
            let filter = filters.get(room.as_str()).unwrap_or(&EventFilter::All);
            filter.allows(event_type)
        }
        _ => true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::make_message;
    use room_protocol::EventFilter;

    fn make_event_msg(room: &str, event_type: room_protocol::EventType) -> Message {
        room_protocol::make_event(room, "bot", event_type, "event content", None)
    }

    #[test]
    fn event_filter_all_keeps_everything() {
        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_event_msg("r", room_protocol::EventType::TaskPosted),
            make_event_msg("r", room_protocol::EventType::TaskFinished),
        ];
        apply_event_filter(&mut msgs, &EventFilter::All);
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn event_filter_none_removes_only_events() {
        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_event_msg("r", room_protocol::EventType::TaskPosted),
            make_event_msg("r", room_protocol::EventType::TaskFinished),
        ];
        apply_event_filter(&mut msgs, &EventFilter::None);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content().unwrap().contains("hello"));
    }

    #[test]
    fn event_filter_only_keeps_matching_events() {
        let mut types = std::collections::BTreeSet::new();
        types.insert(room_protocol::EventType::TaskPosted);
        let filter = EventFilter::Only { types };

        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_event_msg("r", room_protocol::EventType::TaskPosted),
            make_event_msg("r", room_protocol::EventType::TaskFinished),
        ];
        apply_event_filter(&mut msgs, &filter);
        assert_eq!(msgs.len(), 2); // hello + task_posted
    }

    #[test]
    fn event_filter_only_removes_non_matching_events() {
        let mut types = std::collections::BTreeSet::new();
        types.insert(room_protocol::EventType::TaskFinished);
        let filter = EventFilter::Only { types };

        let mut msgs = vec![
            make_event_msg("r", room_protocol::EventType::TaskPosted),
            make_event_msg("r", room_protocol::EventType::TaskAssigned),
            make_event_msg("r", room_protocol::EventType::TaskFinished),
        ];
        apply_event_filter(&mut msgs, &filter);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn event_filter_does_not_affect_non_event_messages() {
        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_message("r", "bob", "world"),
        ];
        apply_event_filter(&mut msgs, &EventFilter::None);
        assert_eq!(msgs.len(), 2, "non-event messages should not be filtered");
    }

    #[test]
    fn load_user_event_filter_missing_file_returns_all() {
        let ef = load_user_event_filter("nonexistent-room-ef-test", "alice");
        assert_eq!(ef, EventFilter::All);
    }

    #[test]
    fn load_user_event_filter_returns_persisted() {
        let state_dir = crate::paths::room_state_dir();
        let _ = std::fs::create_dir_all(&state_dir);
        let room_id = format!("test-ef-load-{}", std::process::id());
        let ef_path = crate::paths::broker_event_filters_path(&state_dir, &room_id);

        let mut map = std::collections::HashMap::new();
        map.insert("alice".to_string(), EventFilter::None);
        let mut types = std::collections::BTreeSet::new();
        types.insert(room_protocol::EventType::TaskPosted);
        map.insert("bob".to_string(), EventFilter::Only { types });
        let json = serde_json::to_string_pretty(&map).unwrap();
        std::fs::write(&ef_path, json).unwrap();

        assert_eq!(load_user_event_filter(&room_id, "alice"), EventFilter::None);
        let bob_filter = load_user_event_filter(&room_id, "bob");
        match bob_filter {
            EventFilter::Only { types } => {
                assert!(types.contains(&room_protocol::EventType::TaskPosted));
                assert_eq!(types.len(), 1);
            }
            _ => panic!("expected Only filter for bob"),
        }
        // Unknown user defaults to All.
        assert_eq!(load_user_event_filter(&room_id, "carol"), EventFilter::All);

        let _ = std::fs::remove_file(&ef_path);
    }
}
