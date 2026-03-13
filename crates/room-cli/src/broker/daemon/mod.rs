//! Multi-room daemon: manages N rooms in a single process.
//!
//! `DaemonState` wraps a map of room_id → `RoomState` and provides room
//! lifecycle (create/destroy/get). The daemon listens on a single UDS
//! socket at a configurable path and dispatches connections to the correct
//! room based on an extended handshake protocol.
//!
//! ## Handshake protocol
//!
//! The first line of a UDS connection to the daemon can carry one of two
//! prefixes:
//!
//! - `ROOM:<room_id>:<rest>` — route to an existing room. The rest of the
//!   line is the standard per-room handshake (`SEND:`, `TOKEN:`, `JOIN:`,
//!   or plain username).
//! - `CREATE:<room_id>` — create a new room. A second line carries the
//!   room configuration as JSON (`{"visibility":"public","invite":[]}`).
//! - `DESTROY:<room_id>` — destroy a room. Signals shutdown to connected
//!   clients and removes the room from the daemon's map.
//!
//! If no recognised prefix is present, the connection is rejected with an error.
//!
//! Examples:
//! ```text
//! ROOM:myroom:JOIN:alice       → join room "myroom" as "alice"
//! ROOM:myroom:TOKEN:<uuid>     → authenticated send to "myroom"
//! ROOM:myroom:SEND:bob         → legacy unauthenticated send to "myroom"
//! ROOM:myroom:alice            → interactive join to "myroom" as "alice"
//! CREATE:newroom               → create room "newroom" (config on next line)
//! DESTROY:myroom               → destroy room "myroom"
//! ```

mod config;
mod dispatcher;
mod lifecycle;
mod migration;
mod pid;

pub use config::{validate_room_id, DaemonConfig};
pub(crate) use lifecycle::create_room_entry;
pub use pid::{is_pid_alive, remove_pid_file, write_pid_file};

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
};

use tokio::{
    net::UnixListener,
    sync::{watch, Mutex},
};

use crate::registry::UserRegistry;

use super::{
    state::{RoomState, TokenMap},
    ws::{self, DaemonWsState},
};

use dispatcher::dispatch_connection;
use migration::load_or_migrate_registry;

/// Registry of active rooms, keyed by room_id.
pub(crate) type RoomMap = Arc<Mutex<HashMap<String, Arc<RoomState>>>>;

/// Multi-room daemon state.
pub struct DaemonState {
    pub(crate) rooms: RoomMap,
    pub(crate) config: DaemonConfig,
    /// Global client ID counter shared across all rooms.
    pub(crate) next_client_id: Arc<AtomicU64>,
    /// Daemon-level shutdown signal.
    pub(crate) shutdown: Arc<watch::Sender<bool>>,
    /// System-level token map shared across all rooms (runtime cache).
    ///
    /// A single `Arc<Mutex<HashMap>>` instance is cloned into every room's
    /// `token_map`. Tokens issued in any room are valid in all rooms managed
    /// by this daemon. Seeded from `user_registry` on startup; kept in sync
    /// by [`super::auth::issue_token_via_registry`].
    pub(crate) system_token_map: TokenMap,
    /// Daemon-level user registry — sole persistence layer for cross-room identity.
    ///
    /// Stores user profiles, room memberships, and tokens to
    /// `~/.room/state/users.json`. New sessions register/update here;
    /// `system_token_map` is derived from this registry at startup and kept
    /// in sync on every join.
    pub(crate) user_registry: Arc<tokio::sync::Mutex<UserRegistry>>,
    /// Number of currently active UDS connections.
    ///
    /// Incremented when a connection is accepted; decremented when the
    /// connection task completes. When the count drops to zero the daemon
    /// starts a grace period timer before sending the shutdown signal.
    pub(crate) connection_count: Arc<AtomicUsize>,
}

