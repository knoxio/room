use crate::message::{make_system, Message};

use super::{fanout::broadcast_and_persist, state::RoomState};

/// Admin command names — routed through `handle_admin_cmd` when received as
/// a `Message::Command` with one of these cmd values.
pub(crate) const ADMIN_CMD_NAMES: &[&str] = &["kick", "reauth", "clear-tokens", "exit", "clear"];

/// The result of routing an inbound command line.
pub(crate) enum CommandResult {
    /// The command was fully handled with a broadcast or no-op; nothing to send back privately.
    Handled,
    /// The command was handled privately; send this JSON line back only to the issuer.
    Reply(String),
    /// The command was handled and the broker is shutting down.
    Shutdown,
    /// Not a special command; the caller should broadcast or DM `msg` normally.
    Passthrough(Message),
}

/// Route a parsed `Message` for a given `username` against `state`.
///
/// Handles `set_status`, `who`, and all admin commands inline. For any other
/// message (including regular chat) returns `CommandResult::Passthrough(msg)`
/// so the caller can broadcast or DM it.
///
/// This is the unified entry point used by both the interactive inbound task
/// and `handle_oneshot_send` — previously the logic was duplicated in both.
pub(crate) async fn route_command(
    msg: Message,
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    if let Message::Command {
        ref cmd,
        ref params,
        ..
    } = msg
    {
        if cmd == "set_status" {
            let status = params.first().cloned().unwrap_or_default();
            state
                .status_map
                .lock()
                .await
                .insert(username.to_owned(), status.clone());
            let display = if status.is_empty() {
                format!("{username} cleared their status")
            } else {
                format!("{username} set status: {status}")
            };
            let sys = make_system(&state.room_id, "broker", display);
            broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter)
                .await?;
            // Broadcast already delivers to all connected clients including the sender;
            // no additional private reply needed.
            return Ok(CommandResult::Handled);
        }

        if cmd == "who" {
            let map = state.status_map.lock().await;
            let mut entries: Vec<String> = map
                .iter()
                .map(|(u, s)| {
                    if s.is_empty() {
                        u.clone()
                    } else {
                        format!("{u}: {s}")
                    }
                })
                .collect();
            entries.sort();
            drop(map);
            let content = if entries.is_empty() {
                "no users online".to_owned()
            } else {
                format!("online — {}", entries.join(", "))
            };
            let sys = make_system(&state.room_id, "broker", content);
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }

        if ADMIN_CMD_NAMES.contains(&cmd.as_str()) {
            let cmd_line = format!("{cmd} {}", params.join(" "));
            let error = handle_admin_cmd(&cmd_line, username, state).await;
            if let Some(err) = error {
                // Permission denied or invalid args — send error back privately.
                let sys = make_system(&state.room_id, "broker", err);
                let json = serde_json::to_string(&sys)?;
                return Ok(CommandResult::Reply(json));
            }
            // Admin command succeeded — the command itself may have broadcast a notice.
            // If it was /exit the shutdown signal was already sent; signal the caller.
            if cmd == "exit" {
                return Ok(CommandResult::Shutdown);
            }
            return Ok(CommandResult::Handled);
        }
    }

    Ok(CommandResult::Passthrough(msg))
}

/// Dispatch a `\command [arg]` line sent from a connected client.
///
/// Returns `None` on success or `Some(error_message)` if the command was rejected.
/// The caller is responsible for delivering any error message back to the issuer.
///
/// Only the room host (the first user to complete the interactive join handshake) is
/// authorised to run admin commands. All other callers receive a permission denied error.
///
/// Supported commands:
/// - `/kick <username>`      — invalidates the user's token so they cannot issue further
///   authenticated requests; the username remains reserved so they cannot rejoin without `\reauth`.
///   Also removes them from the status map so `/who` no longer shows them as online.
/// - `/reauth <username>`    — removes the user's token entirely so they can `room join` again.
/// - `/clear-tokens`         — removes every token for this room (all users must rejoin).
/// - `/exit`                 — broadcasts a shutdown notice then signals the broker to stop.
/// - `/clear`                — truncates the chat history file and broadcasts a notice.
pub(crate) async fn handle_admin_cmd(
    cmd_line: &str,
    issuer: &str,
    state: &RoomState,
) -> Option<String> {
    // Auth: only the room host may run admin commands.
    let host = state.host_user.lock().await.clone();
    if host.as_deref() != Some(issuer) {
        return Some(
            "permission denied: admin commands are restricted to the room host".to_string(),
        );
    }

    let room_id = state.room_id.as_str();
    let clients = &state.clients;
    let token_map = &state.token_map;
    let status_map = &state.status_map;
    let chat_path = &state.chat_path;
    let shutdown = &state.shutdown;
    let seq_counter = &state.seq_counter;
    let mut parts = cmd_line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();

    match cmd {
        "kick" => {
            if arg.is_empty() {
                return None;
            }
            let target = arg.to_owned();
            let mut map = token_map.lock().await;
            // Remove all existing tokens for this username, then insert a per-user sentinel
            // so the username stays reserved. Using KICKED:<username> as the key ensures
            // kicking multiple users does not overwrite each other's sentinel entries.
            map.retain(|_, u| u != &target);
            map.insert(format!("KICKED:{target}"), target.clone());
            drop(map);
            // Remove from status map immediately so /who no longer shows the kicked user.
            status_map.lock().await.remove(&target);
            let content = format!("{issuer} kicked {target} (token invalidated)");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        "reauth" => {
            if arg.is_empty() {
                return None;
            }
            let target = arg.to_owned();
            let mut map = token_map.lock().await;
            map.retain(|_, u| u != &target);
            drop(map);
            // Remove the on-disk token file so the user can join afresh.
            let prefix = format!("room-{room_id}-");
            let suffix = format!("-{target}.token");
            if let Ok(entries) = std::fs::read_dir("/tmp") {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let s = name.to_string_lossy();
                    if s.starts_with(&prefix) && s.ends_with(&suffix) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            let content = format!("{issuer} reauthed {target} (token cleared, can rejoin)");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        "clear-tokens" => {
            token_map.lock().await.clear();
            // Remove all on-disk token files for this room.
            let prefix = format!("room-{room_id}-");
            if let Ok(entries) = std::fs::read_dir("/tmp") {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let s = name.to_string_lossy();
                    if s.starts_with(&prefix) && s.ends_with(".token") {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            let content = format!("{issuer} cleared all tokens (all users must rejoin)");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        "exit" => {
            let content = format!("{issuer} is shutting down the room");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
            // Set to true — watch receivers see this immediately regardless of
            // when they registered, avoiding the notify_waiters() race.
            let _ = shutdown.send(true);
        }
        "clear" => {
            // Truncate the history file.
            if let Err(e) = std::fs::write(chat_path.as_ref(), "") {
                eprintln!("[broker] \\clear failed: {e}");
                return None;
            }
            let content = format!("{issuer} cleared chat history");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        _ => {
            eprintln!("[broker] unknown admin command from {issuer}: \\{cmd_line}");
        }
    }
    None
}
