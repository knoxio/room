use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{atomic::AtomicU64, Arc},
};

use room_protocol::{EventFilter, RoomConfig, SubscriptionTier};
use tokio::sync::{broadcast, watch, Mutex};

use crate::{plugin::PluginRegistry, registry::UserRegistry};
use std::sync::OnceLock;

use super::service::CrossRoomResolver;

/// Maps client ID → (username, broadcast sender).
/// Username is set after the handshake completes.
pub(crate) type ClientMap = Arc<Mutex<HashMap<u64, (String, broadcast::Sender<String>)>>>;

/// Maps username → status string. Status is ephemeral; cleared on disconnect.
pub(crate) type StatusMap = Arc<Mutex<HashMap<String, String>>>;

/// The username of the first client to complete the handshake.
/// The host receives all DMs regardless of sender/recipient.
pub(crate) type HostUser = Arc<Mutex<Option<String>>>;

/// Maps token UUID → username. Populated by one-shot JOIN requests.
/// Cleared when the broker process exits; token files on disk survive restarts.
pub(crate) type TokenMap = Arc<Mutex<HashMap<String, String>>>;

/// Maps username → subscription tier for this room. Persisted as JSON at
/// `~/.room/state/<room_id>.subscriptions` on every mutation; loaded on
/// broker/daemon startup.
/// DM rooms auto-subscribe both participants at `Full` on creation.
pub(crate) type SubscriptionMap = Arc<Mutex<HashMap<String, SubscriptionTier>>>;

/// Maps username → event filter for this room. Persisted as JSON at
/// `~/.room/state/<room_id>.event_filters` on every mutation; loaded on
/// broker/daemon startup. Default for users with no entry is `EventFilter::All`.
pub(crate) type EventFilterMap = Arc<Mutex<HashMap<String, EventFilter>>>;

/// Token and identity state grouped together.
///
/// Contains the per-room token map, its persistence path, and the optional
/// daemon-level user registry. The registry uses `OnceLock` so it can be
/// set after construction without exceeding the clippy argument threshold.
pub(crate) struct AuthState {
    /// Maps token UUID → username. Populated by one-shot JOIN requests.
    pub(crate) token_map: TokenMap,
    /// Path to the persisted token-map file (e.g. `~/.room/state/<room_id>.tokens`).
    pub(crate) token_map_path: Arc<PathBuf>,
    /// Daemon-level user registry for cross-room identity. Unset in
    /// single-room mode.
    pub(crate) registry: OnceLock<Arc<Mutex<UserRegistry>>>,
}

/// Subscription and event-filter state grouped together.
///
/// Contains the per-room subscription map, its persistence path, and the
/// optional per-user event filter map. The event filter uses `OnceLock` so
/// it can be set after construction.
pub(crate) struct FilterState {
    /// Maps username → subscription tier for this room.
    pub(crate) subscription_map: SubscriptionMap,
    /// Path to the persisted subscription-map file (e.g. `~/.room/state/<room_id>.subscriptions`).
    pub(crate) subscription_map_path: Arc<PathBuf>,
    /// Per-user event type filter. Set via [`RoomState::set_event_filter_map`]
    /// after construction. If unset, all event types pass through.
    pub(crate) event_filter_state: OnceLock<(EventFilterMap, Arc<PathBuf>)>,
}

/// Shared broker state passed to every client handler.
pub(crate) struct RoomState {
    pub(crate) clients: ClientMap,
    pub(crate) status_map: StatusMap,
    pub(crate) host_user: HostUser,
    pub(crate) auth: AuthState,
    pub(crate) filters: FilterState,
    pub(crate) chat_path: Arc<PathBuf>,
    pub(crate) room_id: Arc<String>,
    /// Set to `true` by the `/exit` admin command to shut down the broker.
    pub(crate) shutdown: Arc<watch::Sender<bool>>,
    /// Monotonically-increasing sequence counter.
    pub(crate) seq_counter: Arc<AtomicU64>,
    /// Plugin registry for dispatching `/` commands to plugins.
    pub(crate) plugin_registry: Arc<PluginRegistry>,
    /// Room visibility and access control configuration.
    pub(crate) config: Option<RoomConfig>,
    /// Optional cross-room resolver for daemon mode.
    ///
    /// When set, `dispatch_plugin` can handle `--room <id>` flags by resolving
    /// the target room's state and building the `CommandContext` against it.
    /// Set via [`set_cross_room_resolver`] after construction.
    pub(crate) cross_room_resolver: OnceLock<Arc<dyn CrossRoomResolver>>,
}

impl RoomState {
    // ── Factory ───────────────────────────────────────────────────────────────

