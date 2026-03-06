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
        None => serde_json::json!({"type": "message", "content": content}).to_string(),
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
