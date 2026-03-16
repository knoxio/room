# US-BE-002: Configuration loading

**As a** server operator
**I want to** configure the Hive server via a `hive.toml` file
**So that** I can set the room daemon socket path, HTTP port, and data directory without recompiling

## Acceptance Criteria
- [ ] Server reads `hive.toml` from the current working directory on startup, or from the path specified by `--config <path>` CLI flag
- [ ] Config file specifies at minimum: `http_port`, `room_socket_path`, `data_dir`
- [ ] Individual fields can be overridden by environment variables (`HIVE_HTTP_PORT`, `HIVE_ROOM_SOCKET_PATH`, `HIVE_DATA_DIR`)
- [ ] Server fails fast with a descriptive error if the config file is missing and no defaults are applicable
- [ ] Missing optional fields fall back to documented defaults (port 8080, socket `/tmp/roomd.sock`, data dir `~/.hive`)
- [ ] Config is loaded once at startup and stored in a shared `Arc<Config>` accessible to all handlers
- [ ] A `--print-config` flag prints the resolved config as TOML and exits without starting the server

## Technical Notes
- Implement in `crates/hive-server/src/config.rs`
- Use the `toml` crate for deserialization into a `HiveConfig` struct derived with `serde::Deserialize`
- Environment variable override layer sits on top of file deserialization; use a simple manual merge rather than a config framework to keep dependencies minimal
- `data_dir` supports `~` expansion (resolve via `dirs::home_dir()` or manual replacement)
- Validation: `http_port` must be 1–65535, `room_socket_path` must be an absolute path

## Phase
Phase 1 (Skeleton + Room Proxy)
