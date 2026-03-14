//! Plugin framework types for the room chat system.
//!
//! This module defines the traits and types needed to implement a room plugin.
//! External crates can depend on `room-protocol` alone to implement [`Plugin`]
//! — no dependency on `room-cli` or broker internals is required.

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};

use crate::{EventType, Message};

/// Boxed future type used by [`Plugin::handle`] for dyn compatibility.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ── Plugin trait ────────────────────────────────────────────────────────────

/// A plugin that handles one or more `/` commands and/or reacts to room
/// lifecycle events.
///
/// Implement this trait and register it with the broker's plugin registry to
/// add custom commands to a room. The broker dispatches matching
/// `Message::Command` messages to the plugin's [`handle`](Plugin::handle)
/// method, and calls [`on_user_join`](Plugin::on_user_join) /
/// [`on_user_leave`](Plugin::on_user_leave) when users enter or leave.
///
/// Only [`name`](Plugin::name) and [`handle`](Plugin::handle) are required.
/// All other methods have no-op / empty-vec defaults so that adding new
/// lifecycle hooks in future releases does not break existing plugins.
pub trait Plugin: Send + Sync {
    /// Unique identifier for this plugin (e.g. `"stats"`, `"help"`).
    fn name(&self) -> &str;

    /// Semantic version of this plugin (e.g. `"1.0.0"`).
    ///
    /// Used for diagnostics and `/info` output. Defaults to `"0.0.0"` for
    /// plugins that do not track their own version.
    fn version(&self) -> &str {
        "0.0.0"
    }

    /// Plugin API version this plugin was written against.
    ///
    /// The broker rejects plugins whose `api_version()` exceeds the current
    /// [`PLUGIN_API_VERSION`]. Bump this constant when the `Plugin` trait
    /// gains new required methods or changes existing method signatures.
    ///
    /// Defaults to `1` (the initial API revision).
    fn api_version(&self) -> u32 {
        1
    }

    /// Minimum `room-protocol` crate version this plugin requires, as a
    /// semver string (e.g. `"3.1.0"`).
    ///
    /// The broker rejects plugins whose `min_protocol()` is newer than the
    /// running `room-protocol` version. Defaults to `"0.0.0"` (compatible
    /// with any protocol version).
    fn min_protocol(&self) -> &str {
        "0.0.0"
    }

    /// Commands this plugin handles. Each entry drives `/help` output
    /// and TUI autocomplete.
    ///
    /// Defaults to an empty vec for plugins that only use lifecycle hooks
    /// and do not register any commands.
    fn commands(&self) -> Vec<CommandInfo> {
        vec![]
    }

    /// Handle an invocation of one of this plugin's commands.
    ///
    /// Returns a boxed future for dyn compatibility (required because the
    /// registry stores `Box<dyn Plugin>`).
    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>>;

    /// Called after a user joins the room. The default is a no-op.
    ///
    /// Invoked synchronously during the join broadcast path. Implementations
    /// must not block — spawn a task if async work is needed.
    fn on_user_join(&self, _user: &str) {}

    /// Called after a user leaves the room. The default is a no-op.
    ///
    /// Invoked synchronously during the leave broadcast path. Implementations
    /// must not block — spawn a task if async work is needed.
    fn on_user_leave(&self, _user: &str) {}
}

/// Current Plugin API version. Increment when the `Plugin` trait changes in
/// a way that requires plugin authors to update their code (new required
/// methods, changed signatures, removed defaults).
///
/// Plugins returning an `api_version()` higher than this are rejected at
/// registration.
pub const PLUGIN_API_VERSION: u32 = 1;

/// The `room-protocol` crate version, derived from `Cargo.toml` at compile
/// time. Used by the broker to reject plugins that require a newer protocol
/// than the one currently running.
pub const PROTOCOL_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── CommandInfo ─────────────────────────────────────────────────────────────

/// Describes a single command for `/help` and autocomplete.
#[derive(Debug, Clone)]
pub struct CommandInfo {
    /// Command name without the leading `/`.
    pub name: String,
    /// One-line description shown in `/help` and autocomplete.
    pub description: String,
    /// Usage string (e.g. `"/stats [last N]"`).
    pub usage: String,
    /// Typed parameter schemas for validation and autocomplete.
    pub params: Vec<ParamSchema>,
}

// ── Typed parameter schema ─────────────────────────────────────────────────

