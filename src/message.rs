use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Wire format for all messages stored in the chat file and sent over the socket.
///
/// Uses `#[serde(tag = "type")]` internally-tagged enum **without** `#[serde(flatten)]`
/// to avoid the serde flatten + internally-tagged footgun that breaks deserialization.
/// Every variant carries its own id/room/user/ts fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Join {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
    },
    Leave {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
    },
    Message {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        content: String,
    },
    Reply {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        reply_to: String,
        content: String,
    },
    Command {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        cmd: String,
        params: Vec<String>,
    },
    System {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        content: String,
    },
    /// A private direct message. Delivered only to sender, recipient, and the
    /// broker host. Always written to the chat history file.
    #[serde(rename = "dm")]
    DirectMessage {
        id: String,
        room: String,
        /// Sender username (set by the broker).
        user: String,
        ts: DateTime<Utc>,
        /// Recipient username.
        to: String,
        content: String,
    },
}

impl Message {
    pub fn id(&self) -> &str {
        match self {
            Self::Join { id, .. }
            | Self::Leave { id, .. }
            | Self::Message { id, .. }
            | Self::Reply { id, .. }
            | Self::Command { id, .. }
            | Self::System { id, .. }
            | Self::DirectMessage { id, .. } => id,
        }
    }

    pub fn room(&self) -> &str {
        match self {
            Self::Join { room, .. }
            | Self::Leave { room, .. }
            | Self::Message { room, .. }
            | Self::Reply { room, .. }
            | Self::Command { room, .. }
            | Self::System { room, .. }
            | Self::DirectMessage { room, .. } => room,
        }
    }

    pub fn user(&self) -> &str {
        match self {
            Self::Join { user, .. }
            | Self::Leave { user, .. }
            | Self::Message { user, .. }
            | Self::Reply { user, .. }
            | Self::Command { user, .. }
            | Self::System { user, .. }
            | Self::DirectMessage { user, .. } => user,
        }
    }

    pub fn ts(&self) -> &DateTime<Utc> {
        match self {
            Self::Join { ts, .. }
            | Self::Leave { ts, .. }
            | Self::Message { ts, .. }
            | Self::Reply { ts, .. }
            | Self::Command { ts, .. }
            | Self::System { ts, .. }
            | Self::DirectMessage { ts, .. } => ts,
        }
    }
}

// ── Constructors ─────────────────────────────────────────────────────────────

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn make_join(room: &str, user: &str) -> Message {
    Message::Join {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
    }
}

pub fn make_leave(room: &str, user: &str) -> Message {
    Message::Leave {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
    }
}

pub fn make_message(room: &str, user: &str, content: impl Into<String>) -> Message {
    Message::Message {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        content: content.into(),
    }
}

pub fn make_reply(
    room: &str,
    user: &str,
    reply_to: impl Into<String>,
    content: impl Into<String>,
) -> Message {
    Message::Reply {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        reply_to: reply_to.into(),
        content: content.into(),
    }
}

pub fn make_command(
    room: &str,
    user: &str,
    cmd: impl Into<String>,
    params: Vec<String>,
) -> Message {
    Message::Command {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        cmd: cmd.into(),
        params,
    }
}

pub fn make_system(room: &str, user: &str, content: impl Into<String>) -> Message {
    Message::System {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        content: content.into(),
    }
}

pub fn make_dm(room: &str, user: &str, to: &str, content: impl Into<String>) -> Message {
    Message::DirectMessage {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        to: to.to_owned(),
        content: content.into(),
    }
}

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

    fn fixed_ts() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 3, 5, 10, 0, 0).unwrap()
    }

    fn fixed_id() -> String {
        "00000000-0000-0000-0000-000000000001".to_owned()
    }

    // ── Round-trip tests ─────────────────────────────────────────────────────

    #[test]
    fn join_round_trips() {
        let msg = Message::Join {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn leave_round_trips() {
        let msg = Message::Leave {
            id: fixed_id(),
            room: "r".into(),
            user: "bob".into(),
            ts: fixed_ts(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn message_round_trips() {
        let msg = Message::Message {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            content: "hello world".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn reply_round_trips() {
        let msg = Message::Reply {
            id: fixed_id(),
            room: "r".into(),
            user: "bob".into(),
            ts: fixed_ts(),
            reply_to: "ffffffff-0000-0000-0000-000000000000".into(),
            content: "pong".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn command_round_trips() {
        let msg = Message::Command {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            cmd: "claim".into(),
            params: vec!["task-123".into(), "fix the bug".into()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn system_round_trips() {
        let msg = Message::System {
            id: fixed_id(),
            room: "r".into(),
            user: "broker".into(),
            ts: fixed_ts(),
            content: "5 users online".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    // ── JSON shape tests ─────────────────────────────────────────────────────

    #[test]
    fn join_json_has_type_field_at_top_level() {
        let msg = Message::Join {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "join");
        assert_eq!(v["user"], "alice");
        assert_eq!(v["room"], "r");
        assert!(
            v.get("content").is_none(),
            "join should not have content field"
        );
    }

    #[test]
    fn message_json_has_content_at_top_level() {
        let msg = Message::Message {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            content: "hi".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"], "hi");
    }

    #[test]
    fn deserialize_join_from_literal() {
        let raw = r#"{"type":"join","id":"abc","room":"myroom","user":"alice","ts":"2026-03-05T10:00:00Z"}"#;
        let msg: Message = serde_json::from_str(raw).unwrap();
        assert!(matches!(msg, Message::Join { .. }));
        assert_eq!(msg.user(), "alice");
    }

    #[test]
    fn deserialize_message_from_literal() {
        let raw = r#"{"type":"message","id":"abc","room":"r","user":"bob","ts":"2026-03-05T10:00:00Z","content":"yo"}"#;
        let msg: Message = serde_json::from_str(raw).unwrap();
        assert!(matches!(&msg, Message::Message { content, .. } if content == "yo"));
    }

    #[test]
    fn deserialize_command_with_empty_params() {
        let raw = r#"{"type":"command","id":"x","room":"r","user":"u","ts":"2026-03-05T10:00:00Z","cmd":"status","params":[]}"#;
        let msg: Message = serde_json::from_str(raw).unwrap();
        assert!(
            matches!(&msg, Message::Command { cmd, params, .. } if cmd == "status" && params.is_empty())
        );
    }

    // ── parse_client_line tests ───────────────────────────────────────────────

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

    #[test]
    fn dm_round_trips() {
        let msg = Message::DirectMessage {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            to: "bob".into(),
            content: "secret".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn dm_json_has_type_dm() {
        let msg = Message::DirectMessage {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            to: "bob".into(),
            content: "hi".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "dm");
        assert_eq!(v["to"], "bob");
        assert_eq!(v["content"], "hi");
    }

    // ── Accessor tests ────────────────────────────────────────────────────────

    #[test]
    fn accessors_return_correct_fields() {
        let ts = fixed_ts();
        let msg = Message::Message {
            id: fixed_id(),
            room: "testroom".into(),
            user: "carol".into(),
            ts,
            content: "x".into(),
        };
        assert_eq!(msg.id(), fixed_id());
        assert_eq!(msg.room(), "testroom");
        assert_eq!(msg.user(), "carol");
        assert_eq!(msg.ts(), &fixed_ts());
    }
}
