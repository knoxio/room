use super::transport::{resolve_socket_target, send_message_with_token_target as transport_send};
use crate::message::Message;

/// One-shot subscribe subcommand: send `/subscribe [tier]` to the broker, print the response, exit.
///
/// Valid tiers: `full` (default) or `mentions_only`.
///
/// `socket` overrides the default socket path (auto-discovered if `None`).
pub async fn cmd_subscribe(
    room_id: &str,
    token: &str,
    tier: &str,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let target = resolve_socket_target(room_id, socket);
    let wire =
        serde_json::json!({"type": "command", "cmd": "subscribe", "params": [tier]}).to_string();
    let msg = transport_send(&target, token, &wire).await.map_err(|e| {
        if e.to_string().contains("invalid token") {
            anyhow::anyhow!("invalid token — run: room join <username>")
        } else {
            e
        }
    })?;

    match &msg {
        Message::System { content, .. } => println!("{content}"),
        _ => println!("{}", serde_json::to_string(&msg)?),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn subscribe_wire_payload_full() {
        let wire = serde_json::json!({"type": "command", "cmd": "subscribe", "params": ["full"]})
            .to_string();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "subscribe");
        assert_eq!(v["params"][0], "full");
    }

    #[test]
    fn subscribe_wire_payload_mentions_only() {
        let wire = serde_json::json!({
            "type": "command",
            "cmd": "subscribe",
            "params": ["mentions_only"]
        })
        .to_string();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "subscribe");
        assert_eq!(v["params"][0], "mentions_only");
    }
}
