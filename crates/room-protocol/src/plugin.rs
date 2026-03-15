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

    /// Called after every message is broadcast to the room. The default is a
    /// no-op.
    ///
    /// Plugins can use this to observe message flow (e.g. tracking agent
    /// activity for stale detection). Invoked synchronously after
    /// `broadcast_and_persist` — implementations must not block.
    fn on_message(&self, _msg: &Message) {}
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

// ── C ABI for dynamic plugin loading ──────────────────────────────────────

/// Types and conventions for loading plugins from `cdylib` shared libraries.
///
/// Each plugin cdylib exports two symbols:
/// - [`DECLARATION_SYMBOL`]: a static [`PluginDeclaration`] with metadata
/// - [`CREATE_SYMBOL`]: a [`CreateFn`] that constructs the plugin
///
/// The loader reads the declaration first to check API/protocol compatibility,
/// then calls the create function to obtain a `Box<dyn Plugin>`.
pub mod abi {
    use super::Plugin;

    /// Null-terminated symbol name for the [`PluginDeclaration`] static.
    pub const DECLARATION_SYMBOL: &[u8] = b"ROOM_PLUGIN_DECLARATION\0";

    /// Null-terminated symbol name for the [`CreateFn`] function.
    pub const CREATE_SYMBOL: &[u8] = b"room_plugin_create\0";

    /// Null-terminated symbol name for the [`DestroyFn`] function.
    pub const DESTROY_SYMBOL: &[u8] = b"room_plugin_destroy\0";

    /// C-compatible plugin metadata exported as a `#[no_mangle]` static from
    /// each cdylib plugin.
    ///
    /// The loader reads this before calling [`CreateFn`] to verify that the
    /// plugin's API version and protocol requirements are compatible with the
    /// running broker.
    ///
    /// Use [`PluginDeclaration::new`] to construct in a `static` context.
    #[repr(C)]
    pub struct PluginDeclaration {
        /// Must equal [`super::PLUGIN_API_VERSION`] for the plugin to load.
        pub api_version: u32,
        /// Pointer to the plugin name string (UTF-8, not necessarily null-terminated).
        pub name_ptr: *const u8,
        /// Length of the plugin name string in bytes.
        pub name_len: usize,
        /// Pointer to the plugin version string (semver, UTF-8).
        pub version_ptr: *const u8,
        /// Length of the plugin version string in bytes.
        pub version_len: usize,
        /// Pointer to the minimum room-protocol version string (semver, UTF-8).
        pub min_protocol_ptr: *const u8,
        /// Length of the minimum protocol version string in bytes.
        pub min_protocol_len: usize,
    }

    // SAFETY: PluginDeclaration contains only raw pointers to static data and
    // plain integers — no interior mutability, no heap allocation. The pointed-to
    // data lives for `'static` (string literals or env!() constants).
    unsafe impl Send for PluginDeclaration {}
    unsafe impl Sync for PluginDeclaration {}

    impl PluginDeclaration {
        /// Construct a declaration from static string slices. All arguments must
        /// be `'static` — this is enforced by the function signature and is
        /// required because the declaration is stored as a `static`.
        pub const fn new(
            api_version: u32,
            name: &'static str,
            version: &'static str,
            min_protocol: &'static str,
        ) -> Self {
            Self {
                api_version,
                name_ptr: name.as_ptr(),
                name_len: name.len(),
                version_ptr: version.as_ptr(),
                version_len: version.len(),
                min_protocol_ptr: min_protocol.as_ptr(),
                min_protocol_len: min_protocol.len(),
            }
        }

        /// Reconstruct the plugin name.
        ///
        /// Returns `Err` if the bytes are not valid UTF-8.
        ///
        /// # Safety
        ///
        /// The declaration must still be valid — i.e. the shared library that
        /// exported it must not have been unloaded, and the pointer/length pair
        /// must point to a valid byte slice.
        pub unsafe fn name(&self) -> Result<&str, core::str::Utf8Error> {
            core::str::from_utf8(core::slice::from_raw_parts(self.name_ptr, self.name_len))
        }

