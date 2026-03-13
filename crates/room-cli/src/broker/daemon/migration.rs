//! Token registry loading and legacy token migration.

use crate::registry::UserRegistry;

use super::config::DaemonConfig;

/// Load `UserRegistry` from `users.json`, or migrate from the legacy
/// `tokens.json` (written by the #334 system-token-map implementation)
/// if `users.json` does not yet exist.
///
/// After loading (or creating) the registry, always scans the legacy runtime
/// directory for per-room `.token` files left by older `room join` invocations
/// and imports any that are not already present. This lets clients that joined
/// before the `~/.room/state/` migration continue to use their existing tokens
/// without a forced re-join.
pub(crate) fn load_or_migrate_registry(config: &DaemonConfig) -> UserRegistry {
    let users_path = config.state_dir.join("users.json");

    let mut registry = if users_path.exists() {
        // Fast path: users.json exists — use it directly.
        UserRegistry::load(config.state_dir.clone()).unwrap_or_else(|e| {
            eprintln!("[daemon] failed to load user registry: {e}; starting empty");
            UserRegistry::new(config.state_dir.clone())
        })
    } else {
        // Migration path: import from legacy tokens.json if present.
        let tokens_path = config.system_tokens_path();
        if tokens_path.exists() {
            let legacy = crate::broker::auth::load_token_map(&tokens_path);
            if !legacy.is_empty() {
                eprintln!(
                    "[daemon] migrating {} token(s) from tokens.json to users.json",
                    legacy.len()
                );
                let mut reg = UserRegistry::new(config.state_dir.clone());
                for (token, username) in &legacy {
                    // register_user_idempotent is a no-op if already present.
                    if let Err(e) = reg.register_user_idempotent(username) {
                        eprintln!("[daemon] migration: register {username}: {e}");
                        continue;
                    }
                    // Re-insert the existing token directly via issue_token so the
                    // UUID is preserved. Since UserRegistry.issue_token generates a
                    // new UUID, we instead manipulate the token map via the public
                    // API by revoking nothing and accepting the registry's new token.
                    // Trade-off: legacy UUIDs are replaced; clients must re-join.
                    // This is acceptable — migration is a one-time event.
                    let _ = reg.issue_token(username);
                    let _ = token; // legacy token not preserved — clients must re-join
                }
                if let Err(e) = reg.save() {
                    eprintln!("[daemon] migration save failed: {e}");
                }
                reg
            } else {
                // tokens.json exists but is empty — start fresh.
                UserRegistry::new(config.state_dir.clone())
            }
        } else {
            // Neither file exists — start fresh.
            UserRegistry::new(config.state_dir.clone())
        }
    };

    // Always scan the legacy runtime dir for old per-room token files and
    // import any that are not already in the registry. Idempotent — safe to
    // run on every startup.
    migrate_legacy_tmpdir_tokens(&mut registry);

    registry
}

/// Scan the legacy runtime directory for per-room token files and import
/// them into `registry`.
///
/// Before `~/.room/state/` was introduced, `room join` wrote token files to
/// the platform runtime directory (`$TMPDIR` on macOS, `/tmp/` on Linux)
/// as `room-<room_id>-<username>.token`. This function reads each such file,
/// parses the `username` and `token` fields, and imports them — preserving
/// the UUID so existing clients do not need to re-join. Files whose tokens
/// are already in the registry are silently skipped (idempotent).
fn migrate_legacy_tmpdir_tokens(registry: &mut UserRegistry) {
    let legacy_dir = crate::paths::legacy_token_dir();
    migrate_legacy_tmpdir_tokens_from(&legacy_dir, registry);
}

/// Inner implementation of [`migrate_legacy_tmpdir_tokens`] that accepts an
/// explicit directory. Extracted so tests can pass a temp directory without
/// modifying process environment variables.
pub(super) fn migrate_legacy_tmpdir_tokens_from(
    legacy_dir: &std::path::Path,
    registry: &mut UserRegistry,
) {
    let entries = match std::fs::read_dir(legacy_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut count = 0usize;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => continue,
        };
        if !name.starts_with("room-") || !name.ends_with(".token") {
            continue;
        }
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(data.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let (username, token) = match (v["username"].as_str(), v["token"].as_str()) {
            (Some(u), Some(t)) if !u.is_empty() && !t.is_empty() => (u.to_owned(), t.to_owned()),
            _ => continue,
        };
        if let Err(e) = registry.register_user_idempotent(&username) {
            eprintln!("[daemon] legacy token migration: register {username}: {e}");
            continue;
        }
        match registry.import_token(&username, &token) {
            Ok(()) => count += 1,
            Err(e) => {
                eprintln!("[daemon] legacy token migration: import token for {username}: {e}")
            }
        }
    }
    if count > 0 {
        eprintln!(
            "[daemon] imported {count} legacy token(s) from {}",
            legacy_dir.display()
        );
    }
}
