# US-BE-023: SQLite database setup

**As a** Hive server
**I want to** initialise and migrate the SQLite database schema on startup
**So that** Hive-owned state (users, workspaces, teams, agents) is persisted across restarts without manual setup

## Acceptance Criteria
- [ ] On startup, the server creates the SQLite database file at `config.data_dir/hive.db` if it does not exist
- [ ] Schema migrations are run automatically using an embedded migration runner; the server does not start if migrations fail
- [ ] Migrations are versioned and applied in order; already-applied migrations are skipped
- [ ] Schema includes all tables required by other stories: `users`, `api_keys`, `workspaces`, `workspace_rooms`, `workspace_members`, `team_manifests`, `agents`, `provision_jobs`, `provision_job_agents`
- [ ] Foreign key constraints are enabled per connection: `PRAGMA foreign_keys = ON`
- [ ] WAL mode is enabled: `PRAGMA journal_mode = WAL` for concurrent read performance
- [ ] The database file is created with permissions `0600`

## Technical Notes
- Implement in `crates/hive-server/src/db.rs`
- Use `rusqlite` with `bundled` feature to avoid a system SQLite dependency
- Migration runner: embed SQL files as strings via `include_str!` macros; track applied migrations in a `_migrations(version INT, applied_at TEXT)` table created on first run
- Full schema:
  - `users(id TEXT PK, github_id INT UNIQUE, github_login TEXT, email TEXT, room_token TEXT, created_at TEXT)`
  - `api_keys(id TEXT PK, user_id TEXT FK, key_hash TEXT UNIQUE, scopes TEXT, created_at TEXT, revoked_at TEXT)`
  - `workspaces(id TEXT PK, name TEXT, owner_id TEXT FK, created_at TEXT)`
  - `workspace_rooms(workspace_id TEXT FK, room_id TEXT, PRIMARY KEY(workspace_id, room_id))`
  - `workspace_members(workspace_id TEXT FK, user_id TEXT FK, role TEXT, joined_at TEXT, PRIMARY KEY(workspace_id, user_id))`
  - `team_manifests(id TEXT PK, workspace_id TEXT FK, name TEXT, manifest_json TEXT, created_at TEXT)`
  - `agents(id TEXT PK, workspace_id TEXT FK, room_id TEXT, personality TEXT, model TEXT, status TEXT, pid INT, started_at TEXT, stopped_at TEXT, log_path TEXT)`
  - `provision_jobs(id TEXT PK, workspace_id TEXT FK, manifest_id TEXT FK, status TEXT, created_at TEXT)`
  - `provision_job_agents(job_id TEXT FK, agent_id TEXT FK, status TEXT, error TEXT)`
- A `DbPool` wrapper holds a `Mutex<Connection>` for single-writer access; for Phase 3 upgrade to `r2d2` connection pool if contention becomes an issue

## Phase
Cross-cutting
