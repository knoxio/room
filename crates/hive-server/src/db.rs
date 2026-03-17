//! SQLite database setup and migration for Hive.
//!
//! Manages the Hive-specific state: users, workspaces, workspace-room
//! mappings, API keys, and team manifests. Room-side data (messages,
//! tokens, subscriptions) remains in room's own storage.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

/// Schema version — bump when adding new migrations.
const SCHEMA_VERSION: i64 = 1;

/// SQL statements for schema v1.
const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    provider    TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    email       TEXT,
    display_name TEXT,
    room_token  TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(provider, provider_id)
);

CREATE TABLE IF NOT EXISTS workspaces (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    owner_id    INTEGER NOT NULL REFERENCES users(id),
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS workspace_rooms (
    workspace_id INTEGER NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    room_id      TEXT NOT NULL,
    added_at     TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (workspace_id, room_id)
);

CREATE TABLE IF NOT EXISTS api_keys (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    key_hash    TEXT NOT NULL UNIQUE,
    label       TEXT,
    scopes      TEXT NOT NULL DEFAULT '*',
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at  TEXT
);

CREATE TABLE IF NOT EXISTS team_manifests (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id  INTEGER NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    manifest_json TEXT NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

/// Thread-safe database handle.
///
/// Uses a `Mutex<Connection>` for synchronous SQLite access from async
/// handlers via `spawn_blocking`. SQLite in WAL mode supports concurrent
/// readers but single writer — the mutex serializes writes.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open (or create) the database at `path` and run migrations.
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory database (for tests).
    pub fn open_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Run schema migrations up to `SCHEMA_VERSION`.
    fn migrate(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().expect("db lock poisoned");

        // Create migration tracking table
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )?;

        let current: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM _migrations",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if current < 1 {
            conn.execute_batch(SCHEMA_V1)?;
            conn.execute("INSERT INTO _migrations (version) VALUES (1)", [])?;
            tracing::info!("database migrated to schema v1");
        }

        let final_version: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM _migrations",
            [],
            |row| row.get(0),
        )?;
        tracing::info!("database schema at v{final_version} (target: v{SCHEMA_VERSION})");

        Ok(())
    }

    /// Execute a closure with the database connection.
    ///
    /// Callers should keep the closure short to avoid holding the mutex.
    pub fn with_conn<F, T>(&self, f: F) -> Result<T, rusqlite::Error>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error>,
    {
        let conn = self.conn.lock().expect("db lock poisoned");
        f(&conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_memory_creates_tables() {
        let db = Database::open_memory().unwrap();
        db.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
            let names: Vec<String> = stmt
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            assert!(names.contains(&"users".to_owned()));
            assert!(names.contains(&"workspaces".to_owned()));
            assert!(names.contains(&"workspace_rooms".to_owned()));
            assert!(names.contains(&"api_keys".to_owned()));
            assert!(names.contains(&"team_manifests".to_owned()));
            assert!(names.contains(&"_migrations".to_owned()));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migration_is_idempotent() {
        let db = Database::open_memory().unwrap();
        // Running migrate again should not fail
        db.migrate().unwrap();
        db.migrate().unwrap();

        db.with_conn(|conn| {
            let version: i64 =
                conn.query_row("SELECT MAX(version) FROM _migrations", [], |row| row.get(0))?;
            assert_eq!(version, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn foreign_keys_enforced() {
        let db = Database::open_memory().unwrap();
        let result = db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workspaces (name, owner_id) VALUES ('test', 999)",
                [],
            )
        });
        // Should fail — owner_id 999 doesn't exist in users
        assert!(result.is_err());
    }

    #[test]
    fn insert_user_and_workspace() {
        let db = Database::open_memory().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO users (provider, provider_id, email, display_name) VALUES ('github', '12345', 'test@example.com', 'Test User')",
                [],
            )?;
            let user_id: i64 = conn.query_row(
                "SELECT id FROM users WHERE provider_id = '12345'",
                [],
                |row| row.get(0),
            )?;

            conn.execute(
                "INSERT INTO workspaces (name, owner_id) VALUES ('my-workspace', ?1)",
                [user_id],
            )?;
            let ws_name: String = conn.query_row(
                "SELECT name FROM workspaces WHERE owner_id = ?1",
                [user_id],
                |row| row.get(0),
            )?;
            assert_eq!(ws_name, "my-workspace");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn workspace_room_membership() {
        let db = Database::open_memory().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO users (provider, provider_id) VALUES ('github', '1')",
                [],
            )?;
            conn.execute(
                "INSERT INTO workspaces (name, owner_id) VALUES ('ws', 1)",
                [],
            )?;
            conn.execute(
                "INSERT INTO workspace_rooms (workspace_id, room_id) VALUES (1, 'room-dev')",
                [],
            )?;
            conn.execute(
                "INSERT INTO workspace_rooms (workspace_id, room_id) VALUES (1, 'room-staging')",
                [],
            )?;

            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM workspace_rooms WHERE workspace_id = 1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn cascade_delete_workspace_removes_rooms() {
        let db = Database::open_memory().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO users (provider, provider_id) VALUES ('github', '1')",
                [],
            )?;
            conn.execute(
                "INSERT INTO workspaces (name, owner_id) VALUES ('ws', 1)",
                [],
            )?;
            conn.execute(
                "INSERT INTO workspace_rooms (workspace_id, room_id) VALUES (1, 'room-a')",
                [],
            )?;

            // Delete workspace — should cascade to workspace_rooms
            conn.execute("DELETE FROM workspaces WHERE id = 1", [])?;

            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM workspace_rooms WHERE workspace_id = 1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn unique_user_constraint() {
        let db = Database::open_memory().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO users (provider, provider_id) VALUES ('github', '1')",
                [],
            )?;
            let result = conn.execute(
                "INSERT INTO users (provider, provider_id) VALUES ('github', '1')",
                [],
            );
            assert!(result.is_err());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn open_file_database() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("hive.db");
        let db = Database::open(&db_path).unwrap();

        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO users (provider, provider_id) VALUES ('test', '1')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        // Reopen — data should persist
        let db2 = Database::open(&db_path).unwrap();
        db2.with_conn(|conn| {
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
            assert_eq!(count, 1);
            Ok(())
        })
        .unwrap();
    }
}
