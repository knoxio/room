//! UDS connection dispatcher: routes incoming connections to the correct handler.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use tokio::sync::broadcast;

use crate::registry::UserRegistry;

use super::{
    config::DaemonConfig,
    lifecycle::{handle_create, handle_destroy, handle_global_join},
    RoomMap,
};
use crate::broker::state::TokenMap;

/// Dispatch a raw UDS connection to the correct room based on the handshake.
///
/// Handles two top-level protocols:
/// - `CREATE:<room_id>` — create a new room (reads config JSON from second line)
/// - `ROOM:<room_id>:<rest>` — route to an existing room
pub(super) async fn dispatch_connection(
    stream: tokio::net::UnixStream,
    rooms: &RoomMap,
    next_client_id: &Arc<AtomicU64>,
    daemon_config: &DaemonConfig,
    system_token_map: &TokenMap,
    user_registry: &Arc<tokio::sync::Mutex<UserRegistry>>,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncWriteExt, BufReader};

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let mut first = String::new();
    crate::broker::read_line_limited(&mut reader, &mut first).await?;
    let first_line = first.trim();

    if first_line.is_empty() {
        return Ok(());
    }

    use crate::broker::handshake::{
        parse_client_handshake, parse_daemon_prefix, ClientHandshake, DaemonPrefix,
    };
    let (room_id, rest) = match parse_daemon_prefix(first_line) {
        DaemonPrefix::Destroy(room_id) => {
            return handle_destroy(&room_id, &mut reader, &mut write_half, rooms, user_registry)
                .await;
        }
        DaemonPrefix::Create(room_id) => {
            return handle_create(
                &room_id,
                &mut reader,
                &mut write_half,
                rooms,
                daemon_config,
                system_token_map,
                user_registry,
            )
            .await;
        }
        DaemonPrefix::Join(username) => {
            return handle_global_join(&username, &mut write_half, user_registry).await;
        }
        DaemonPrefix::Room { room_id, rest } => (room_id, rest),
        DaemonPrefix::Unknown => {
            let err = serde_json::json!({
                "type": "error",
                "code": "missing_room_prefix",
                "message": "daemon mode requires ROOM:<room_id>: or CREATE:<room_id> prefix"
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    };

    // Look up the room.
    let state = {
        let map = rooms.lock().await;
        map.get(room_id.as_str()).cloned()
    };

    let state = match state {
        Some(s) => s,
        None => {
            let err = serde_json::json!({
                "type": "error",
                "code": "room_not_found",
                "room": room_id
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    };

    let cid = next_client_id.fetch_add(1, Ordering::SeqCst) + 1;

    // Dispatch based on the per-room handshake after the ROOM: prefix.
    let username = match parse_client_handshake(&rest) {
        ClientHandshake::Send(u) => {
            eprintln!(
                "[broker/daemon] DEPRECATED: SEND:{u} handshake used — \
                 migrate to TOKEN:<uuid> (SEND: will be removed in a future version)"
            );
            return crate::broker::handle_oneshot_send(u, reader, write_half, &state).await;
        }
        ClientHandshake::Token(token) => {
            // Try room-level token map first, then fall back to global UserRegistry.
            let resolved = match crate::broker::auth::validate_token(&token, &state.token_map).await
            {
                Some(u) => Some(u),
                None => {
                    let reg = user_registry.lock().await;
                    reg.validate_token(&token).map(|u| u.to_owned())
                }
            };
            return match resolved {
                Some(u) => crate::broker::handle_oneshot_send(u, reader, write_half, &state).await,
                None => {
                    let err = serde_json::json!({"type":"error","code":"invalid_token"});
                    write_half
                        .write_all(format!("{err}\n").as_bytes())
                        .await
                        .map_err(Into::into)
                }
            };
        }
        ClientHandshake::Join(u) => {
            let result = crate::broker::auth::handle_oneshot_join_with_registry(
                u,
                write_half,
                user_registry,
                &state.token_map,
                &state.subscription_map,
                state.config.as_ref(),
            )
            .await;
            // Persist auto-subscription from join so it survives broker restart.
            crate::broker::persistence::persist_subscriptions(&state).await;
            return result;
        }
        ClientHandshake::Session(token) => {
            // Resolve username from token (room-level first, then UserRegistry).
            let resolved = match crate::broker::auth::validate_token(&token, &state.token_map).await
            {
                Some(u) => Some(u),
                None => {
                    let reg = user_registry.lock().await;
                    reg.validate_token(&token).map(|u| u.to_owned())
                }
            };
            match resolved {
                Some(u) => u,
                None => {
                    let err = serde_json::json!({"type":"error","code":"invalid_token"});
                    write_half.write_all(format!("{err}\n").as_bytes()).await?;
                    return Ok(());
                }
            }
        }
        ClientHandshake::Interactive(u) => {
            eprintln!(
                "[broker/daemon] DEPRECATED: unauthenticated interactive join for '{u}' — \
                 migrate to SESSION:<token>"
            );
            u
        }
    };

    // Interactive join (authenticated via SESSION: or deprecated plain username).
    if username.is_empty() {
        return Ok(());
    }

    // Check join permission before entering interactive session.
    if let Err(reason) =
        crate::broker::auth::check_join_permission(&username, state.config.as_ref())
    {
        let err = serde_json::json!({
            "type": "error",
            "code": "join_denied",
            "message": reason,
            "username": username
        });
        write_half.write_all(format!("{err}\n").as_bytes()).await?;
        return Ok(());
    }

    // Register client in room, then hand off to the full interactive handler.
    let (tx, _) = broadcast::channel::<String>(256);
    state
        .clients
        .lock()
        .await
        .insert(cid, (String::new(), tx.clone()));

    let result =
        crate::broker::run_interactive_session(cid, &username, reader, write_half, tx, &state)
            .await;

    state.clients.lock().await.remove(&cid);
    result
}
