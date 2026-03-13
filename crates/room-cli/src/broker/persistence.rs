use std::{collections::HashMap, path::Path};

use room_protocol::{EventFilter, SubscriptionTier};

use super::state::RoomState;

// ── Subscription persistence ─────────────────────────────────────────────────

/// Write a subscription map to disk as JSON.
pub(crate) fn save_subscription_map(
    map: &HashMap<String, SubscriptionTier>,
    path: &Path,
) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(map).map_err(|e| format!("serialize subscriptions: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Load a subscription map from disk. Returns an empty map if the file does
/// not exist or contains invalid JSON.
pub(crate) fn load_subscription_map(path: &Path) -> HashMap<String, SubscriptionTier> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    serde_json::from_str(&contents).unwrap_or_else(|e| {
        eprintln!(
            "[broker] corrupt subscription file {}: {e} — starting empty",
            path.display()
        );
        HashMap::new()
    })
}

/// Persist the current subscription map to disk (fire-and-forget logging).
///
/// Called after every mutation to the in-memory map. Uses synchronous I/O
/// because the file is tiny (a few KB at most) and consistency matters more
/// than shaving microseconds.
pub(crate) async fn persist_subscriptions(state: &RoomState) {
    let snapshot = state.subscription_snapshot().await;
    if let Err(e) = save_subscription_map(&snapshot, &state.filters.subscription_map_path) {
        eprintln!("[broker] subscription persist failed: {e}");
    }
}

// ── Event filter persistence ─────────────────────────────────────────────────

/// Write an event filter map to disk as JSON.
pub(crate) fn save_event_filter_map(
    map: &HashMap<String, EventFilter>,
    path: &Path,
) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(map).map_err(|e| format!("serialize event filters: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Load an event filter map from disk. Returns an empty map if the file does
/// not exist or contains invalid JSON.
pub(crate) fn load_event_filter_map(path: &Path) -> HashMap<String, EventFilter> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    serde_json::from_str(&contents).unwrap_or_else(|e| {
        eprintln!(
            "[broker] corrupt event filter file {}: {e} — starting empty",
            path.display()
        );
        HashMap::new()
    })
}

/// Persist the current event filter map to disk (fire-and-forget logging).
///
/// Called after every mutation. No-ops if the event filter map has not been
/// attached to the [`RoomState`].
pub(crate) async fn persist_event_filters(state: &RoomState) {
    if let Some(path) = state.event_filter_path() {
        let snapshot = state.event_filter_snapshot().await;
        if let Err(e) = save_event_filter_map(&snapshot, path) {
            eprintln!("[broker] event filter persist failed: {e}");
        }
    }
}
