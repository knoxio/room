//! Room lifecycle operations: create, destroy, global join, subscriptions.

use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;

use crate::registry::UserRegistry;

use super::{
    config::{validate_room_id, DaemonConfig},
    RoomMap,
};
use crate::broker::state::{RoomState, TokenMap};

/// Build the initial subscription map for a room based on its config.
///
/// DM rooms auto-subscribe both participants at `Full` so they receive all
/// messages without an explicit `/subscribe` call. Other room types start
/// with an empty subscription map (users subscribe explicitly or via
/// auto-subscribe-on-mention).
pub(crate) fn build_initial_subscriptions(
    config: &room_protocol::RoomConfig,
) -> HashMap<String, room_protocol::SubscriptionTier> {
    let mut subs = HashMap::new();
    if config.visibility == room_protocol::RoomVisibility::Dm {
        for user in &config.invite_list {
            subs.insert(user.clone(), room_protocol::SubscriptionTier::Full);
        }
    }
    subs
}

/// Core room-creation logic shared by UDS and REST paths.
///
/// Validates the room ID, checks for duplicates, builds a [`RoomState`], and
/// inserts it into the room map. Pass `config: None` to create a configless
/// room (no invite list, no visibility constraint).
///
/// `registry` is attached to the [`RoomState`] via [`RoomState::set_registry`]
/// so that admin commands (`/kick`, `/reauth`) can also revoke tokens from the
/// daemon-level [`UserRegistry`] in addition to the in-memory token map.
pub(crate) async fn create_room_entry(
    room_id: &str,
    config: Option<room_protocol::RoomConfig>,
    rooms: &RoomMap,
    daemon_config: &DaemonConfig,
    system_token_map: &TokenMap,
    registry: Option<Arc<tokio::sync::Mutex<UserRegistry>>>,
) -> Result<(), String> {
    validate_room_id(room_id)?;

    {
        let map = rooms.lock().await;
        if map.contains_key(room_id) {
            return Err(format!("room already exists: {room_id}"));
        }
    }

    let chat_path = daemon_config.chat_path(room_id);
    let subscription_map_path = daemon_config.subscription_map_path(room_id);

    // Load persisted subscriptions and merge with config-derived initial subs.
    let persisted_subs = crate::broker::persistence::load_subscription_map(&subscription_map_path);
    let merged_subs = if let Some(ref cfg) = config {
        let mut initial = build_initial_subscriptions(cfg);
        initial.extend(persisted_subs);
        initial
    } else {
        persisted_subs
    };

    let state = RoomState::new(
        room_id.to_owned(),
        chat_path,
        daemon_config.system_tokens_path(),
        subscription_map_path,
        Arc::clone(system_token_map),
        Arc::new(Mutex::new(merged_subs)),
        config,
    )?;

    // Attach the UserRegistry so admin commands propagate identity changes.
    if let Some(reg) = registry {
        state.set_registry(reg);
    }

    // Attach persisted event filters.
    let ef_path = daemon_config.event_filter_map_path(room_id);
    let persisted_ef = crate::broker::persistence::load_event_filter_map(&ef_path);
    state.set_event_filter_map(Arc::new(Mutex::new(persisted_ef)), ef_path);

    rooms.lock().await.insert(room_id.to_owned(), state);

    // Write a meta file so one-shot commands (poll, watch, pull) can find the
    // chat file without a broker connection. The meta file lives in the
    // platform runtime dir alongside the daemon socket.
    let meta_path = crate::paths::room_meta_path(room_id);
    let chat_path_str = daemon_config.chat_path(room_id);
    let meta_json = serde_json::json!({ "chat_path": chat_path_str });
    let _ = std::fs::write(&meta_path, meta_json.to_string());

    Ok(())
}

