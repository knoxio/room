use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{atomic::AtomicU64, Arc},
};

use room_protocol::{RoomConfig, SubscriptionTier};
use tokio::sync::{broadcast, watch, Mutex};

use crate::{plugin::PluginRegistry, registry::UserRegistry};
use std::sync::OnceLock;

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

/// Default claim TTL: 30 minutes. Claims expire after this duration unless renewed.
pub(crate) const CLAIM_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// A single task claim with creation timestamp for lease-based expiry.
#[derive(Debug, Clone)]
pub(crate) struct ClaimEntry {
    pub(crate) task: String,
    pub(crate) claimed_at: std::time::Instant,
}

/// Maps username → claim entry. Ephemeral; cleared on broker exit.
/// Users can hold at most one claim at a time (new claim replaces old).
/// Claims expire after [`CLAIM_TTL`] and are lazily swept on access.
pub(crate) type ClaimMap = Arc<Mutex<HashMap<String, ClaimEntry>>>;

/// Maps username → subscription tier for this room. Persisted as JSON at
/// `~/.room/state/<room_id>.subscriptions` on every mutation; loaded on
/// broker/daemon startup.
/// DM rooms auto-subscribe both participants at `Full` on creation.
pub(crate) type SubscriptionMap = Arc<Mutex<HashMap<String, SubscriptionTier>>>;

/// Shared broker state passed to every client handler.
pub(crate) struct RoomState {
    pub(crate) clients: ClientMap,
    pub(crate) status_map: StatusMap,
    pub(crate) host_user: HostUser,
    pub(crate) token_map: TokenMap,
    pub(crate) claim_map: ClaimMap,
    pub(crate) subscription_map: SubscriptionMap,
    pub(crate) chat_path: Arc<PathBuf>,
    /// Path to the persisted token-map file (e.g. `~/.room/state/<room_id>.tokens`).
    pub(crate) token_map_path: Arc<PathBuf>,
    /// Path to the persisted subscription-map file (e.g. `~/.room/state/<room_id>.subscriptions`).
    pub(crate) subscription_map_path: Arc<PathBuf>,
    pub(crate) room_id: Arc<String>,
    /// Set to `true` by the `/exit` admin command to shut down the broker.
    /// Using watch so receivers that check after the fact see `true` immediately
    /// — unlike `Notify`, this avoids the race where `notify_waiters()` fires
    /// before a task's `.notified()` future is registered.
    pub(crate) shutdown: Arc<watch::Sender<bool>>,
    /// Monotonically-increasing sequence counter. Incremented for every message
    /// broadcast or persisted by the broker, starting at 1.
    pub(crate) seq_counter: Arc<AtomicU64>,
    /// Plugin registry for dispatching `/` commands to plugins.
    pub(crate) plugin_registry: Arc<PluginRegistry>,
    /// Room visibility and access control configuration.
    /// `None` for rooms created without explicit config (backward compat).
    pub(crate) config: Option<RoomConfig>,
    /// Daemon-level user registry for cross-room identity. Unset in
    /// single-room mode. When set, admin commands (`/kick`, `/reauth`)
    /// also revoke tokens from the registry so users can rejoin after reauth.
    ///
    /// Uses `OnceLock` so it can be set after [`RoomState::new`] without
    /// requiring an extra constructor parameter (which would exceed the
    /// clippy `too-many-arguments` threshold).
    pub(crate) registry: OnceLock<Arc<Mutex<UserRegistry>>>,
}

impl RoomState {
    // ── Factory ───────────────────────────────────────────────────────────────

    /// Construct a fully wired `Arc<RoomState>` with default empty collections.
    ///
    /// Creates fresh empty maps for `clients`, `status_map`, `host_user`, `claim_map`,
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
        let claim_map: ClaimMap = Arc::new(Mutex::new(HashMap::new()));
        let plugins =
            crate::plugin::PluginRegistry::with_all_plugins(&chat_path, claim_map.clone())
                .map_err(|e| format!("plugin error: {e}"))?;

        let (shutdown_tx, _) = watch::channel(false);