/// Schema for a single command parameter — drives validation, `/help` output,
/// and TUI argument autocomplete.
#[derive(Debug, Clone)]
pub struct ParamSchema {
    /// Display name (e.g. `"username"`, `"count"`).
    pub name: String,
    /// What kind of value this parameter accepts.
    pub param_type: ParamType,
    /// Whether the parameter must be provided.
    pub required: bool,
    /// One-line description shown in `/help <command>`.
    pub description: String,
}

/// The kind of value a parameter accepts.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamType {
    /// Free-form text (no validation beyond presence).
    Text,
    /// One of a fixed set of allowed values.
    Choice(Vec<String>),
    /// An online username — TUI shows the mention picker.
    Username,
    /// An integer, optionally bounded.
    Number { min: Option<i64>, max: Option<i64> },
}

// ── CommandContext ───────────────────────────────────────────────────────────

/// Context passed to a plugin's `handle` method.
pub struct CommandContext {
    /// The command name that was invoked (without `/`).
    pub command: String,
    /// Arguments passed after the command name.
    pub params: Vec<String>,
    /// Username of the invoker.
    pub sender: String,
    /// Room ID.
    pub room_id: String,
    /// Message ID that triggered this command.
    pub message_id: String,
    /// Timestamp of the triggering message.
    pub timestamp: DateTime<Utc>,
    /// Scoped handle for reading chat history.
    pub history: Box<dyn HistoryAccess>,
    /// Scoped handle for writing back to the chat.
    pub writer: Box<dyn MessageWriter>,
    /// Snapshot of room metadata.
    pub metadata: RoomMetadata,
    /// All registered commands (so `/help` can list them without
    /// holding a reference to the registry).
    pub available_commands: Vec<CommandInfo>,
    /// Optional access to daemon-level team membership.
    ///
    /// `Some` in daemon mode (backed by `UserRegistry`), `None` in standalone
    /// mode where teams are not available.
    pub team_access: Option<Box<dyn TeamAccess>>,
}

// ── PluginResult ────────────────────────────────────────────────────────────

/// What the broker should do after a plugin handles a command.
pub enum PluginResult {
    /// Send a private reply only to the invoker.
    /// Second element is optional machine-readable data for programmatic consumers.
    Reply(String, Option<serde_json::Value>),
    /// Broadcast a message to the entire room.
    /// Second element is optional machine-readable data for programmatic consumers.
    Broadcast(String, Option<serde_json::Value>),
    /// Command handled silently (side effects already done via [`MessageWriter`]).
    Handled,
}

// ── MessageWriter trait ─────────────────────────────────────────────────────

/// Async message dispatch for plugins. Abstracts over the broker's broadcast
/// and persistence machinery so plugins never touch broker internals.
///
/// The broker provides a concrete implementation; external crates only see
/// this trait.
pub trait MessageWriter: Send + Sync {
    /// Broadcast a system message to all connected clients and persist to history.
    fn broadcast(&self, content: &str) -> BoxFuture<'_, anyhow::Result<()>>;

    /// Send a private system message only to a specific user.
    fn reply_to(&self, username: &str, content: &str) -> BoxFuture<'_, anyhow::Result<()>>;

    /// Broadcast a typed event to all connected clients and persist to history.
    fn emit_event(
        &self,
        event_type: EventType,
        content: &str,
        params: Option<serde_json::Value>,
    ) -> BoxFuture<'_, anyhow::Result<()>>;
}

// ── HistoryAccess trait ─────────────────────────────────────────────────────

/// Async read-only access to a room's chat history.
///
/// Respects DM visibility — a plugin invoked by user X will not see DMs
/// between Y and Z.
pub trait HistoryAccess: Send + Sync {
    /// Load all messages (filtered by DM visibility).
    fn all(&self) -> BoxFuture<'_, anyhow::Result<Vec<Message>>>;

    /// Load the last `n` messages (filtered by DM visibility).
    fn tail(&self, n: usize) -> BoxFuture<'_, anyhow::Result<Vec<Message>>>;

    /// Load messages after the one with the given ID (filtered by DM visibility).
    fn since(&self, message_id: &str) -> BoxFuture<'_, anyhow::Result<Vec<Message>>>;

    /// Count total messages in the chat.
    fn count(&self) -> BoxFuture<'_, anyhow::Result<usize>>;
}

