pub mod list;
pub mod poll;
pub mod token;
pub mod transport;
pub mod who;

pub use list::cmd_list;
pub use poll::{
    cmd_poll, cmd_poll_multi, cmd_pull, cmd_query, cmd_watch, poll_messages, poll_messages_multi,
    pull_messages, QueryOptions,
};
pub use token::{cmd_join, token_file_path, username_from_token, username_from_token_any_room};
pub use transport::{
    join_session, join_session_target, resolve_socket_target, send_message,
    send_message_with_token, send_message_with_token_target, SocketTarget,
};
pub use who::cmd_who;

use room_protocol::dm_room_id;
use transport::send_message_with_token_target as transport_send_target;

/// One-shot send subcommand: connect, send, print echo JSON to stdout, exit.
///
/// Authenticates via `token` (from `room join`). The broker resolves the sender's
/// username from the token — no username arg required. When `to` is `Some(recipient)`,
/// the message is sent as a DM routed only to sender, recipient, and host.
///
/// Slash commands (e.g. `/who`, `/dm user msg`) are automatically converted to the
/// appropriate JSON envelope, matching TUI behaviour.
///
/// `socket` overrides the default socket path (auto-discovered if `None`).
pub async fn cmd_send(
    room_id: &str,
    token: &str,
    to: Option<&str>,
    content: &str,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let target = resolve_socket_target(room_id, socket);
    let wire = match to {
        Some(recipient) => {
            serde_json::json!({"type": "dm", "to": recipient, "content": content}).to_string()
        }
        None => build_wire_payload(content),
    };
    let msg = transport_send_target(&target, token, &wire)
        .await
        .map_err(|e| {
            if e.to_string().contains("invalid token") {
                anyhow::anyhow!("invalid token — run: room join {room_id} <username>")
            } else {
                e
            }
        })?;
    println!("{}", serde_json::to_string(&msg)?);
    Ok(())
}

/// One-shot DM subcommand: compute canonical DM room ID, send message, exit.
///
/// Resolves the caller's username from the token file, then computes the
/// deterministic DM room ID (`dm-<sorted_a>-<sorted_b>`). Sends the message
/// to that room's broker socket. The DM room must already exist (room creation
/// will be handled by E1-6 dynamic room creation).
///
/// Returns an error if the caller tries to DM themselves or if the DM room
/// broker is not running.
///
/// `socket` overrides the default socket path (auto-discovered if `None`).
pub async fn cmd_dm(
    recipient: &str,
    token: &str,
    content: &str,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    // Resolve the caller's username from the token
    let caller = username_from_token_any_room(token)?;

    // Compute canonical DM room ID
    let dm_id = dm_room_id(&caller, recipient).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Build the wire payload as a DM message
    let wire = serde_json::json!({"type": "dm", "to": recipient, "content": content}).to_string();

    // Resolve socket target for the DM room.
    let target = resolve_socket_target(&dm_id, socket);
    let msg = transport_send_target(&target, token, &wire)
        .await
        .map_err(|e| {
            if e.to_string().contains("No such file")
                || e.to_string().contains("Connection refused")
            {
                anyhow::anyhow!(
                    "DM room '{dm_id}' is not running — start it or use a daemon with the room pre-created"
                )
            } else if e.to_string().contains("invalid token") {
                anyhow::anyhow!(
                    "invalid token for DM room '{dm_id}' — you may need to join it first"
                )
            } else {
                e
            }
        })?;
    println!("{}", serde_json::to_string(&msg)?);
    Ok(())
}

/// Convert user input into a JSON wire envelope, routing slash commands to the
/// appropriate message type. Mirrors `tui::input::build_payload` for parity.
fn build_wire_payload(input: &str) -> String {
    // `/dm <user> <message>`
    if let Some(rest) = input.strip_prefix("/dm ") {
        let mut parts = rest.splitn(2, ' ');
        let to = parts.next().unwrap_or("");
        let content = parts.next().unwrap_or("");
        return serde_json::json!({"type": "dm", "to": to, "content": content}).to_string();
    }
    // Any other slash command: `/who`, `/kick user`, etc.
    if let Some(rest) = input.strip_prefix('/') {
        let mut parts = rest.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("");
        let params: Vec<&str> = parts.next().unwrap_or("").split_whitespace().collect();
        return serde_json::json!({"type": "command", "cmd": cmd, "params": params}).to_string();
    }
    // Plain message
    serde_json::json!({"type": "message", "content": input}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_message() {
        let wire = build_wire_payload("hello world");
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"], "hello world");
    }

    #[test]
    fn who_command() {
        let wire = build_wire_payload("/who");
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "who");
        let params = v["params"].as_array().unwrap();
        assert!(params.is_empty());
    }

    #[test]
    fn command_with_params() {
        let wire = build_wire_payload("/kick alice");
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "kick");
        let params: Vec<&str> = v["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        assert_eq!(params, vec!["alice"]);
    }

    #[test]
    fn command_with_multiple_params() {
        let wire = build_wire_payload("/set_status away brb");
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "set_status");
        let params: Vec<&str> = v["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        assert_eq!(params, vec!["away", "brb"]);
    }

    #[test]
    fn dm_via_slash() {
        let wire = build_wire_payload("/dm bob hey there");
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "dm");
        assert_eq!(v["to"], "bob");
        assert_eq!(v["content"], "hey there");
    }

    #[test]
    fn dm_slash_no_message() {
        let wire = build_wire_payload("/dm bob");
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "dm");
        assert_eq!(v["to"], "bob");
        assert_eq!(v["content"], "");
    }

    #[test]
    fn slash_only() {
        let wire = build_wire_payload("/");
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "");
    }

    #[test]
    fn message_starting_with_slash_like_path() {
        // Only exact slash-prefix triggers command routing — `/tmp/foo` is a command named `tmp/foo`
        // This matches TUI behaviour: any `/` prefix is a command
        let wire = build_wire_payload("/tmp/foo");
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "tmp/foo");
    }

    #[test]
    fn empty_string() {
        let wire = build_wire_payload("");
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"], "");
    }
}
