//! Hive server configuration.
//!
//! Loaded from `hive.toml` (or path specified via `--config`). Falls back to
//! sensible defaults when the file is missing.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Top-level Hive server configuration.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct HiveConfig {
    /// HTTP server settings.
    pub server: ServerConfig,
    /// Room daemon connection settings.
    pub daemon: DaemonConfig,
}

/// HTTP server bind configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Address to bind the HTTP server to.
    pub host: String,
    /// Port for the HTTP server.
    pub port: u16,
    /// Directory for persistent data (SQLite database, etc.).
    pub data_dir: String,
    /// Allowed CORS origins. Overridden by `HIVE_CORS_ORIGINS` (comma-separated).
    /// Defaults to `["http://localhost:5173"]`.
    pub cors_origins: Vec<String>,
}

/// Room daemon connection configuration.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Path to the room daemon Unix socket.
    pub socket_path: PathBuf,
    /// WebSocket URL of the room daemon (e.g. ws://127.0.0.1:4200).
    pub ws_url: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let host = std::env::var("HIVE_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
        let port = std::env::var("HIVE_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(3000);
        let data_dir = std::env::var("HIVE_DATA_DIR").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
            format!("{home}/.hive/data")
        });
        let cors_origins = std::env::var("HIVE_CORS_ORIGINS")
            .map(|s| s.split(',').map(|o| o.trim().to_owned()).collect())
            .unwrap_or_else(|_| vec!["http://localhost:5173".to_owned()]);
        Self {
            host,
            port,
            data_dir,
            cors_origins,
        }
    }
}

/// Returns `true` when `HIVE_ENV=production` is set.
pub fn is_production() -> bool {
    std::env::var("HIVE_ENV")
        .map(|v| v == "production")
        .unwrap_or(false)
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from("/tmp/roomd.sock"),
            ws_url: "ws://127.0.0.1:4200".to_owned(),
        }
    }
}

/// Load configuration from a TOML file, falling back to defaults.
pub fn load_config(path: &Path) -> HiveConfig {
    match std::fs::read_to_string(path) {
        Ok(content) => toml::from_str(&content).unwrap_or_else(|e| {
            eprintln!(
                "[hive] warning: invalid config {}: {e} — using defaults",
                path.display()
            );
            HiveConfig::default()
        }),
        Err(_) => {
            eprintln!("[hive] no config at {} — using defaults", path.display());
            HiveConfig::default()
        }
    }
}