// ── TeamAccess trait ────────────────────────────────────────────────────────

/// Read-only access to daemon-level team membership.
///
/// Plugins use this trait to check whether a user belongs to a team without
/// depending on `room-daemon` or `UserRegistry` directly. The broker provides
/// a concrete implementation backed by the registry; standalone mode passes
/// `None` (no team checking available).
pub trait TeamAccess: Send + Sync {
    /// Returns `true` if the named team exists in the registry.
    fn team_exists(&self, team: &str) -> bool;

    /// Returns `true` if `user` is a member of `team`.
    fn is_member(&self, team: &str, user: &str) -> bool;
}

// ── RoomMetadata ────────────────────────────────────────────────────────────

/// Frozen snapshot of room state for plugin consumption.
pub struct RoomMetadata {
    /// Users currently online with their status.
    pub online_users: Vec<UserInfo>,
    /// Username of the room host.
    pub host: Option<String>,
    /// Total messages in the chat file.
    pub message_count: usize,
}

/// A user's online presence.
pub struct UserInfo {
    pub username: String,
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_type_choice_equality() {
        let a = ParamType::Choice(vec!["x".to_owned(), "y".to_owned()]);
        let b = ParamType::Choice(vec!["x".to_owned(), "y".to_owned()]);
        assert_eq!(a, b);
        let c = ParamType::Choice(vec!["x".to_owned()]);
        assert_ne!(a, c);
    }

    #[test]
    fn param_type_number_equality() {
        let a = ParamType::Number {
            min: Some(1),
            max: Some(100),
        };
        let b = ParamType::Number {
            min: Some(1),
            max: Some(100),
        };
        assert_eq!(a, b);
        let c = ParamType::Number {
            min: None,
            max: None,
        };
        assert_ne!(a, c);
    }

    #[test]
    fn param_type_variants_are_distinct() {
        assert_ne!(ParamType::Text, ParamType::Username);
        assert_ne!(
            ParamType::Text,
            ParamType::Number {
                min: None,
                max: None
            }
        );
        assert_ne!(ParamType::Text, ParamType::Choice(vec!["a".to_owned()]));
    }

    // ── Versioning defaults ─────────────────────────────────────────────

    struct DefaultsPlugin;

    impl Plugin for DefaultsPlugin {
        fn name(&self) -> &str {
            "defaults"
        }

        fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
            Box::pin(async { Ok(PluginResult::Handled) })
        }
    }

    #[test]
    fn default_version_is_zero() {
        assert_eq!(DefaultsPlugin.version(), "0.0.0");
    }

    #[test]
    fn default_api_version_is_one() {
        assert_eq!(DefaultsPlugin.api_version(), 1);
    }

    #[test]
    fn default_min_protocol_is_zero() {
        assert_eq!(DefaultsPlugin.min_protocol(), "0.0.0");
    }

    #[test]
    fn plugin_api_version_const_is_one() {
        assert_eq!(PLUGIN_API_VERSION, 1);
    }

    #[test]
    fn protocol_version_const_matches_cargo() {
        // PROTOCOL_VERSION is set at compile time via env!("CARGO_PKG_VERSION").
        // It must be a non-empty semver string with at least major.minor.patch.
        assert!(!PROTOCOL_VERSION.is_empty());
        let parts: Vec<&str> = PROTOCOL_VERSION.split('.').collect();
        assert!(
            parts.len() >= 3,
            "PROTOCOL_VERSION must be major.minor.patch, got: {PROTOCOL_VERSION}"
        );
        for part in &parts {
            assert!(
                part.parse::<u64>().is_ok(),
                "each segment must be numeric, got: {part}"
            );
        }
    }

    struct VersionedPlugin;

    impl Plugin for VersionedPlugin {
        fn name(&self) -> &str {
            "versioned"
        }

        fn version(&self) -> &str {
            "2.5.1"
        }

        fn api_version(&self) -> u32 {
            1
        }

        fn min_protocol(&self) -> &str {
            "3.0.0"
        }

        fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
            Box::pin(async { Ok(PluginResult::Handled) })
        }
    }

    #[test]
    fn custom_version_methods_override_defaults() {
        assert_eq!(VersionedPlugin.version(), "2.5.1");
        assert_eq!(VersionedPlugin.api_version(), 1);
        assert_eq!(VersionedPlugin.min_protocol(), "3.0.0");
    }
}