/// Handle a `DESTROY:<room_id>` request: remove the room from the daemon.
///
/// Protocol:
/// 1. Client sends `DESTROY:<room_id>\n`
/// 2. Client sends `<token>\n` on the next line.
/// 3. Daemon validates the token, then responds with
///    `{"type":"room_destroyed","room":"<id>"}\n` or an error.
///
/// Connected clients receive EOF when the room's shutdown signal fires.
/// Chat files are preserved on disk.
pub(super) async fn handle_destroy(
    room_id: &str,
    reader: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    rooms: &RoomMap,
    user_registry: &Arc<tokio::sync::Mutex<UserRegistry>>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    if room_id.is_empty() {
        let err = serde_json::json!({
            "type": "error",
            "code": "invalid_room_id",
            "message": "room ID is empty"
        });
        write_half.write_all(format!("{err}\n").as_bytes()).await?;
        return Ok(());
    }

    // Read the token from the second line.
    let mut token_line = String::new();
    crate::broker::read_line_limited(reader, &mut token_line).await?;
    let token = token_line.trim();

    if token.is_empty() {
        let err = serde_json::json!({
            "type": "error",
            "code": "missing_token",
            "message": "DESTROY requires a valid token on the second line"
        });
        write_half.write_all(format!("{err}\n").as_bytes()).await?;
        return Ok(());
    }

    // Validate against UserRegistry.
    {
        let reg = user_registry.lock().await;
        if reg.validate_token(token).is_none() {
            let err = serde_json::json!({
                "type": "error",
                "code": "invalid_token",
                "message": "token is not valid"
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    }

    // Remove the room and signal shutdown.
    let state = {
        let mut map = rooms.lock().await;
        map.remove(room_id)
    };

    match state {
        Some(s) => {
            // Signal shutdown so connected clients receive EOF.
            let _ = s.shutdown.send(true);
            let ok = serde_json::json!({
                "type": "room_destroyed",
                "room": room_id
            });
            write_half.write_all(format!("{ok}\n").as_bytes()).await?;
        }
        None => {
            let err = serde_json::json!({
                "type": "error",
                "code": "room_not_found",
                "room": room_id
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
        }
    }

    Ok(())
}

/// Handle a `CREATE:<room_id>` request: validate, read config, create the room.
///
/// Protocol:
/// 1. Client sends `CREATE:<room_id>\n`
/// 2. Client sends config JSON on the next line: `{"visibility":"public","invite":[]}\n`
/// 3. Daemon responds with `{"type":"room_created","room":"<id>"}\n` or an error envelope.
pub(super) async fn handle_create(
    room_id: &str,
    reader: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    rooms: &RoomMap,
    daemon_config: &DaemonConfig,
    system_token_map: &TokenMap,
    user_registry: &Arc<tokio::sync::Mutex<UserRegistry>>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    // Validate room ID.
    if let Err(e) = validate_room_id(room_id) {
        let err = serde_json::json!({
            "type": "error",
            "code": "invalid_room_id",
            "message": e
        });
        write_half.write_all(format!("{err}\n").as_bytes()).await?;
        return Ok(());
    }

    // Check for duplicate before reading config (fast-fail).
    {
        let map = rooms.lock().await;
        if map.contains_key(room_id) {
            let err = serde_json::json!({
                "type": "error",
                "code": "room_exists",
                "message": format!("room already exists: {room_id}")
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    }

    // Read config JSON from second line.
    let mut config_line = String::new();
    crate::broker::read_line_limited(reader, &mut config_line).await?;
    let config_str = config_line.trim();

    let (visibility_str, invite, token): (String, Vec<String>, Option<String>) =
        if config_str.is_empty() {
            ("public".into(), vec![], None)
        } else {
            let v: serde_json::Value = match serde_json::from_str(config_str) {
                Ok(v) => v,
                Err(e) => {
                    let err = serde_json::json!({
                        "type": "error",
                        "code": "invalid_config",
                        "message": format!("invalid config JSON: {e}")
                    });
                    write_half.write_all(format!("{err}\n").as_bytes()).await?;
                    return Ok(());
                }
            };
            let vis = v["visibility"].as_str().unwrap_or("public").to_owned();
            let inv = v["invite"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                        .collect()
                })
                .unwrap_or_default();
            let tok = v["token"].as_str().map(|s| s.to_owned());
            (vis, inv, tok)
        };

    // Validate the token.
    let token_str = match token.as_deref() {
        Some(t) if !t.is_empty() => t,
        _ => {
            let err = serde_json::json!({
                "type": "error",
                "code": "missing_token",
                "message": "CREATE requires a valid token in the config JSON"
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    };
    {
        let reg = user_registry.lock().await;
        if reg.validate_token(token_str).is_none() {
            let err = serde_json::json!({
                "type": "error",
                "code": "invalid_token",
                "message": "token is not valid"
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    }

    // Build RoomConfig from the parsed visibility + invite list.
    let room_config = match visibility_str.as_str() {
        "public" => room_protocol::RoomConfig {
            visibility: room_protocol::RoomVisibility::Public,
            max_members: None,
            invite_list: invite.into_iter().collect(),
            created_by: "system".to_owned(),
            created_at: chrono::Utc::now().to_rfc3339(),
        },
        "private" => room_protocol::RoomConfig {
            visibility: room_protocol::RoomVisibility::Private,
            max_members: None,
            invite_list: invite.into_iter().collect(),
            created_by: "system".to_owned(),
            created_at: chrono::Utc::now().to_rfc3339(),
        },
        "dm" => {
            if invite.len() != 2 {
                let err = serde_json::json!({
                    "type": "error",
                    "code": "invalid_config",
                    "message": "dm visibility requires exactly 2 users in invite list"
                });
                write_half.write_all(format!("{err}\n").as_bytes()).await?;
                return Ok(());
            }
            room_protocol::RoomConfig::dm(&invite[0], &invite[1])
        }
        other => {
            let err = serde_json::json!({
                "type": "error",
                "code": "invalid_config",
                "message": format!("unknown visibility: {other}")
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    };

    // Delegate to the shared room-creation helper.
    if let Err(e) = create_room_entry(
        room_id,
        Some(room_config),
        rooms,
        daemon_config,
        system_token_map,
        Some(user_registry.clone()),
    )
    .await
    {
        let err = serde_json::json!({
            "type": "error",
            "code": "internal",
            "message": e
        });
        write_half.write_all(format!("{err}\n").as_bytes()).await?;
        return Ok(());
    }

    let ok = serde_json::json!({
        "type": "room_created",
        "room": room_id
    });
    write_half.write_all(format!("{ok}\n").as_bytes()).await?;
    Ok(())
}

/// Handle a global `JOIN:<username>` request at daemon level.
///
/// Registers the user in the global UserRegistry (or returns the existing token
/// if already registered) and writes the token response. No room association.
pub(super) async fn handle_global_join(
    username: &str,
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    registry: &Arc<tokio::sync::Mutex<UserRegistry>>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let mut reg = registry.lock().await;

    // If user already has a token, return it. Otherwise register and issue.
    let token = if reg.has_token_for_user(username) {
        // Find existing token via snapshot (reverse lookup: token→user).
        reg.token_snapshot()
            .into_iter()
            .find(|(_, u)| u == username)
            .map(|(t, _)| t)
            .expect("has_token_for_user was true but no token found")
    } else {
        reg.register_user_idempotent(username)
            .map_err(|e| anyhow::anyhow!("registration failed: {e}"))?;
        reg.issue_token(username)
            .map_err(|e| anyhow::anyhow!("token issuance failed: {e}"))?
    };

    let resp = serde_json::json!({
        "type": "token",
        "token": token,
        "username": username
    });
    write_half.write_all(format!("{resp}\n").as_bytes()).await?;
    Ok(())
}