        Ok(Arc::new(Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_map: Arc::new(Mutex::new(HashMap::new())),
            host_user: Arc::new(Mutex::new(None)),
            token_map,
            claim_map,
            subscription_map,
            chat_path: Arc::new(chat_path),
            token_map_path: Arc::new(token_map_path),
            subscription_map_path: Arc::new(subscription_map_path),
            room_id: Arc::new(room_id),
            shutdown: Arc::new(shutdown_tx),
            seq_counter: Arc::new(AtomicU64::new(0)),
            plugin_registry: Arc::new(plugins),
            config,
            registry: OnceLock::new(),
        }))
    }

    // ── registry ──────────────────────────────────────────────────────────────

    /// Attach the daemon-level [`UserRegistry`] to this room.
    ///
    /// Must be called at most once, immediately after construction, before the
    /// `Arc<RoomState>` is shared with other tasks. Silently no-ops if called
    /// a second time (consistent with `OnceLock` semantics).
    pub(crate) fn set_registry(&self, registry: Arc<Mutex<UserRegistry>>) {
        let _ = self.registry.set(registry);
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

    // ── claim_map accessors ───────────────────────────────────────────────────

    /// Record a task claim for `user`. Replaces any existing claim.
    /// Stores the current timestamp for lease-based expiry.
    pub(crate) async fn set_claim(&self, user: &str, task: String) {
        self.claim_map.lock().await.insert(
            user.to_owned(),
            ClaimEntry {
                task,
                claimed_at: std::time::Instant::now(),
            },
        );
    }

    /// Remove and return a user's active claim task, if any.
    pub(crate) async fn remove_claim(&self, user: &str) -> Option<String> {
        self.claim_map.lock().await.remove(user).map(|e| e.task)
    }

    /// Sweep expired claims and return remaining (username, task, elapsed)
    /// triples, sorted by username.
    pub(crate) async fn claim_entries(&self) -> Vec<(String, String, std::time::Duration)> {
        let mut map = self.claim_map.lock().await;
        let now = std::time::Instant::now();
        map.retain(|_, entry| now.duration_since(entry.claimed_at) < CLAIM_TTL);
        let mut entries: Vec<(String, String, std::time::Duration)> = map
            .iter()
            .map(|(u, e)| (u.clone(), e.task.clone(), now.duration_since(e.claimed_at)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    // ── subscription_map accessors ────────────────────────────────────────────

    /// Set a user's subscription tier.
    pub(crate) async fn set_subscription(&self, user: &str, tier: SubscriptionTier) {
        self.subscription_map
            .lock()
            .await
            .insert(user.to_owned(), tier);
    }

    /// Return all (username, tier) subscription pairs, sorted by username.
    pub(crate) async fn subscription_entries(&self) -> Vec<(String, SubscriptionTier)> {
        let mut entries: Vec<(String, SubscriptionTier)> = self
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
        self.subscription_map.lock().await.clone()
    }

    /// Number of subscribed users.
    pub(crate) async fn subscription_count(&self) -> usize {
        self.subscription_map.lock().await.len()
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
            state.plugin_registry.resolve("help").is_some(),
            "help plugin should be registered"
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
        assert!(state.claim_entries().await.is_empty());
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
        assert!(state.token_map.lock().await.contains_key("tok-1"));
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

    // ── claim_map accessors ───────────────────────────────────────────────────

    #[tokio::test]
    async fn set_claim_and_claim_entries() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state.set_claim("alice", "fix #42".to_owned()).await;
        let entries = state.claim_entries().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "alice");
        assert_eq!(entries[0].1, "fix #42");
        // elapsed should be very small (just created)
        assert!(entries[0].2.as_secs() < 2);
    }

    #[tokio::test]
    async fn remove_claim_returns_task_and_deletes() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state.set_claim("alice", "task".to_owned()).await;
        let removed = state.remove_claim("alice").await;
        assert_eq!(removed, Some("task".to_owned()));
        assert!(state.claim_entries().await.is_empty());
    }

    #[tokio::test]
    async fn remove_claim_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        assert_eq!(state.remove_claim("nobody").await, None);
    }

    #[tokio::test]
    async fn claim_entries_sorted_by_username() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state.set_claim("bob", "b".to_owned()).await;
        state.set_claim("alice", "a".to_owned()).await;
        let entries = state.claim_entries().await;
        assert_eq!(entries[0].0, "alice");
        assert_eq!(entries[1].0, "bob");
    }

    #[tokio::test]
    async fn claim_entries_sweeps_expired() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        // Insert a claim with a backdated timestamp (expired)
        {
            let mut map = state.claim_map.lock().await;
            map.insert(
                "stale".to_owned(),
                ClaimEntry {
                    task: "old task".to_owned(),
                    claimed_at: std::time::Instant::now()
                        - CLAIM_TTL
                        - std::time::Duration::from_secs(1),
                },
            );
        }
        // Also insert a fresh claim
        state.set_claim("fresh", "new task".to_owned()).await;
        let entries = state.claim_entries().await;
        assert_eq!(entries.len(), 1, "expired claim should be swept");
        assert_eq!(entries[0].0, "fresh");
    }

    #[tokio::test]
    async fn claim_reclaim_by_owner_resets_timestamp() {
        let dir = TempDir::new().unwrap();
        let state = make_state(&dir);
        state.set_claim("alice", "task A".to_owned()).await;
        // Re-claim should replace entry
        state.set_claim("alice", "task B".to_owned()).await;
        let entries = state.claim_entries().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "task B");
    }

    #[tokio::test]
    async fn claim_ttl_is_30_minutes() {
        assert_eq!(CLAIM_TTL, std::time::Duration::from_secs(30 * 60));
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
}
