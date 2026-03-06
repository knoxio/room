use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{atomic::AtomicU64, Arc},
};

use tokio::sync::{broadcast, watch, Mutex};

use crate::plugin::PluginRegistry;

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

/// Shared broker state passed to every client handler.
pub(crate) struct RoomState {
    pub(crate) clients: ClientMap,
    pub(crate) status_map: StatusMap,
    pub(crate) host_user: HostUser,
    pub(crate) token_map: TokenMap,
    pub(crate) chat_path: Arc<PathBuf>,
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
}
