//! Persistent user registry for cross-room identity.
//!
//! [`UserRegistry`] provides daemon-level user management: registration,
//! token issuance/validation, room membership tracking, and global status.
//! Data is persisted as JSON in a configurable data directory.
//!
//! This module is standalone — it does not depend on broker internals.
//! The daemon (`roomd`, #251) wraps it in `Arc<Mutex<_>>` for concurrent access.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use uuid::Uuid;

/// A registered user with cross-room identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct User {
    pub username: String,
    pub created_at: DateTime<Utc>,
    pub rooms: HashSet<String>,
    pub status: Option<String>,
}

/// Persistent storage format — serialized to `users.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RegistryData {
    users: HashMap<String, User>,
    /// Maps token UUID string → username.
    tokens: HashMap<String, String>,
}

/// Daemon-level user registry with persistent storage.
///
/// Manages user lifecycle, token auth, room membership, and global status.
/// All mutations auto-save to `{data_dir}/users.json`.
#[derive(Debug)]
pub struct UserRegistry {
    data: RegistryData,
    data_dir: PathBuf,
}

const REGISTRY_FILE: &str = "users.json";

impl UserRegistry {
    /// Create a new empty registry backed by the given directory.
    ///
    /// Does **not** load from disk — use [`UserRegistry::load`] for that.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data: RegistryData::default(),
            data_dir,
        }
    }

    /// Load an existing registry from `{data_dir}/users.json`.
    ///
    /// Returns a fresh empty registry if the file does not exist.
    /// Returns an error only if the file exists but cannot be parsed.
    pub fn load(data_dir: PathBuf) -> Result<Self, String> {
        let path = data_dir.join(REGISTRY_FILE);
        if !path.exists() {
            return Ok(Self::new(data_dir));
        }
        let contents =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let data: RegistryData = serde_json::from_str(&contents)
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
        Ok(Self { data, data_dir })
    }

    /// Persist the registry to `{data_dir}/users.json`.
    pub fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.data_dir)
            .map_err(|e| format!("create dir {}: {e}", self.data_dir.display()))?;
        let path = self.data_dir.join(REGISTRY_FILE);
        let json = serde_json::to_string_pretty(&self.data)
            .map_err(|e| format!("serialize registry: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
    }

    // ── User CRUD ──────────────────────────────────────────────────

    /// Register a new user. Fails if the username is already taken.
    pub fn register_user(&mut self, username: &str) -> Result<&User, String> {
        if username.is_empty() {
            return Err("username cannot be empty".into());
        }
        if self.data.users.contains_key(username) {
            return Err(format!("username already registered: {username}"));
        }
        let user = User {
            username: username.to_owned(),
            created_at: Utc::now(),
            rooms: HashSet::new(),
            status: None,
        };
        self.data.users.insert(username.to_owned(), user);
        self.save()?;
        Ok(self.data.users.get(username).unwrap())
    }

    /// Remove a user and all their tokens.
    ///
    /// Returns `true` if the user existed.
    pub fn remove_user(&mut self, username: &str) -> Result<bool, String> {
        let existed = self.data.users.remove(username).is_some();
        if existed {
            self.data.tokens.retain(|_, u| u != username);
            self.save()?;
        }
        Ok(existed)
    }

    /// Look up a user by username.
    pub fn get_user(&self, username: &str) -> Option<&User> {
        self.data.users.get(username)
    }

    /// List all registered users.
    pub fn list_users(&self) -> Vec<&User> {
        self.data.users.values().collect()
    }

    // ── Token auth ─────────────────────────────────────────────────

    /// Issue a new token for a registered user.
    ///
    /// The user must already be registered via [`register_user`].
    pub fn issue_token(&mut self, username: &str) -> Result<String, String> {
        if !self.data.users.contains_key(username) {
            return Err(format!("user not registered: {username}"));
        }
        let token = Uuid::new_v4().to_string();
        self.data.tokens.insert(token.clone(), username.to_owned());
        self.save()?;
        Ok(token)
    }

    /// Validate a token, returning the associated username.
    pub fn validate_token(&self, token: &str) -> Option<&str> {
        self.data.tokens.get(token).map(|s| s.as_str())
    }

    /// Revoke a specific token. Returns `true` if it existed.
    pub fn revoke_token(&mut self, token: &str) -> Result<bool, String> {
        let existed = self.data.tokens.remove(token).is_some();
        if existed {
            self.save()?;
        }
        Ok(existed)
    }

    /// Revoke all tokens for a user. Returns the number revoked.
    pub fn revoke_user_tokens(&mut self, username: &str) -> Result<usize, String> {
        let before = self.data.tokens.len();
        self.data.tokens.retain(|_, u| u != username);
        let revoked = before - self.data.tokens.len();
        if revoked > 0 {
            self.save()?;
        }
        Ok(revoked)
    }

    // ── Room membership ────────────────────────────────────────────

    /// Record that a user has joined a room.
    pub fn join_room(&mut self, username: &str, room_id: &str) -> Result<(), String> {
        let user = self
            .data
            .users
            .get_mut(username)
            .ok_or_else(|| format!("user not registered: {username}"))?;
        user.rooms.insert(room_id.to_owned());
        self.save()
    }

    /// Record that a user has left a room.
    pub fn leave_room(&mut self, username: &str, room_id: &str) -> Result<bool, String> {
        let user = self
            .data
            .users
            .get_mut(username)
            .ok_or_else(|| format!("user not registered: {username}"))?;
        let was_member = user.rooms.remove(room_id);
        if was_member {
            self.save()?;
        }
        Ok(was_member)
    }

    // ── Status ─────────────────────────────────────────────────────

    /// Set or clear a user's global status.
    ///
    /// Pass `None` to clear. Status applies across all rooms the user is in.
    pub fn set_status(&mut self, username: &str, status: Option<String>) -> Result<(), String> {
        let user = self
            .data
            .users
            .get_mut(username)
            .ok_or_else(|| format!("user not registered: {username}"))?;
        user.status = status;
        self.save()
    }

    /// Return the path to the backing JSON file.
    pub fn data_path(&self) -> PathBuf {
        self.data_dir.join(REGISTRY_FILE)
    }

    /// Return `true` if any token is currently associated with `username`.
    ///
    /// Used by daemon auth to detect username collisions without scanning the
    /// entire token map externally.
    pub fn has_token_for_user(&self, username: &str) -> bool {
        self.data.tokens.values().any(|u| u == username)
    }

    /// Register a user if not already registered; no-op if already present.
    ///
    /// Unlike [`register_user`], this is idempotent — calling it for an
    /// existing user does not return an error. Used by daemon auth so that
    /// users from a previous session (loaded from `users.json`) can rejoin
    /// without triggering a registration error.
    pub fn register_user_idempotent(&mut self, username: &str) -> Result<(), String> {
        if self.data.users.contains_key(username) {
            return Ok(());
        }
        self.register_user(username)?;
        Ok(())
    }

    /// Return a snapshot of all current token → username mappings.
    ///
    /// Used at daemon startup to seed the in-memory `TokenMap` from persisted
    /// registry data so existing tokens remain valid without a fresh join.
    pub fn token_snapshot(&self) -> std::collections::HashMap<String, String> {
        self.data.tokens.clone()
    }

    /// Insert a pre-existing token UUID for a registered user.
    ///
    /// Unlike [`issue_token`], which generates a fresh UUID, this method
    /// preserves the caller-supplied `token` string. It is intended for
    /// migration paths that read legacy token files (e.g. `/tmp/room-*-*.token`)
    /// and want existing clients to remain valid without a forced re-join.
    ///
    /// Returns `Ok(())` immediately if the token is already present in the
    /// registry (idempotent). Returns an error if `username` is not registered.
    pub fn import_token(&mut self, username: &str, token: &str) -> Result<(), String> {
        if !self.data.users.contains_key(username) {
            return Err(format!("user not registered: {username}"));
        }
        if self.data.tokens.contains_key(token) {
            return Ok(());
        }
        self.data
            .tokens
            .insert(token.to_owned(), username.to_owned());
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_registry() -> (UserRegistry, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let reg = UserRegistry::new(dir.path().to_owned());
        (reg, dir)
    }

    // ── User CRUD ──────────────────────────────────────────────────

    #[test]
    fn register_and_get_user() {
        let (mut reg, _dir) = tmp_registry();
        let user = reg.register_user("alice").unwrap();
        assert_eq!(user.username, "alice");
        assert!(user.rooms.is_empty());
        assert!(user.status.is_none());

        let fetched = reg.get_user("alice").unwrap();
        assert_eq!(fetched.username, "alice");
    }

    #[test]
    fn register_duplicate_rejected() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        let err = reg.register_user("alice").unwrap_err();
        assert!(err.contains("already registered"));
    }

    #[test]
    fn register_empty_username_rejected() {
        let (mut reg, _dir) = tmp_registry();
        let err = reg.register_user("").unwrap_err();
        assert!(err.contains("cannot be empty"));
    }

    #[test]
    fn remove_user_cleans_tokens() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        let token = reg.issue_token("alice").unwrap();
        assert!(reg.validate_token(&token).is_some());

        reg.remove_user("alice").unwrap();
        assert!(reg.get_user("alice").is_none());
        assert!(reg.validate_token(&token).is_none());
    }

    #[test]
    fn remove_nonexistent_user_returns_false() {
        let (mut reg, _dir) = tmp_registry();
        assert!(!reg.remove_user("ghost").unwrap());
    }

    #[test]
    fn list_users_returns_all() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        reg.register_user("bob").unwrap();
        let users = reg.list_users();
        assert_eq!(users.len(), 2);
        let names: HashSet<&str> = users.iter().map(|u| u.username.as_str()).collect();
        assert!(names.contains("alice"));
        assert!(names.contains("bob"));
    }

    // ── Token auth ─────────────────────────────────────────────────

    #[test]
    fn issue_and_validate_token() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        let token = reg.issue_token("alice").unwrap();
        assert_eq!(reg.validate_token(&token), Some("alice"));
    }

    #[test]
    fn issue_token_for_unregistered_user_fails() {
        let (mut reg, _dir) = tmp_registry();
        let err = reg.issue_token("ghost").unwrap_err();
        assert!(err.contains("not registered"));
    }

    #[test]
    fn validate_unknown_token_returns_none() {
        let (reg, _dir) = tmp_registry();
        assert!(reg.validate_token("bad-token").is_none());
    }

    #[test]
    fn revoke_token() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        let token = reg.issue_token("alice").unwrap();
        assert!(reg.revoke_token(&token).unwrap());
        assert!(reg.validate_token(&token).is_none());
    }

    #[test]
    fn revoke_nonexistent_token_returns_false() {
        let (mut reg, _dir) = tmp_registry();
        assert!(!reg.revoke_token("nope").unwrap());
    }

    #[test]
    fn revoke_user_tokens_removes_all() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        let t1 = reg.issue_token("alice").unwrap();
        let t2 = reg.issue_token("alice").unwrap();
        assert_eq!(reg.revoke_user_tokens("alice").unwrap(), 2);
        assert!(reg.validate_token(&t1).is_none());
        assert!(reg.validate_token(&t2).is_none());
    }

    #[test]
    fn multiple_users_tokens_isolated() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        reg.register_user("bob").unwrap();
        let ta = reg.issue_token("alice").unwrap();
        let tb = reg.issue_token("bob").unwrap();

        reg.revoke_user_tokens("alice").unwrap();
        assert!(reg.validate_token(&ta).is_none());
        assert_eq!(reg.validate_token(&tb), Some("bob"));
    }

    // ── Room membership ────────────────────────────────────────────

    #[test]
    fn join_and_leave_room() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        reg.join_room("alice", "lobby").unwrap();
        assert!(reg.get_user("alice").unwrap().rooms.contains("lobby"));

        assert!(reg.leave_room("alice", "lobby").unwrap());
        assert!(!reg.get_user("alice").unwrap().rooms.contains("lobby"));
    }

    #[test]
    fn join_multiple_rooms() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        reg.join_room("alice", "room-a").unwrap();
        reg.join_room("alice", "room-b").unwrap();
        let rooms = &reg.get_user("alice").unwrap().rooms;
        assert_eq!(rooms.len(), 2);
        assert!(rooms.contains("room-a"));
        assert!(rooms.contains("room-b"));
    }

    #[test]
    fn leave_room_not_member_returns_false() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        assert!(!reg.leave_room("alice", "nowhere").unwrap());
    }

    #[test]
    fn room_ops_on_unregistered_user_fail() {
        let (mut reg, _dir) = tmp_registry();
        assert!(reg.join_room("ghost", "lobby").is_err());
        assert!(reg.leave_room("ghost", "lobby").is_err());
    }

    // ── Status ─────────────────────────────────────────────────────

    #[test]
    fn set_and_clear_status() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        reg.set_status("alice", Some("coding".into())).unwrap();
        assert_eq!(
            reg.get_user("alice").unwrap().status.as_deref(),
            Some("coding")
        );

        reg.set_status("alice", None).unwrap();
        assert!(reg.get_user("alice").unwrap().status.is_none());
    }

    #[test]
    fn status_on_unregistered_user_fails() {
        let (mut reg, _dir) = tmp_registry();
        assert!(reg.set_status("ghost", Some("hi".into())).is_err());
    }

    // ── Persistence ────────────────────────────────────────────────

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let token;
        {
            let mut reg = UserRegistry::new(dir.path().to_owned());
            reg.register_user("alice").unwrap();
            token = reg.issue_token("alice").unwrap();
            reg.join_room("alice", "lobby").unwrap();
            reg.set_status("alice", Some("active".into())).unwrap();
            // save is called by each mutation, but explicit save is fine too
        }

        let loaded = UserRegistry::load(dir.path().to_owned()).unwrap();
        let user = loaded.get_user("alice").unwrap();
        assert_eq!(user.username, "alice");
        assert!(user.rooms.contains("lobby"));
        assert_eq!(user.status.as_deref(), Some("active"));
        assert_eq!(loaded.validate_token(&token), Some("alice"));
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let reg = UserRegistry::load(dir.path().to_owned()).unwrap();
        assert!(reg.list_users().is_empty());
    }

    #[test]
    fn has_token_for_user_true_when_token_exists() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        reg.issue_token("alice").unwrap();
        assert!(reg.has_token_for_user("alice"));
    }

    #[test]
    fn has_token_for_user_false_when_no_token() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        assert!(!reg.has_token_for_user("alice"));
    }

    #[test]
    fn register_user_idempotent_noop_for_existing() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        let token = reg.issue_token("alice").unwrap();
        // Should not error and should not disturb existing data
        reg.register_user_idempotent("alice").unwrap();
        assert_eq!(reg.validate_token(&token), Some("alice"));
    }

    #[test]
    fn register_user_idempotent_creates_new_user() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user_idempotent("bob").unwrap();
        assert!(reg.get_user("bob").is_some());
    }

    #[test]
    fn token_snapshot_returns_all_tokens() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        reg.register_user("bob").unwrap();
        let t1 = reg.issue_token("alice").unwrap();
        let t2 = reg.issue_token("bob").unwrap();
        let snap = reg.token_snapshot();
        assert_eq!(snap.get(&t1).map(String::as_str), Some("alice"));
        assert_eq!(snap.get(&t2).map(String::as_str), Some("bob"));
    }

    // ── import_token ───────────────────────────────────────────────

    #[test]
    fn import_token_preserves_uuid() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        reg.import_token("alice", "legacy-uuid-1234").unwrap();
        assert_eq!(reg.validate_token("legacy-uuid-1234"), Some("alice"));
    }

    #[test]
    fn import_token_noop_if_already_present() {
        let (mut reg, _dir) = tmp_registry();
        reg.register_user("alice").unwrap();
        reg.import_token("alice", "tok-abc").unwrap();
        // Second call must not error and must not change anything.
        reg.import_token("alice", "tok-abc").unwrap();
        assert_eq!(reg.validate_token("tok-abc"), Some("alice"));
    }

    #[test]
    fn import_token_fails_for_unregistered_user() {
        let (mut reg, _dir) = tmp_registry();
        let err = reg.import_token("ghost", "tok-xyz").unwrap_err();
        assert!(err.contains("not registered"));
    }

    #[test]
    fn load_corrupt_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(REGISTRY_FILE), "not json{{{").unwrap();
        let err = UserRegistry::load(dir.path().to_owned()).unwrap_err();
        assert!(err.contains("parse"));
    }

    #[test]
    fn persistence_survives_remove_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut reg = UserRegistry::new(dir.path().to_owned());
            reg.register_user("alice").unwrap();
            reg.register_user("bob").unwrap();
            reg.remove_user("alice").unwrap();
        }

        let loaded = UserRegistry::load(dir.path().to_owned()).unwrap();
        assert!(loaded.get_user("alice").is_none());
        assert!(loaded.get_user("bob").is_some());
    }
}