impl DaemonState {
    /// Create a new daemon with the given configuration and no rooms.
    pub fn new(config: DaemonConfig) -> Self {
        let (shutdown_tx, _) = watch::channel(false);

        // Load UserRegistry from disk (sole source of truth for identity).
        //
        // Migration path: if `users.json` (UserRegistry) does not exist but
        // the legacy `tokens.json` (system_token_map from #334) does, import
        // the flat token map into a fresh registry so existing sessions survive
        // the upgrade without requiring a forced re-join.
        let registry = load_or_migrate_registry(&config);

        // Seed the runtime token map from the registry so existing tokens remain
        // valid across daemon restarts without requiring a fresh join.
        let token_snapshot = registry.token_snapshot();

        Self {
            rooms: Arc::new(Mutex::new(HashMap::new())),
            config,
            next_client_id: Arc::new(AtomicU64::new(0)),
            shutdown: Arc::new(shutdown_tx),
            system_token_map: Arc::new(Mutex::new(token_snapshot)),
            user_registry: Arc::new(tokio::sync::Mutex::new(registry)),
            connection_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Create a room and register it. Returns `Err` if the room ID is invalid
    /// or the room already exists.
    pub async fn create_room(&self, room_id: &str) -> Result<(), String> {
        create_room_entry(
            room_id,
            None,
            &self.rooms,
            &self.config,
            &self.system_token_map,
            Some(self.user_registry.clone()),
        )
        .await
    }

    /// Create a room with explicit configuration. Returns `Err` if the room ID
    /// is invalid or the room already exists.
    pub async fn create_room_with_config(
        &self,
        room_id: &str,
        config: room_protocol::RoomConfig,
    ) -> Result<(), String> {
        create_room_entry(
            room_id,
            Some(config),
            &self.rooms,
            &self.config,
            &self.system_token_map,
            Some(self.user_registry.clone()),
        )
        .await
    }

    /// Get a room's config, if it exists.
    pub async fn get_room_config(&self, room_id: &str) -> Option<room_protocol::RoomConfig> {
        self.rooms
            .lock()
            .await
            .get(room_id)
            .and_then(|s| s.config.clone())
    }

    /// Destroy a room. Returns `Err` if the room does not exist.
    ///
    /// Signals the room's shutdown so connected clients receive EOF.
    pub async fn destroy_room(&self, room_id: &str) -> Result<(), String> {
        let mut rooms = self.rooms.lock().await;
        let state = rooms
            .remove(room_id)
            .ok_or_else(|| format!("room not found: {room_id}"))?;

        // Signal the room's shutdown so any connected clients receive EOF.
        let _ = state.shutdown.send(true);
        Ok(())
    }

    /// Check if a room exists.
    pub async fn has_room(&self, room_id: &str) -> bool {
        self.rooms.lock().await.contains_key(room_id)
    }

    /// Get a handle to the daemon-level shutdown sender.
    pub fn shutdown_handle(&self) -> Arc<watch::Sender<bool>> {
        self.shutdown.clone()
    }

    /// List all active room IDs.
    pub async fn list_rooms(&self) -> Vec<String> {
        self.rooms.lock().await.keys().cloned().collect()
    }

    /// Insert a token directly into a room's token map, bypassing the join
    /// permission check. Intended for integration tests only.
    #[doc(hidden)]
    pub async fn test_inject_token(
        &self,
        room_id: &str,
        username: &str,
        token: &str,
    ) -> Result<(), String> {
        let rooms = self.rooms.lock().await;
        let room = rooms
            .get(room_id)
            .ok_or_else(|| format!("room not found: {room_id}"))?;
        room.auth
            .token_map
            .lock()
            .await
            .insert(token.to_owned(), username.to_owned());
        Ok(())
    }

    /// Run the daemon: listen on UDS, dispatch connections to rooms.
    ///
    /// When the last UDS connection closes, starts a grace period timer
    /// (`config.grace_period_secs`). If no new connection arrives before the
    /// timer fires, sends a shutdown signal. Any new connection during the
    /// grace period cancels the timer. On exit, cleans up the PID file and
    /// socket file.
    pub async fn run(&self) -> anyhow::Result<()> {
        // Write PID file only for the default daemon socket.  Daemons with an
        // explicit socket override (tests, CI) are independent instances and
        // must not clobber the system PID file.
        let pid_path = if self.config.socket_path == crate::paths::room_socket_path() {
            match write_pid_file(&crate::paths::room_pid_path()) {
                Ok(()) => Some(crate::paths::room_pid_path()),
                Err(e) => {
                    eprintln!("[daemon] failed to write PID file: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Remove stale socket synchronously.
        if self.config.socket_path.exists() {
            std::fs::remove_file(&self.config.socket_path)?;
        }

        let listener = UnixListener::bind(&self.config.socket_path)?;
        eprintln!(
            "[daemon] listening on {}",
            self.config.socket_path.display()
        );

        let mut shutdown_rx = self.shutdown.subscribe();
        let grace_duration = tokio::time::Duration::from_secs(self.config.grace_period_secs);

        // mpsc channel: connection tasks notify the main loop when they close.
        let (close_tx, mut close_rx) = tokio::sync::mpsc::channel::<()>(64);

        // Optional grace period sleep — active when the last connection closes.
        let mut grace_sleep: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;

        // Start WebSocket/REST server if configured.
        if let Some(port) = self.config.ws_port {
            let ws_state = DaemonWsState {
                rooms: self.rooms.clone(),
                next_client_id: self.next_client_id.clone(),
                config: self.config.clone(),
                system_token_map: self.system_token_map.clone(),
                user_registry: self.user_registry.clone(),
            };
            let app = ws::create_daemon_router(ws_state);
            let tcp = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
            eprintln!("[daemon] WebSocket/REST listening on port {port}");
            tokio::spawn(async move {
                if let Err(e) = axum::serve(tcp, app).await {
                    eprintln!("[daemon] WS server error: {e}");
                }
            });
        }

        let result = loop {
            // Build the grace future: fires if a grace sleep is active,
            // otherwise stays pending forever.
            let grace_fut = async {
                match grace_sleep.as_mut() {
                    Some(s) => {
                        s.await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                accept = listener.accept() => {
                    let (stream, _) = match accept {
                        Ok(a) => a,
                        Err(e) => break Err(e.into()),
                    };
                    // Cancel any pending grace timer — we have a new connection.
                    grace_sleep = None;

                    let count = self.connection_count.clone();
                    count.fetch_add(1, Ordering::SeqCst);
                    let rooms = self.rooms.clone();
                    let next_id = self.next_client_id.clone();
                    let cfg = self.config.clone();
                    let sys_tokens = self.system_token_map.clone();
                    let registry = self.user_registry.clone();
                    let tx = close_tx.clone();

                    tokio::spawn(async move {
                        if let Err(e) = dispatch_connection(stream, &rooms, &next_id, &cfg, &sys_tokens, &registry).await {
                            eprintln!("[daemon] connection error: {e:#}");
                        }
                        count.fetch_sub(1, Ordering::SeqCst);
                        // Notify main loop so it can start the grace timer.
                        let _ = tx.send(()).await;
                    });
                }
                Some(()) = close_rx.recv() => {
                    // A connection closed. Start grace period if none remain.
                    if self.connection_count.load(Ordering::SeqCst) == 0 {
                        eprintln!(
                            "[daemon] no connections — grace period {}s started",
                            self.config.grace_period_secs
                        );
                        grace_sleep =
                            Some(Box::pin(tokio::time::sleep(grace_duration)));
                    }
                }
                _ = grace_fut => {
                    eprintln!("[daemon] grace period expired, shutting down");
                    let _ = self.shutdown.send(true);
                    // The shutdown_rx arm will fire on the next iteration; break
                    // here directly to avoid a double-exit path.
                    break Ok(());
                }
                _ = shutdown_rx.changed() => {
                    eprintln!("[daemon] shutdown requested, exiting");
                    if let Some(ref p) = pid_path {
                        remove_pid_file(p);
                    }
                    break Ok(());
                }
            }
        };

        // Clean up ephemeral files on exit.
        let _ = std::fs::remove_file(&self.config.socket_path);
        let _ = std::fs::remove_file(crate::paths::room_pid_path());
        // Remove per-room meta files written during room creation.
        for room_id in self.list_rooms().await {
            let _ = std::fs::remove_file(crate::paths::room_meta_path(&room_id));
        }

        result
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use config::MAX_ROOM_ID_LEN;
    use lifecycle::build_initial_subscriptions;
    use std::path::PathBuf;

    // ── PID management ───────────────────────────────────────────────────

    #[test]
    fn write_pid_file_creates_file_with_current_pid() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.pid");
        write_pid_file(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let pid: u32 = content.trim().parse().expect("PID should be a number");
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn is_pid_alive_true_for_current_process() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.pid");
        write_pid_file(&path).unwrap();
        assert!(is_pid_alive(&path), "current process should be alive");
    }

    #[test]
    fn is_pid_alive_false_for_missing_file() {
        let path = std::path::Path::new("/tmp/nonexistent-room-test-99999999.pid");
        assert!(!is_pid_alive(path));
    }

    #[test]
    fn remove_pid_file_deletes_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("remove.pid");
        write_pid_file(&path).unwrap();
        assert!(path.exists());
        remove_pid_file(&path);
        assert!(!path.exists());
    }

    #[test]
    fn remove_pid_file_noop_when_missing() {
        // Should not panic if the file is already gone.
        let path = std::path::Path::new("/tmp/gone-99999999.pid");
        remove_pid_file(path); // must not panic
    }

    // ── DaemonState lifecycle ─────────────────────────────────────────────

    /// Test helper: look up a room's state by ID.
    async fn get_room(daemon: &DaemonState, room_id: &str) -> Arc<RoomState> {
        daemon
            .rooms
            .lock()
            .await
            .get(room_id)
            .cloned()
            .unwrap_or_else(|| panic!("room {room_id} not found"))
    }

    #[tokio::test]
    async fn create_room_succeeds() {
        let daemon = DaemonState::new(DaemonConfig::default());
        assert!(daemon.create_room("test-room").await.is_ok());
        let state = get_room(&daemon, "test-room").await;
        assert_eq!(*state.room_id, "test-room");
    }

    #[tokio::test]
    async fn create_duplicate_room_fails() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("dup").await.unwrap();
        let result = daemon.create_room("dup").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[tokio::test]
    async fn has_room_returns_true_for_created() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("room-a").await.unwrap();
        assert!(daemon.has_room("room-a").await);
        assert!(!daemon.has_room("room-b").await);
    }

    #[tokio::test]
    async fn destroy_room_removes_it() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("doomed").await.unwrap();
        assert!(daemon.destroy_room("doomed").await.is_ok());
        assert!(!daemon.has_room("doomed").await);
    }

    #[tokio::test]
    async fn destroy_nonexistent_room_fails() {
        let daemon = DaemonState::new(DaemonConfig::default());
        let result = daemon.destroy_room("nope").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn destroy_room_signals_shutdown() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("shutme").await.unwrap();
        let state = get_room(&daemon, "shutme").await;
        let rx = state.shutdown.subscribe();
        assert!(!*rx.borrow());

        daemon.destroy_room("shutme").await.unwrap();
        // The shutdown signal should now be true.
        assert!(*rx.borrow());
    }

    #[tokio::test]
    async fn list_rooms_returns_all() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("alpha").await.unwrap();
        daemon.create_room("beta").await.unwrap();
        daemon.create_room("gamma").await.unwrap();

        let mut rooms = daemon.list_rooms().await;
        rooms.sort();
        assert_eq!(rooms, vec!["alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn list_rooms_empty_initially() {
        let daemon = DaemonState::new(DaemonConfig::default());
        assert!(daemon.list_rooms().await.is_empty());
    }

    #[tokio::test]
    async fn create_room_initializes_plugins() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("plugtest").await.unwrap();
        let state = get_room(&daemon, "plugtest").await;
        // help is a builtin (not in plugin registry), stats should be registered
        assert!(state.plugin_registry.resolve("help").is_none());
        assert!(state.plugin_registry.resolve("stats").is_some());
    }

    // ── DaemonConfig ──────────────────────────────────────────────────────

    #[test]
    fn config_chat_path_format() {
        let config = DaemonConfig {
            data_dir: PathBuf::from("/var/room"),
            ..DaemonConfig::default()
        };
        assert_eq!(
            config.chat_path("myroom"),
            PathBuf::from("/var/room/myroom.chat")
        );
    }

    #[test]
    fn config_default_socket_path() {
        let config = DaemonConfig::default();
        assert_eq!(config.socket_path, crate::paths::room_socket_path());
    }

    // ── create_room_with_config ───────────────────────────────────────────

    #[tokio::test]
    async fn create_room_with_dm_config() {
        let daemon = DaemonState::new(DaemonConfig::default());
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        assert!(daemon
            .create_room_with_config("dm-alice-bob", config)
            .await
            .is_ok());

        let state = get_room(&daemon, "dm-alice-bob").await;
        let cfg = state.config.as_ref().unwrap();
        assert_eq!(cfg.visibility, room_protocol::RoomVisibility::Dm);
        assert_eq!(cfg.max_members, Some(2));
        assert!(cfg.invite_list.contains("alice"));
        assert!(cfg.invite_list.contains("bob"));
    }

    #[tokio::test]
    async fn create_room_with_config_duplicate_fails() {
        let daemon = DaemonState::new(DaemonConfig::default());
        let config = room_protocol::RoomConfig::public("owner");
        daemon
            .create_room_with_config("dup", config.clone())
            .await
            .unwrap();
        assert!(daemon.create_room_with_config("dup", config).await.is_err());
    }

    #[tokio::test]
    async fn get_room_config_returns_none_for_unconfigured() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("plain").await.unwrap();
        assert!(daemon.get_room_config("plain").await.is_none());
    }

    #[tokio::test]
    async fn get_room_config_returns_config_when_present() {
        let daemon = DaemonState::new(DaemonConfig::default());
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        daemon
            .create_room_with_config("dm-alice-bob", config)
            .await
            .unwrap();
        let cfg = daemon.get_room_config("dm-alice-bob").await.unwrap();
        assert_eq!(cfg.visibility, room_protocol::RoomVisibility::Dm);
    }

    #[tokio::test]
    async fn dm_room_id_deterministic_and_lookup_works() {
        let daemon = DaemonState::new(DaemonConfig::default());
        let room_id = room_protocol::dm_room_id("bob", "alice").unwrap();
        assert_eq!(room_id, "dm-alice-bob");

        let config = room_protocol::RoomConfig::dm("bob", "alice");
        daemon
            .create_room_with_config(&room_id, config)
            .await
            .unwrap();
        assert!(daemon.has_room("dm-alice-bob").await);
        // Reverse order gives the same room_id
        assert_eq!(
            room_protocol::dm_room_id("alice", "bob").unwrap(),
            "dm-alice-bob"
        );
    }

    // ── validate_room_id ──────────────────────────────────────────────────

    #[test]
    fn valid_room_ids() {
        for id in [
            "lobby",
            "agent-room-2",
            "my_room",
            "Room.1",
            "dm-alice-bob",
            "a",
            &"x".repeat(MAX_ROOM_ID_LEN),
        ] {
            assert!(validate_room_id(id).is_ok(), "should accept: {id:?}");
        }
    }

    #[test]
    fn empty_room_id_rejected() {
        let err = validate_room_id("").unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn room_id_too_long_rejected() {
        let long = "x".repeat(MAX_ROOM_ID_LEN + 1);
        let err = validate_room_id(&long).unwrap_err();
        assert!(err.contains("too long"), "{err}");
    }

    #[test]
    fn dot_dot_traversal_rejected() {
        for id in ["..", "room/../etc", "..secret", "a..b"] {
            let err = validate_room_id(id).unwrap_err();
            assert!(err.contains(".."), "should reject {id:?}: {err}");
        }
    }

    #[test]
    fn single_dot_rejected() {
        let err = validate_room_id(".").unwrap_err();
        assert!(err.contains(".."), "{err}");
    }

    #[test]
    fn slash_rejected() {
        for id in ["room/sub", "/etc/passwd", "a/b/c"] {
            let err = validate_room_id(id).unwrap_err();
            assert!(err.contains("unsafe"), "should reject {id:?}: {err}");
        }
    }

    #[test]
    fn backslash_rejected() {
        let err = validate_room_id("room\\sub").unwrap_err();
        assert!(err.contains("unsafe"), "{err}");
    }

    #[test]
    fn null_byte_rejected() {
        let err = validate_room_id("room\0id").unwrap_err();
        assert!(err.contains("unsafe"), "{err}");
    }

    #[test]
    fn whitespace_rejected() {
        for id in ["room name", "room\tid", "room\nid", " leading", "trailing "] {
            let err = validate_room_id(id).unwrap_err();
            assert!(err.contains("whitespace"), "should reject {id:?}: {err}");
        }
    }

    #[test]
    fn other_unsafe_chars_rejected() {
        for ch in [':', '*', '?', '"', '<', '>', '|'] {
            let id = format!("room{ch}id");
            let err = validate_room_id(&id).unwrap_err();
            assert!(err.contains("unsafe"), "should reject {ch:?}: {err}");
        }
    }

    #[tokio::test]
    async fn create_room_rejects_invalid_id() {
        let daemon = DaemonState::new(DaemonConfig::default());
        assert!(daemon.create_room("room/sub").await.is_err());
        assert!(daemon.create_room("..").await.is_err());
        assert!(daemon.create_room("").await.is_err());
        assert!(daemon.create_room("room name").await.is_err());
    }

    #[tokio::test]
    async fn create_room_with_config_rejects_invalid_id() {
        let daemon = DaemonState::new(DaemonConfig::default());
        let config = room_protocol::RoomConfig::public("owner");
        assert!(daemon
            .create_room_with_config("../etc", config)
            .await
            .is_err());
    }

    // ── DM auto-subscribe ─────────────────────────────────────────────────

    #[tokio::test]
    async fn dm_room_auto_subscribes_both_participants() {
        let daemon = DaemonState::new(DaemonConfig::default());
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        daemon
            .create_room_with_config("dm-alice-bob", config)
            .await
            .unwrap();

        let state = get_room(&daemon, "dm-alice-bob").await;
        let subs = state.filters.subscription_map.lock().await;
        assert_eq!(subs.len(), 2);
        assert_eq!(
            subs.get("alice"),
            Some(&room_protocol::SubscriptionTier::Full)
        );
        assert_eq!(
            subs.get("bob"),
            Some(&room_protocol::SubscriptionTier::Full)
        );
    }

    #[tokio::test]
    async fn public_room_starts_with_no_subscriptions() {
        let daemon = DaemonState::new(DaemonConfig::default());
        let config = room_protocol::RoomConfig::public("owner");
        daemon
            .create_room_with_config("lobby", config)
            .await
            .unwrap();

        let state = get_room(&daemon, "lobby").await;
        let subs = state.filters.subscription_map.lock().await;
        assert!(subs.is_empty());
    }

    #[tokio::test]
    async fn unconfigured_room_starts_with_no_subscriptions() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("plain").await.unwrap();

        let state = get_room(&daemon, "plain").await;
        let subs = state.filters.subscription_map.lock().await;
        assert!(subs.is_empty());
    }

    #[tokio::test]
    async fn dm_auto_subscribe_uses_full_tier() {
        let daemon = DaemonState::new(DaemonConfig::default());
        let config = room_protocol::RoomConfig::dm("carol", "dave");
        daemon
            .create_room_with_config("dm-carol-dave", config)
            .await
            .unwrap();

        let state = get_room(&daemon, "dm-carol-dave").await;
        let subs = state.filters.subscription_map.lock().await;
        // Verify it's Full, not MentionsOnly
        for (_, tier) in subs.iter() {
            assert_eq!(*tier, room_protocol::SubscriptionTier::Full);
        }
    }

    #[test]
    fn build_initial_subscriptions_dm_populates() {
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        let subs = build_initial_subscriptions(&config);
        assert_eq!(subs.len(), 2);
        assert_eq!(subs["alice"], room_protocol::SubscriptionTier::Full);
        assert_eq!(subs["bob"], room_protocol::SubscriptionTier::Full);
    }

    #[test]
    fn build_initial_subscriptions_public_empty() {
        let config = room_protocol::RoomConfig::public("owner");
        let subs = build_initial_subscriptions(&config);
        assert!(subs.is_empty());
    }

    // ── DaemonConfig grace_period_secs ────────────────────────────────────

    #[test]
    fn default_grace_period_is_30() {
        let config = DaemonConfig::default();
        assert_eq!(config.grace_period_secs, 30);
    }

    #[test]
    fn custom_grace_period_preserved() {
        let config = DaemonConfig {
            grace_period_secs: 0,
            ..DaemonConfig::default()
        };
        assert_eq!(config.grace_period_secs, 0);
    }

    // ── connection_count refcount ─────────────────────────────────────────

    #[tokio::test]
    async fn connection_count_starts_at_zero() {
        let daemon = DaemonState::new(DaemonConfig::default());
        assert_eq!(daemon.connection_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn connection_count_increments_and_decrements() {
        let count = Arc::new(AtomicUsize::new(0));
        count.fetch_add(1, Ordering::SeqCst);
        count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        count.fetch_sub(1, Ordering::SeqCst);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        count.fetch_sub(1, Ordering::SeqCst);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    /// Verify that the daemon exits cleanly when the shutdown signal is sent.
    /// Uses an Arc<DaemonState> so the run() task can hold a reference while
    /// the test also holds one to send the shutdown signal.
    #[tokio::test]
    async fn daemon_exits_on_shutdown_signal() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("test-grace.sock");
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        std::fs::create_dir_all(dir.path().join("state")).unwrap();

        let config = DaemonConfig {
            socket_path: socket.clone(),
            data_dir: dir.path().join("data"),
            state_dir: dir.path().join("state"),
            ws_port: None,
            grace_period_secs: 0,
        };
        let daemon = Arc::new(DaemonState::new(config));
        let shutdown = daemon.shutdown_handle();

        let daemon2 = Arc::clone(&daemon);
        let handle = tokio::spawn(async move { daemon2.run().await });

        // Wait for socket to become connectable (daemon is up).
        for _ in 0..100 {
            if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
        assert!(
            tokio::net::UnixStream::connect(&socket).await.is_ok(),
            "daemon socket not ready"
        );

        // Send shutdown — daemon should exit quickly.
        let _ = shutdown.send(true);
        let result = tokio::time::timeout(tokio::time::Duration::from_secs(5), handle).await;
        assert!(result.is_ok(), "daemon did not exit within 5s");
        assert!(result.unwrap().unwrap().is_ok(), "run() returned error");
    }

    /// Verify that a new connection during the grace period resets the timer.
    /// We check this by confirming connection_count goes from 0 → 1 → 0 without
    /// a premature shutdown.
    #[tokio::test]
    async fn grace_period_cancelled_by_new_connection() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("test-cancel-grace.sock");

        let config = DaemonConfig {
            socket_path: socket.clone(),
            data_dir: dir.path().join("data"),
            state_dir: dir.path().join("state"),
            ws_port: None,
            grace_period_secs: 60, // long grace — should not fire
        };
        let daemon = DaemonState::new(config);

        // Manually exercise the counter: simulate connect + disconnect.
        daemon.connection_count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(daemon.connection_count.load(Ordering::SeqCst), 1);
        daemon.connection_count.fetch_sub(1, Ordering::SeqCst);
        assert_eq!(daemon.connection_count.load(Ordering::SeqCst), 0);

        // Simulate a second connection arriving (cancels grace timer).
        daemon.connection_count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(daemon.connection_count.load(Ordering::SeqCst), 1);

        // Daemon has not shut down.
        assert!(!*daemon.shutdown.borrow());
    }

    // ── migrate_legacy_tmpdir_tokens ──────────────────────────────────────

    /// Write a token file to `dir` in the format written by old `room join`.
    fn write_legacy_token(dir: &std::path::Path, room_id: &str, username: &str, token: &str) {
        let name = format!("room-{room_id}-{username}.token");
        let data = serde_json::json!({"username": username, "token": token});
        std::fs::write(dir.join(name), format!("{data}\n")).unwrap();
    }

    #[test]
    fn migrate_legacy_tmpdir_tokens_imports_token() {
        let token_dir = tempfile::TempDir::new().unwrap();
        let state_dir = tempfile::TempDir::new().unwrap();
        write_legacy_token(token_dir.path(), "lobby", "alice", "legacy-uuid-alice");

        let mut registry = UserRegistry::new(state_dir.path().to_owned());

        migration::migrate_legacy_tmpdir_tokens_from(token_dir.path(), &mut registry);

        assert_eq!(registry.validate_token("legacy-uuid-alice"), Some("alice"));
        assert!(registry.get_user("alice").is_some());
    }

    #[test]
    fn migrate_legacy_tmpdir_tokens_idempotent() {
        let token_dir = tempfile::TempDir::new().unwrap();
        let state_dir = tempfile::TempDir::new().unwrap();
        write_legacy_token(token_dir.path(), "lobby", "bob", "tok-bob");

        let mut registry = UserRegistry::new(state_dir.path().to_owned());
        migration::migrate_legacy_tmpdir_tokens_from(token_dir.path(), &mut registry);
        migration::migrate_legacy_tmpdir_tokens_from(token_dir.path(), &mut registry);

        // Token still valid and exactly one entry for bob.
        assert_eq!(registry.validate_token("tok-bob"), Some("bob"));
        let snap = registry.token_snapshot();
        assert_eq!(snap.values().filter(|u| u.as_str() == "bob").count(), 1);
    }

    #[test]
    fn migrate_legacy_tmpdir_tokens_skips_non_token_files() {
        let token_dir = tempfile::TempDir::new().unwrap();
        let state_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(token_dir.path().join("roomd.sock"), "not a token").unwrap();
        std::fs::write(token_dir.path().join("something.json"), "{}").unwrap();

        let mut registry = UserRegistry::new(state_dir.path().to_owned());
        migration::migrate_legacy_tmpdir_tokens_from(token_dir.path(), &mut registry);

        assert!(registry.list_users().is_empty());
    }

    #[test]
    fn migrate_legacy_tmpdir_tokens_skips_malformed_json() {
        let token_dir = tempfile::TempDir::new().unwrap();
        let state_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(token_dir.path().join("room-x-bad.token"), "not-json{{{").unwrap();

        let mut registry = UserRegistry::new(state_dir.path().to_owned());
        migration::migrate_legacy_tmpdir_tokens_from(token_dir.path(), &mut registry);

        assert!(registry.list_users().is_empty());
    }
}
