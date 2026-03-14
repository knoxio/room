use room_protocol::{make_system, EventFilter, SubscriptionTier};

use crate::broker::{
    auth::check_join_permission,
    fanout::broadcast_and_persist,
    persistence::{persist_event_filters, persist_subscriptions},
    state::RoomState,
};

use super::CommandResult;

/// Handle `/who` — list online users with their statuses.
pub(super) async fn handle_who(state: &RoomState) -> anyhow::Result<CommandResult> {
    let entries: Vec<String> = state
        .status_entries()
        .await
        .into_iter()
        .map(|(u, s)| {
            if s.is_empty() {
                u
            } else {
                // Sanitize commas in status text so the TUI parser
                // (which splits on ", ") doesn't treat status fragments
                // as separate usernames (#656).
                let safe = s.replace(", ", "; ");
                format!("{u}: {safe}")
            }
        })
        .collect();
    let content = if entries.is_empty() {
        "no users online".to_owned()
    } else {
        format!("online — {}", entries.join(", "))
    };
    let sys = make_system(&state.room_id, "broker", content);
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::Reply(json))
}

/// Handle `/who_all` — list all daemon-registered users (cross-room).
///
/// In daemon mode, queries the [`UserRegistry`] for every registered username.
/// In standalone mode, falls back to the same output as `/who`.
pub(super) async fn handle_who_all(state: &RoomState) -> anyhow::Result<CommandResult> {
    let usernames: Vec<String> = if let Some(registry) = state.auth.registry.get() {
        let guard = registry.lock().await;
        let mut names: Vec<String> = guard
            .list_users()
            .iter()
            .map(|u| u.username.clone())
            .collect();
        names.sort();
        names
    } else {
        // Standalone mode — fall back to current room users.
        state
            .status_entries()
            .await
            .into_iter()
            .map(|(u, _)| u)
            .collect()
    };
    let json_array = serde_json::to_string(&usernames)?;
    let content = format!("users_all: {json_array}");
    let sys = make_system(&state.room_id, "broker", content);
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::Reply(json))
}

/// Handle `/set_status [text]` — set or clear the user's status, broadcast the change.
pub(super) async fn handle_set_status(
    params: &[String],
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    let status = params.join(" ");
    state.set_status(username, status.clone()).await;
    let display = if status.is_empty() {
        format!("{username} cleared their status")
    } else {
        format!("{username} set status: {status}")
    };
    let sys = make_system(&state.room_id, "broker", display);
    let seq_msg =
        broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter).await?;
    let json = serde_json::to_string(&seq_msg)?;
    Ok(CommandResult::HandledWithReply(json))
}

/// Handle `/subscribe [tier]` — subscribe to room messages with a tier filter.
pub(super) async fn handle_subscribe(
    params: &[String],
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    // Reject subscriptions from users who cannot join this room.
    // Without this guard, non-participants could subscribe to DM rooms
    // and read regular messages through the poll path (#491).
    if let Err(reason) = check_join_permission(username, state.config.as_ref()) {
        let sys = make_system(&state.room_id, "broker", reason);
        let json = serde_json::to_string(&sys)?;
        return Ok(CommandResult::Reply(json));
    }
    let tier_str = params.first().map(String::as_str).unwrap_or("full");
    let tier: SubscriptionTier = match tier_str.parse() {
        Ok(t) => t,
        Err(e) => {
            let sys = make_system(&state.room_id, "broker", e);
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    };
    state.set_subscription(username, tier).await;
    persist_subscriptions(state).await;
    let display = format!("{username} subscribed to {} (tier: {tier})", state.room_id);
    let sys = make_system(&state.room_id, "broker", display);
    broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter).await?;
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::HandledWithReply(json))
}

/// Handle `/unsubscribe` — set subscription to Unsubscribed tier.
pub(super) async fn handle_unsubscribe(
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    state
        .set_subscription(username, SubscriptionTier::Unsubscribed)
        .await;
    persist_subscriptions(state).await;
    let display = format!("{username} unsubscribed from {}", state.room_id);
    let sys = make_system(&state.room_id, "broker", display);
    broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter).await?;
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::HandledWithReply(json))
}

/// Handle `/subscribe_events [filter]` — set event filter for the user.
pub(super) async fn handle_subscribe_events(
    params: &[String],
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    // Reject from users who cannot join this room (same guard as subscribe).
    if let Err(reason) = check_join_permission(username, state.config.as_ref()) {
        let sys = make_system(&state.room_id, "broker", reason);
        let json = serde_json::to_string(&sys)?;
        return Ok(CommandResult::Reply(json));
    }
    let filter_str = params.first().map(String::as_str).unwrap_or("all");
    let filter: EventFilter = match filter_str.parse() {
        Ok(f) => f,
        Err(e) => {
            let sys = make_system(&state.room_id, "broker", e);
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    };
    state.set_event_filter(username, filter.clone()).await;
    persist_event_filters(state).await;
    let display = format!(
        "{username} event filter set to {filter} in {}",
        state.room_id
    );
    let sys = make_system(&state.room_id, "broker", display);
    broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter).await?;
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::HandledWithReply(json))
}

/// Handle `/subscriptions` — list all subscription tiers and event filters.
pub(super) async fn handle_subscriptions(state: &RoomState) -> anyhow::Result<CommandResult> {
    let raw = state.subscription_entries().await;
    let ef_raw = state.event_filter_entries().await;
    let content = if raw.is_empty() && ef_raw.is_empty() {
        "no subscriptions".to_owned()
    } else {
        let mut parts = Vec::new();
        if !raw.is_empty() {
            let entries: Vec<String> = raw.into_iter().map(|(u, t)| format!("{u}: {t}")).collect();
            parts.push(format!("tiers — {}", entries.join(", ")));
        }
        if !ef_raw.is_empty() {
            let entries: Vec<String> = ef_raw
                .into_iter()
                .map(|(u, f)| format!("{u}: {f}"))
                .collect();
            parts.push(format!("event filters — {}", entries.join(", ")));
        }
        parts.join(" | ")
    };
    let sys = make_system(&state.room_id, "broker", content);
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::Reply(json))
}