        /// Reconstruct the plugin version string.
        ///
        /// Returns `Err` if the bytes are not valid UTF-8.
        ///
        /// # Safety
        ///
        /// Same as [`name`](Self::name).
        pub unsafe fn version(&self) -> Result<&str, core::str::Utf8Error> {
            core::str::from_utf8(core::slice::from_raw_parts(
                self.version_ptr,
                self.version_len,
            ))
        }

        /// Reconstruct the minimum protocol version string.
        ///
        /// Returns `Err` if the bytes are not valid UTF-8.
        ///
        /// # Safety
        ///
        /// Same as [`name`](Self::name).
        pub unsafe fn min_protocol(&self) -> Result<&str, core::str::Utf8Error> {
            core::str::from_utf8(core::slice::from_raw_parts(
                self.min_protocol_ptr,
                self.min_protocol_len,
            ))
        }
    }

    /// Type signature for the plugin creation function exported by cdylib plugins.
    ///
    /// The function receives a UTF-8 JSON configuration string (pointer + length)
    /// and returns a double-boxed `Plugin` trait object. The outer `Box` yields a
    /// thin pointer (C-ABI safe); the inner `Box<dyn Plugin>` is a fat pointer
    /// stored on the heap.
    ///
    /// # Arguments
    ///
    /// * `config_json` — pointer to a UTF-8 JSON string, or null for default config
    /// * `config_len` — length of the config string in bytes (0 if null)
    ///
    /// # Returns
    ///
    /// A thin pointer to a heap-allocated `Box<dyn Plugin>`. The caller takes
    /// ownership and must free it via [`DestroyFn`] or
    /// `drop(Box::from_raw(ptr))`.
    ///
    /// # Safety
    ///
    /// * If `config_json` is non-null, it must be valid for reads of `config_len` bytes
    /// * The returned pointer must not be null
    pub type CreateFn =
        unsafe extern "C" fn(config_json: *const u8, config_len: usize) -> *mut Box<dyn Plugin>;

    /// Type signature for the plugin destruction function exported by cdylib plugins.
    ///
    /// Frees a plugin previously returned by [`CreateFn`]. The loader calls this
    /// during shutdown or when unloading a plugin.
    ///
    /// # Safety
    ///
    /// * `plugin` must have been returned by [`CreateFn`] from the same library
    /// * Must not be called more than once on the same pointer
    pub type DestroyFn = unsafe extern "C" fn(plugin: *mut Box<dyn Plugin>);

    /// Helper to extract a `&str` config from raw FFI pointers.
    ///
    /// Returns an empty string if the pointer is null or the length is zero.
    /// Panics if the bytes are not valid UTF-8.
    ///
    /// # Safety
    ///
    /// If `ptr` is non-null, it must be valid for reads of `len` bytes.
    pub unsafe fn config_from_raw(ptr: *const u8, len: usize) -> &'static str {
        if ptr.is_null() || len == 0 {
            ""
        } else {
            let bytes = core::slice::from_raw_parts(ptr, len);
            core::str::from_utf8(bytes).expect("plugin config is not valid UTF-8")
        }
    }
}

