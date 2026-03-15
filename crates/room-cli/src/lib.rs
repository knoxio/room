// Modules that live in room-daemon — re-exported for backward compatibility.
pub use room_daemon::{broker, history, paths, plugin, query, registry};

// Modules that remain in room-cli.
pub mod client;
pub mod message;
pub mod oneshot;
pub mod plugin_cmd;
pub mod tui;
pub mod upgrade;