    /// Construct a fully wired `Arc<RoomState>` with default empty collections.
    ///
    /// Creates fresh empty maps for `clients`, `status_map`, `host_user`,
    /// a fresh `seq_counter`, and a new `watch::channel` for `shutdown`. Callers that
    /// need a `watch::Receiver` should call `state.shutdown.subscribe()` after construction.
    ///
    /// The built-in `/help` and `/stats` plugins are registered automatically.
    ///
    /// # Parameters
    ///
    /// - `token_map` — pre-populated token map; pass `Arc::new(Mutex::new(HashMap::new()))`
    ///   for a fresh map or supply an existing shared map (daemon mode uses one map per daemon).
    /// - `subscription_map` — pre-populated subscription map loaded from disk (or empty).
    /// - `config` — room visibility/ACL config; `None` for legacy configless rooms.
    ///
    /// To attach a daemon-level [`UserRegistry`] (needed for admin commands in
    /// daemon mode), call [`RoomState::set_registry`] on the returned `Arc`
    /// immediately after construction — before handing it to other tasks.
    pub(crate) fn new(
        room_id: String,
        chat_path: PathBuf,
        token_map_path: PathBuf,
        subscription_map_path: PathBuf,
        token_map: TokenMap,
        subscription_map: SubscriptionMap,
        config: Option<RoomConfig>,
    ) -> Result<Arc<Self>, String> {
        let plugins = crate::plugin::PluginRegistry::with_all_plugins(&chat_path, None)
            .map_err(|e| format!("plugin error: {e}"))?;

        let (shutdown_tx, _) = watch::channel(false);

        Ok(Arc::new(Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_map: Arc::new(Mutex::new(HashMap::new())),
            host_user: Arc::new(Mutex::new(None)),
            auth: AuthState {
                token_map,
                token_map_path: Arc::new(token_map_path),
                registry: OnceLock::new(),
            },
            filters: FilterState {
                subscription_map,
                subscription_map_path: Arc::new(subscription_map_path),
                event_filter_state: OnceLock::new(),
            },
            chat_path: Arc::new(chat_path),
            room_id: Arc::new(room_id),
            shutdown: Arc::new(shutdown_tx),
            seq_counter: Arc::new(AtomicU64::new(0)),
            plugin_registry: Arc::new(plugins),
            config,
            cross_room_resolver: OnceLock::new(),
        }))
    }

    // ── registry ──────────────────────────────────────────────────────────────

    /// Attach the daemon-level [`UserRegistry`] to this room.
    ///
    /// Must be called at most once, immediately after construction, before the
    /// `Arc<RoomState>` is shared with other tasks. Silently no-ops if called
    /// a second time (consistent with `OnceLock` semantics).
    pub(crate) fn set_registry(&self, registry: Arc<Mutex<UserRegistry>>) {
        let _ = self.auth.registry.set(registry);
    }

    // ── cross_room_resolver ──────────────────────────────────────────────────

    /// Attach a cross-room resolver so plugins can handle `--room <id>` flags.
    ///
    /// Must be called at most once, immediately after construction. Silently
    /// no-ops if called a second time (consistent with `OnceLock` semantics).
    pub(crate) fn set_cross_room_resolver(&self, resolver: Arc<dyn CrossRoomResolver>) {
        let _ = self.cross_room_resolver.set(resolver);
    }

    // ── event_filter_map ─────────────────────────────────────────────────────

    /// Attach the event filter map and its persistence path.
    ///
    /// Must be called at most once, immediately after construction. Silently
    /// no-ops if called a second time (consistent with `OnceLock` semantics).
    pub(crate) fn set_event_filter_map(&self, map: EventFilterMap, path: PathBuf) {
        let _ = self.filters.event_filter_state.set((map, Arc::new(path)));
    }

    /// Set a user's event filter.
    ///
    /// No-ops if the event filter map has not been attached via
    /// [`set_event_filter_map`].
    pub(crate) async fn set_event_filter(&self, user: &str, filter: EventFilter) {
        if let Some((map, _)) = self.filters.event_filter_state.get() {
            map.lock().await.insert(user.to_owned(), filter);
        }
    }

    /// Return all (username, filter) event filter pairs, sorted by username.
    pub(crate) async fn event_filter_entries(&self) -> Vec<(String, EventFilter)> {
        match self.filters.event_filter_state.get() {
            Some((map, _)) => {
                let mut entries: Vec<(String, EventFilter)> = map
                    .lock()
                    .await
                    .iter()
                    .map(|(u, f)| (u.clone(), f.clone()))
                    .collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                entries
            }
            None => Vec::new(),
        }
    }

    /// Return a cloned snapshot of the event filter map (for persistence).
    pub(crate) async fn event_filter_snapshot(&self) -> HashMap<String, EventFilter> {
        match self.filters.event_filter_state.get() {
            Some((map, _)) => map.lock().await.clone(),
            None => HashMap::new(),
        }
    }

    /// Return the persistence path for the event filter map, if attached.
    pub(crate) fn event_filter_path(&self) -> Option<&PathBuf> {
        self.filters
            .event_filter_state
            .get()
            .map(|(_, p): &(EventFilterMap, Arc<PathBuf>)| p.as_ref())
    }

    // ── status_map accessors ──────────────────────────────────────────────────

    /// Set (or clear) a user's status string.
    pub(crate) async fn set_status(&self, user: &str, status: String) {
        self.status_map.lock().await.insert(user.to_owned(), status);
    }

    /// Remove a user's status entry (e.g. on kick or disconnect).
    pub(crate) async fn remove_status(&self, user: &str) {
        self.status_map.lock().await.remove(user);
    }

    /// Return all (username, status) pairs, sorted by username.
    pub(crate) async fn status_entries(&self) -> Vec<(String, String)> {
        let mut entries: Vec<(String, String)> = self
            .status_map
            .lock()
            .await
            .iter()
            .map(|(u, s)| (u.clone(), s.clone()))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    /// Number of users currently in the status map (i.e. online).
    pub(crate) async fn status_count(&self) -> usize {
        self.status_map.lock().await.len()
    }

    // ── subscription_map accessors ────────────────────────────────────────────

    /// Set a user's subscription tier.
    pub(crate) async fn set_subscription(&self, user: &str, tier: SubscriptionTier) {
        self.filters
            .subscription_map
            .lock()
            .await
            .insert(user.to_owned(), tier);
    }

    /// Return all (username, tier) subscription pairs, sorted by username.
    pub(crate) async fn subscription_entries(&self) -> Vec<(String, SubscriptionTier)> {
        let mut entries: Vec<(String, SubscriptionTier)> = self
            .filters
            .subscription_map
            .lock()
            .await
            .iter()
            .map(|(u, t)| (u.clone(), *t))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    /// Return a cloned snapshot of the full subscription map (for persistence).
    pub(crate) async fn subscription_snapshot(&self) -> HashMap<String, SubscriptionTier> {
        self.filters.subscription_map.lock().await.clone()
    }

    /// Number of subscribed users.
    pub(crate) async fn subscription_count(&self) -> usize {
        self.filters.subscription_map.lock().await.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use room_protocol::SubscriptionTier;
    use std::{collections::HashMap, sync::Arc};
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    fn make_state(dir: &TempDir) -> Arc<RoomState> {
        let chat = dir.path().join("chat.ndjson");
        let token_map_path = dir.path().join("room.tokens");
        let sub_map_path = dir.path().join("room.subscriptions");
        RoomState::new(
            "test-room".to_owned(),
            chat,
            token_map_path,
            sub_map_path,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            None,
        )
        .unwrap()
    }

    // ── factory ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn new_creates_state_with_correct_room_id() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        assert_eq!(state.room_id.as_str(), "test-room");
    }

    #[tokio::test]
    async fn new_registers_builtin_plugins() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        assert!(
            state.plugin_registry.resolve("help").is_none(),
            "help is a builtin, not a plugin"
        );
        assert!(
            state.plugin_registry.resolve("stats").is_some(),
            "stats plugin should be registered"
        );
    }

    #[tokio::test]
    async fn new_starts_with_empty_collections() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        assert_eq!(state.status_count().await, 0);
        assert_eq!(state.subscription_count().await, 0);
    }

    #[tokio::test]
    async fn new_uses_provided_token_map() {
        let dir = TempDir::new().unwrap();
        let chat = dir.path().join("chat.ndjson");
        let token_map = Arc::new(Mutex::new({
            let mut m = HashMap::new();
            m.insert("tok-1".to_owned(), "alice".to_owned());
            m
        }));
        let state = RoomState::new(
            "r".to_owned(),
            chat,
            dir.path().join("r.tokens"),
            dir.path().join("r.subscriptions"),
            token_map,
            Arc::new(Mutex::new(HashMap::new())),
            None,
        )
        .unwrap();
        assert!(state.auth.token_map.lock().await.contains_key("tok-1"));
    }

    // ── status_map accessors ──────────────────────────────────────────────────

    #[tokio::test]
    async fn set_status_inserts_entry() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state.set_status("alice", "busy".to_owned()).await;
        assert_eq!(state.status_count().await, 1);
    }

    #[tokio::test]
    async fn set_status_empty_string_clears_display() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state.set_status("alice", "busy".to_owned()).await;
        state.set_status("alice", String::new()).await;
        // Entry still present but empty
        let entries = state.status_entries().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "");
    }

    #[tokio::test]
    async fn remove_status_deletes_entry() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state.set_status("alice", "busy".to_owned()).await;
        state.remove_status("alice").await;
        assert_eq!(state.status_count().await, 0);
    }

    #[tokio::test]
    async fn status_entries_returns_sorted() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state.set_status("carol", "c".to_owned()).await;
        state.set_status("alice", "a".to_owned()).await;
        state.set_status("bob", "b".to_owned()).await;
        let entries = state.status_entries().await;
        assert_eq!(entries[0].0, "alice");
        assert_eq!(entries[1].0, "bob");
        assert_eq!(entries[2].0, "carol");
    }

    // ── subscription_map accessors ────────────────────────────────────────────

    #[tokio::test]
    async fn set_subscription_and_count() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state
            .set_subscription("alice", SubscriptionTier::Full)
            .await;
        assert_eq!(state.subscription_count().await, 1);
    }

    #[tokio::test]
    async fn subscription_entries_sorted() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state
            .set_subscription("bob", SubscriptionTier::MentionsOnly)
            .await;
        state
            .set_subscription("alice", SubscriptionTier::Full)
            .await;
        let entries = state.subscription_entries().await;
        assert_eq!(entries[0].0, "alice");
        assert_eq!(entries[1].0, "bob");
    }

    #[tokio::test]
    async fn subscription_snapshot_clones_map() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state
            .set_subscription("alice", SubscriptionTier::Full)
            .await;
        let snap = state.subscription_snapshot().await;
        assert_eq!(snap.get("alice"), Some(&SubscriptionTier::Full));
        // Snapshot is a clone — mutations don't affect the original.
        drop(snap);
        assert_eq!(state.subscription_count().await, 1);
    }

    // ── event_filter_map accessors ──────────────────────────────────────────

    fn attach_event_filters(state: &RoomState, dir: &TempDir) {
        let path = dir.path().join("room.event_filters");
        state.set_event_filter_map(Arc::new(Mutex::new(HashMap::new())), path);
    }

    #[tokio::test]
    async fn event_filter_entries_empty_without_attach() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        // Without set_event_filter_map, entries should be empty
        assert!(state.event_filter_entries().await.is_empty());
    }

    #[tokio::test]
    async fn event_filter_noop_without_attach() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        // set_event_filter should silently no-op without attachment
        state.set_event_filter("alice", EventFilter::None).await;
        assert!(state.event_filter_entries().await.is_empty());
    }

    #[tokio::test]
    async fn set_event_filter_inserts_entry() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        attach_event_filters(&state, &dir);
        state.set_event_filter("alice", EventFilter::None).await;
        let entries = state.event_filter_entries().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "alice");
        assert_eq!(entries[0].1, EventFilter::None);
    }

    #[tokio::test]
    async fn set_event_filter_overwrites() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        attach_event_filters(&state, &dir);
        state.set_event_filter("alice", EventFilter::None).await;
        state.set_event_filter("alice", EventFilter::All).await;
        let entries = state.event_filter_entries().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, EventFilter::All);
    }

    #[tokio::test]
    async fn event_filter_entries_sorted() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        attach_event_filters(&state, &dir);
        state.set_event_filter("carol", EventFilter::None).await;
        state.set_event_filter("alice", EventFilter::All).await;
        let entries = state.event_filter_entries().await;
        assert_eq!(entries[0].0, "alice");
        assert_eq!(entries[1].0, "carol");
    }

    #[tokio::test]
    async fn event_filter_snapshot_clones_map() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        attach_event_filters(&state, &dir);
        state.set_event_filter("alice", EventFilter::None).await;
        let snap: HashMap<String, EventFilter> = state.event_filter_snapshot().await;
        assert_eq!(snap.get("alice"), Some(&EventFilter::None));
        drop(snap);
        let entries: Vec<(String, EventFilter)> = state.event_filter_entries().await;
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn event_filter_path_none_without_attach() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        assert!(state.event_filter_path().is_none());
    }

    #[tokio::test]
    async fn event_filter_path_some_after_attach() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        let path = dir.path().join("room.event_filters");
        state.set_event_filter_map(Arc::new(Mutex::new(HashMap::new())), path.clone());
        assert_eq!(state.event_filter_path(), Some(&path));
    }
}
