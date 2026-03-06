pub use room_protocol::*;

use serde::Deserialize;

/// Parse a raw line from a client socket.
/// JSON envelope → Message with broker-assigned id/room/ts.
/// Plain text → Message::Message with broker-assigned metadata.
pub fn parse_client_line(raw: &str, room: &str, user: &str) -> Result<Message, serde_json::Error> {
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Envelope {
        Message {
            content: String,
        },
        Reply {
            reply_to: String,
            content: String,
        },
        Command {
            cmd: String,
            params: Vec<String>,
        },
        #[serde(rename = "dm")]
        Dm {
            to: String,
            content: String,
        },
    }

    if raw.starts_with('{') {
        let env: Envelope = serde_json::from_str(raw)?;
        let msg = match env {
            Envelope::Message { content } => make_message(room, user, content),
            Envelope::Reply { reply_to, content } => make_reply(room, user, reply_to, content),
            Envelope::Command { cmd, params } => make_command(room, user, cmd, params),
            Envelope::Dm { to, content } => make_dm(room, user, &to, content),
        };
        Ok(msg)
    } else {
        Ok(make_message(room, user, raw))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_text_becomes_message() {
        let msg = parse_client_line("hello there", "myroom", "alice").unwrap();
        assert!(matches!(&msg, Message::Message { content, .. } if content == "hello there"));
        assert_eq!(msg.user(), "alice");
        assert_eq!(msg.room(), "myroom");
    }

    #[test]
    fn parse_json_message_envelope() {
        let raw = r#"{"type":"message","content":"from agent"}"#;
        let msg = parse_client_line(raw, "r", "bot1").unwrap();
        assert!(matches!(&msg, Message::Message { content, .. } if content == "from agent"));
    }

    #[test]
    fn parse_json_reply_envelope() {
        let raw = r#"{"type":"reply","reply_to":"deadbeef","content":"ack"}"#;
        let msg = parse_client_line(raw, "r", "bot1").unwrap();
        assert!(
            matches!(&msg, Message::Reply { reply_to, content, .. } if reply_to == "deadbeef" && content == "ack")
        );
    }

    #[test]
    fn parse_json_command_envelope() {
        let raw = r#"{"type":"command","cmd":"claim","params":["task-42"]}"#;
        let msg = parse_client_line(raw, "r", "agent").unwrap();
        assert!(
            matches!(&msg, Message::Command { cmd, params, .. } if cmd == "claim" && params == &["task-42"])
        );
    }

    #[test]
    fn parse_invalid_json_errors() {
        let result = parse_client_line(r#"{"type":"unknown_type"}"#, "r", "u");
        assert!(result.is_err());
    }

    #[test]
    fn parse_dm_envelope() {
        let raw = r#"{"type":"dm","to":"bob","content":"hey bob"}"#;
        let msg = parse_client_line(raw, "r", "alice").unwrap();
        assert!(
            matches!(&msg, Message::DirectMessage { to, content, .. } if to == "bob" && content == "hey bob")
        );
        assert_eq!(msg.user(), "alice");
    }
}
