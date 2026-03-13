use room_protocol::SubscriptionTier;

use crate::{broker::persistence::load_subscription_map, message::Message, paths};

// ── Subscription tier lookup ──────────────────────────────────────────────────

/// Look up a user's subscription tier for a room from the persisted
/// subscription map on disk.
///
/// Returns `Full` when the subscription file is missing, corrupt, or the user
/// has no entry — unsubscribed users must have been explicitly recorded.
pub(in crate::oneshot) fn load_user_tier(room_id: &str, username: &str) -> SubscriptionTier {
    let state_dir = paths::room_state_dir();
    let sub_path = paths::broker_subscriptions_path(&state_dir, room_id);
    let map = load_subscription_map(&sub_path);
    map.get(username).copied().unwrap_or(SubscriptionTier::Full)
}

/// Apply subscription-tier filtering to a message list in place.
///
/// - `Full` — no filtering (all messages pass).
/// - `MentionsOnly` — keep only messages that @mention `username`.
/// - `Unsubscribed` — remove all messages.
pub(in crate::oneshot) fn apply_tier_filter(
    messages: &mut Vec<Message>,
    tier: SubscriptionTier,
    username: &str,
) {
    match tier {
        SubscriptionTier::Full => {}
        SubscriptionTier::MentionsOnly => {
            messages.retain(|m| m.mentions().iter().any(|mention| mention == username));
        }
        SubscriptionTier::Unsubscribed => {
            messages.clear();
        }
    }
}

