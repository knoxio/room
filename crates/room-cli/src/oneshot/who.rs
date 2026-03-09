use super::transport::{resolve_socket_target, send_message_with_token_target as transport_send};
use crate::message::Message;

/// One-shot who subcommand: send `/who` to the broker, print the response, exit.
///
/// By default prints just the human-readable content line. With `json = true`,
/// prints the full JSON response for machine consumption.
///
/// `socket` overrides the default socket path (auto-discovered if `None`).
pub async fn cmd_who(
    room_id: &str,
    token: &str,
    json: bool,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let target = resolve_socket_target(room_id, socket);
    let wire = serde_json::json!({"type": "command", "cmd": "who", "params": []}).to_string();
    let msg = transport_send(&target, token, &wire).await.map_err(|e| {
        if e.to_string().contains("invalid token") {
            anyhow::anyhow!("invalid token — run: room join <username>")
        } else {
            e
        }
    })?;

    if json {
        println!("{}", serde_json::to_string(&msg)?);
    } else {
        match &msg {
            Message::System { content, .. } => println!("{content}"),
            _ => println!("{}", serde_json::to_string(&msg)?),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::message::Message;

    #[test]
    fn system_message_content_extraction() {
        let json = r#"{"type":"system","id":"abc","room":"r","user":"broker","ts":"2026-01-01T00:00:00Z","seq":1,"content":"online — alice, bob: reviewing PR"}"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        if let Message::System { content, .. } = &msg {
            assert_eq!(content, "online — alice, bob: reviewing PR");
        } else {
            panic!("expected System message");
        }
    }

    #[test]
    fn who_wire_payload_is_valid_command() {
        let wire = serde_json::json!({"type": "command", "cmd": "who", "params": []}).to_string();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "who");
        assert!(v["params"].as_array().unwrap().is_empty());
    }
}