/// Declares the C ABI entry points for a cdylib plugin.
///
/// Generates three `#[no_mangle]` exports:
/// - `ROOM_PLUGIN_DECLARATION` — a [`abi::PluginDeclaration`] static
/// - `room_plugin_create` — calls the provided closure with a `&str` config
///   and returns a double-boxed `dyn Plugin`
/// - `room_plugin_destroy` — frees a plugin returned by `room_plugin_create`
///
/// # Arguments
///
/// * `$name` — plugin name as a string literal (e.g. `"taskboard"`)
/// * `$create` — an expression that takes `config: &str` and returns
///   `impl Plugin` (e.g. a closure or function call)
///
/// # Example
///
/// ```ignore
/// use room_protocol::declare_plugin;
///
/// declare_plugin!("my-plugin", |config: &str| {
///     MyPlugin::from_config(config)
/// });
/// ```
#[macro_export]
macro_rules! declare_plugin {
    ($name:expr, $create:expr) => {
        /// Plugin metadata for dynamic loading.
        ///
        /// When the `cdylib-exports` feature is enabled, this static is exported
        /// with `#[no_mangle]` so that `libloading` can find it by name. When
        /// the feature is off (rlib / static linking), the symbol is mangled to
        /// avoid collisions with other plugins in the same binary.
        #[cfg_attr(feature = "cdylib-exports", no_mangle)]
        pub static ROOM_PLUGIN_DECLARATION: $crate::plugin::abi::PluginDeclaration =
            $crate::plugin::abi::PluginDeclaration::new(
                $crate::plugin::PLUGIN_API_VERSION,
                $name,
                env!("CARGO_PKG_VERSION"),
                "0.0.0",
            );

        /// # Safety
        ///
        /// See [`room_protocol::plugin::abi::CreateFn`] for safety contract.
        #[cfg_attr(feature = "cdylib-exports", no_mangle)]
        pub unsafe extern "C" fn room_plugin_create(
            config_json: *const u8,
            config_len: usize,
        ) -> *mut Box<dyn $crate::plugin::Plugin> {
            let config = unsafe { $crate::plugin::abi::config_from_raw(config_json, config_len) };
            let create_fn = $create;
            let plugin: Box<dyn $crate::plugin::Plugin> = Box::new(create_fn(config));
            Box::into_raw(Box::new(plugin))
        }

        /// # Safety
        ///
        /// See [`room_protocol::plugin::abi::DestroyFn`] for safety contract.
        #[cfg_attr(feature = "cdylib-exports", no_mangle)]
        pub unsafe extern "C" fn room_plugin_destroy(plugin: *mut Box<dyn $crate::plugin::Plugin>) {
            if !plugin.is_null() {
                drop(unsafe { Box::from_raw(plugin) });
            }
        }
    };
}

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

    // ── ABI types ──────────────────────────────────────────────────────────

    #[test]
    fn declaration_new_stores_correct_values() {
        let decl = abi::PluginDeclaration::new(1, "test-plugin", "1.2.3", "3.0.0");
        assert_eq!(decl.api_version, 1);
        unsafe {
            assert_eq!(decl.name().unwrap(), "test-plugin");
            assert_eq!(decl.version().unwrap(), "1.2.3");
            assert_eq!(decl.min_protocol().unwrap(), "3.0.0");
        }
    }

    #[test]
    fn declaration_with_empty_strings() {
        let decl = abi::PluginDeclaration::new(0, "", "", "");
        assert_eq!(decl.api_version, 0);
        assert_eq!(decl.name_len, 0);
        assert_eq!(decl.version_len, 0);
        assert_eq!(decl.min_protocol_len, 0);
    }

    #[test]
    fn declaration_is_repr_c_sized() {
        // PluginDeclaration must have a stable, known size for FFI.
        // On 64-bit: u32(4) + padding(4) + 3*(ptr+usize) = 4+4+48 = 56 bytes
        let size = std::mem::size_of::<abi::PluginDeclaration>();
        assert!(size > 0, "PluginDeclaration must have non-zero size");
        // Alignment must be pointer-aligned for C compatibility.
        let align = std::mem::align_of::<abi::PluginDeclaration>();
        assert!(
            align >= std::mem::align_of::<usize>(),
            "PluginDeclaration must be at least pointer-aligned"
        );
    }

    #[test]
    fn config_from_raw_null_returns_empty() {
        let result = unsafe { abi::config_from_raw(std::ptr::null(), 0) };
        assert_eq!(result, "");
    }

    #[test]
    fn config_from_raw_zero_len_returns_empty() {
        let data = b"some data";
        let result = unsafe { abi::config_from_raw(data.as_ptr(), 0) };
        assert_eq!(result, "");
    }

    #[test]
    fn config_from_raw_valid_data() {
        let json = b"{\"path\":\"/tmp\"}";
        let result = unsafe { abi::config_from_raw(json.as_ptr(), json.len()) };
        assert_eq!(result, "{\"path\":\"/tmp\"}");
    }

    #[test]
    fn symbol_names_are_null_terminated() {
        assert!(abi::DECLARATION_SYMBOL.ends_with(b"\0"));
        assert!(abi::CREATE_SYMBOL.ends_with(b"\0"));
        assert!(abi::DESTROY_SYMBOL.ends_with(b"\0"));
    }

    #[test]
    fn create_fn_type_is_c_abi() {
        // Verify CreateFn can be stored in a function pointer variable.
        // This is a compile-time check — if the type is invalid, it won't compile.
        let _: Option<abi::CreateFn> = None;
        let _: Option<abi::DestroyFn> = None;
    }
}