/// Apply per-room subscription-tier filtering to a message list in place.
///
/// Looks up the user's tier for each message's room and filters accordingly.
/// Each room is checked independently — a user can be `Full` in one room and
/// `Unsubscribed` in another.
pub(in crate::oneshot) fn apply_per_room_tier_filter(
    messages: &mut Vec<Message>,
    room_ids: &[String],
    username: &str,
) {
    use std::collections::HashMap;
    let tiers: HashMap<&str, SubscriptionTier> = room_ids
        .iter()
        .map(|r| (r.as_str(), load_user_tier(r, username)))
        .collect();

    messages.retain(|m| {
        let tier = tiers
            .get(m.room())
            .copied()
            .unwrap_or(SubscriptionTier::Full);
        match tier {
            SubscriptionTier::Full => true,
            SubscriptionTier::MentionsOnly => {
                m.mentions().iter().any(|mention| mention == username)
            }
            SubscriptionTier::Unsubscribed => false,
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::make_message;
    use room_protocol::SubscriptionTier;

    /// load_user_tier returns Full when no subscription file exists.
    #[test]
    fn load_user_tier_missing_file_returns_full() {
        // Use a room ID that will never have a subscription file on disk.
        let tier = load_user_tier("nonexistent-room-tier-test", "alice");
        assert_eq!(tier, SubscriptionTier::Full);
    }

    /// load_user_tier returns the persisted tier when the file exists.
    #[test]
    fn load_user_tier_returns_persisted_tier() {
        let state_dir = crate::paths::room_state_dir();
        let _ = std::fs::create_dir_all(&state_dir);
        let room_id = format!("test-tier-load-{}", std::process::id());
        let sub_path = crate::paths::broker_subscriptions_path(&state_dir, &room_id);

        let mut map = std::collections::HashMap::new();
        map.insert("alice".to_string(), SubscriptionTier::MentionsOnly);
        map.insert("bob".to_string(), SubscriptionTier::Unsubscribed);
        let json = serde_json::to_string_pretty(&map).unwrap();
        std::fs::write(&sub_path, json).unwrap();

        assert_eq!(
            load_user_tier(&room_id, "alice"),
            SubscriptionTier::MentionsOnly
        );
        assert_eq!(
            load_user_tier(&room_id, "bob"),
            SubscriptionTier::Unsubscribed
        );
        // Unknown user defaults to Full.
        assert_eq!(load_user_tier(&room_id, "carol"), SubscriptionTier::Full);

        let _ = std::fs::remove_file(&sub_path);
    }

    /// apply_tier_filter with Full keeps all messages.
    #[test]
    fn apply_tier_filter_full_keeps_all() {
        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_message("r", "bob", "world"),
        ];
        apply_tier_filter(&mut msgs, SubscriptionTier::Full, "carol");
        assert_eq!(msgs.len(), 2);
    }

    /// apply_tier_filter with MentionsOnly keeps only @mentions.
    #[test]
    fn apply_tier_filter_mentions_only_filters() {
        let mut msgs = vec![
            make_message("r", "alice", "hey @carol check this"),
            make_message("r", "bob", "unrelated message"),
            make_message("r", "dave", "also @carol"),
        ];
        apply_tier_filter(&mut msgs, SubscriptionTier::MentionsOnly, "carol");
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content().unwrap().contains("@carol"));
        assert!(msgs[1].content().unwrap().contains("@carol"));
    }

    /// apply_tier_filter with Unsubscribed clears all messages.
    #[test]
    fn apply_tier_filter_unsubscribed_clears_all() {
        let mut msgs = vec![
            make_message("r", "alice", "hey @carol"),
            make_message("r", "bob", "world"),
        ];
        apply_tier_filter(&mut msgs, SubscriptionTier::Unsubscribed, "carol");
        assert!(msgs.is_empty());
    }

    /// apply_tier_filter with MentionsOnly and no mentions returns empty.
    #[test]
    fn apply_tier_filter_mentions_only_no_mentions_returns_empty() {
        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_message("r", "bob", "world"),
        ];
        apply_tier_filter(&mut msgs, SubscriptionTier::MentionsOnly, "carol");
        assert!(msgs.is_empty());
    }

    /// MentionsOnly tier sets mention_user filter, narrowing results to @mentions.
    #[test]
    fn mentions_only_tier_sets_mention_user_on_filter() {
        use crate::query::QueryFilter;
        // Verify the tier logic: when tier is MentionsOnly and mention_user is
        // not already set, it should be set to the username.
        let mut filter = QueryFilter::default();
        let tier = SubscriptionTier::MentionsOnly;

        // Simulate what cmd_query does.
        match tier {
            SubscriptionTier::MentionsOnly => {
                if filter.mention_user.is_none() {
                    filter.mention_user = Some("alice".to_string());
                }
            }
            _ => {}
        }

        assert_eq!(filter.mention_user, Some("alice".to_string()));

        // Now verify with apply_tier_filter that messages are correctly narrowed.
        let mut msgs = vec![
            make_message("r", "bob", "hey @alice look"),
            make_message("r", "bob", "unrelated chatter"),
        ];
        apply_tier_filter(&mut msgs, SubscriptionTier::MentionsOnly, "alice");
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content().unwrap().contains("@alice"));
    }

    /// MentionsOnly tier does not override an existing mention_user filter.
    #[test]
    fn mentions_only_tier_preserves_existing_mention_user() {
        use crate::query::QueryFilter;
        let mut filter = QueryFilter {
            mention_user: Some("bob".to_string()),
            ..Default::default()
        };

        // MentionsOnly should not overwrite the existing filter.
        match SubscriptionTier::MentionsOnly {
            SubscriptionTier::MentionsOnly => {
                if filter.mention_user.is_none() {
                    filter.mention_user = Some("alice".to_string());
                }
            }
            _ => {}
        }

        assert_eq!(
            filter.mention_user,
            Some("bob".to_string()),
            "existing mention_user filter should be preserved"
        );
    }

    // ── per-room subscription tier filtering tests ─────────────────────────

    #[test]
    fn per_room_tier_filter_full_keeps_all() {
        let mut msgs = vec![
            make_message("dev", "alice", "hello from dev"),
            make_message("lobby", "bob", "hello from lobby"),
        ];
        // No subscription files → defaults to Full for both rooms.
        let rooms = vec![
            "nonexistent-perroom-full-1".to_string(),
            "nonexistent-perroom-full-2".to_string(),
        ];
        apply_per_room_tier_filter(&mut msgs, &rooms, "carol");
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn per_room_tier_filter_mixed_tiers() {
        let state_dir = crate::paths::room_state_dir();
        let _ = std::fs::create_dir_all(&state_dir);

        let room_full = format!("perroom-mixed-full-{}", std::process::id());
        let room_unsub = format!("perroom-mixed-unsub-{}", std::process::id());
        let room_mentions = format!("perroom-mixed-ment-{}", std::process::id());

        // Write subscription maps: room_full=Full (default), room_unsub=Unsubscribed, room_mentions=MentionsOnly
        let sub_unsub = crate::paths::broker_subscriptions_path(&state_dir, &room_unsub);
        let mut map_unsub = std::collections::HashMap::new();
        map_unsub.insert("alice".to_string(), SubscriptionTier::Unsubscribed);
        std::fs::write(&sub_unsub, serde_json::to_string(&map_unsub).unwrap()).unwrap();

        let sub_ment = crate::paths::broker_subscriptions_path(&state_dir, &room_mentions);
        let mut map_ment = std::collections::HashMap::new();
        map_ment.insert("alice".to_string(), SubscriptionTier::MentionsOnly);
        std::fs::write(&sub_ment, serde_json::to_string(&map_ment).unwrap()).unwrap();

        let mut msgs = vec![
            make_message(&room_full, "bob", "visible in full room"),
            make_message(&room_unsub, "bob", "invisible — unsubscribed"),
            make_message(&room_mentions, "bob", "no mention — filtered"),
            make_message(&room_mentions, "bob", "hey @alice check this"),
        ];

        let rooms = vec![room_full.clone(), room_unsub.clone(), room_mentions.clone()];
        apply_per_room_tier_filter(&mut msgs, &rooms, "alice");

        // Only the Full room message and the MentionsOnly room @alice message survive.
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content().unwrap().contains("visible in full room"));
        assert!(msgs[1].content().unwrap().contains("@alice"));

        // Cleanup
        let _ = std::fs::remove_file(&sub_unsub);
        let _ = std::fs::remove_file(&sub_ment);
    }

    #[test]
    fn per_room_tier_filter_unknown_room_defaults_to_full() {
        let mut msgs = vec![make_message("mystery", "bob", "hello")];
        // Room not in the room_ids list at all — tier defaults to Full.
        apply_per_room_tier_filter(&mut msgs, &["other".to_string()], "alice");
        assert_eq!(msgs.len(), 1);
    }
}
