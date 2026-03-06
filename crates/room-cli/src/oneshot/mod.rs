pub mod list;
pub mod poll;
pub mod token;
pub mod transport;

pub use list::cmd_list;
pub use poll::{cmd_poll, cmd_pull, cmd_watch, poll_messages, pull_messages};
pub use token::{cmd_join, token_file_path, username_from_token};
pub use transport::{join_session, send_message, send_message_with_token};

use std::path::PathBuf;

use transport::send_message_with_token as transport_send;

/// One-shot send subcommand: connect, send, print echo JSON to stdout, exit.
///
/// Authenticates via `token` (from `room join`). The broker resolves the sender's
/// username from the token — no username arg required. When `to` is `Some(recipient)`,
/// the message is sent as a DM routed only to sender, recipient, and host.
///
/// Slash commands (e.g. `/who`, `/dm user msg`) are automatically converted to the
/// appropriate JSON envelope, matching TUI behaviour.
pub async fn cmd_send(
    room_id: &str,
    token: &str,
    to: Option<&str>,
    content: &str,
) -> anyhow::Result<()> {
    let socket_path = PathBuf::from(format!("/tmp/room-{room_id}.sock"));
    let wire = match to {
        Some(recipient) => {
            serde_json::json!({"type": "dm", "to": recipient, "content": content}).to_string()
        }
        None => build_wire_payload(content),
    };
    let msg = transport_send(&socket_path, token, &wire)
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
