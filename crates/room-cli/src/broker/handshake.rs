//! Handshake protocol parsing for UDS and WebSocket connections.
//!
//! The room wire protocol uses a text-based handshake as the first
//! line (UDS) or first frame (WebSocket) of every connection. This module
//! provides typed parsers for both protocol layers:
//!
//! ## Per-room client handshake
//!
//! The first line after a room is established can be:
//!
//! | Prefix | Variant | Meaning |
//! |---|---|---|
//! | `SEND:<username>` | [`ClientHandshake::Send`] | Legacy unauthenticated one-shot send |
//! | `TOKEN:<uuid>` | [`ClientHandshake::Token`] | Token-authenticated one-shot send |
//! | `JOIN:<username>` | [`ClientHandshake::Join`] | Register username, receive session token |
//! | `<username>` | [`ClientHandshake::Interactive`] | Full interactive join |
//!
//! ## Daemon-level prefix
//!
//! The first line of a connection to the multi-room daemon is:
//!
//! | Prefix | Variant | Meaning |
//! |---|---|---|
//! | `CREATE:<room_id>` | [`DaemonPrefix::Create`] | Create a new room |
//! | `DESTROY:<room_id>` | [`DaemonPrefix::Destroy`] | Destroy an existing room |
//! | `ROOM:<room_id>:<rest>` | [`DaemonPrefix::Room`] | Route to an existing room |
//! | anything else | [`DaemonPrefix::Unknown`] | Rejected with an error response |

/// Typed result of parsing the first line of a per-room connection.
#[derive(Debug, PartialEq)]
pub(crate) enum ClientHandshake {
    /// `SEND:<username>` — legacy unauthenticated one-shot send.
    Send(String),
    /// `TOKEN:<uuid>` — token-authenticated one-shot send.
    Token(String),
    /// `JOIN:<username>` — register username, receive a session token.
    Join(String),
    /// `<username>` — full interactive session.
    Interactive(String),
}

/// Parse the first line of a per-room connection into a typed handshake.
///
/// Recognition order: `SEND:` → `TOKEN:` → `JOIN:` → Interactive.
/// Whitespace has already been trimmed by the caller.
pub(crate) fn parse_client_handshake(line: &str) -> ClientHandshake {
    if let Some(u) = line.strip_prefix("SEND:") {
        return ClientHandshake::Send(u.to_owned());
    }
    if let Some(t) = line.strip_prefix("TOKEN:") {
        return ClientHandshake::Token(t.to_owned());
    }
    if let Some(u) = line.strip_prefix("JOIN:") {
        return ClientHandshake::Join(u.to_owned());
    }
    ClientHandshake::Interactive(line.to_owned())
}

/// Typed result of parsing the first line of a daemon connection.
#[derive(Debug, PartialEq)]
pub(crate) enum DaemonPrefix {
    /// `CREATE:<room_id>` — create a new room (config follows on the next line).
    Create(String),
    /// `DESTROY:<room_id>` — destroy an existing room.
    Destroy(String),
    /// `ROOM:<room_id>:<rest>` — route to an existing room.
    ///
    /// `rest` is everything after the second colon: `JOIN:alice`, `TOKEN:uuid`,
    /// `SEND:bob`, or a plain username for an interactive join.
    Room { room_id: String, rest: String },
    /// Unrecognised prefix — the connection should be rejected with an error.
    Unknown,
}

/// Parse the first line of a daemon connection into a typed prefix.
///
/// Recognition order: `DESTROY:` → `CREATE:` → `ROOM:` → Unknown.
/// Whitespace has already been trimmed by the caller.
pub(crate) fn parse_daemon_prefix(line: &str) -> DaemonPrefix {
    if let Some(room_id) = line.strip_prefix("DESTROY:") {
        return DaemonPrefix::Destroy(room_id.to_owned());
    }
    if let Some(room_id) = line.strip_prefix("CREATE:") {
        return DaemonPrefix::Create(room_id.to_owned());
    }
    if let Some(stripped) = line.strip_prefix("ROOM:") {
        if let Some(colon) = stripped.find(':') {
            let room_id = &stripped[..colon];
            if !room_id.is_empty() {
                let rest = stripped[colon + 1..].to_owned();
                return DaemonPrefix::Room {
                    room_id: room_id.to_owned(),
                    rest,
                };
            }
        }
    }
    DaemonPrefix::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_client_handshake ────────────────────────────────────────────

    #[test]
    fn client_send_prefix() {
        assert_eq!(
            parse_client_handshake("SEND:alice"),
            ClientHandshake::Send("alice".into())
        );
    }

    #[test]
    fn client_token_prefix() {
        assert_eq!(
            parse_client_handshake("TOKEN:abc-123"),
            ClientHandshake::Token("abc-123".into())
        );
    }

    #[test]
    fn client_join_prefix() {
        assert_eq!(
            parse_client_handshake("JOIN:bob"),
            ClientHandshake::Join("bob".into())
        );
    }

    #[test]
    fn client_interactive_plain_username() {
        assert_eq!(
            parse_client_handshake("alice"),
            ClientHandshake::Interactive("alice".into())
        );
    }

    #[test]
    fn client_interactive_empty_string() {
        assert_eq!(
            parse_client_handshake(""),
            ClientHandshake::Interactive("".into())
        );
    }

    #[test]
    fn client_send_empty_username() {
        assert_eq!(
            parse_client_handshake("SEND:"),
            ClientHandshake::Send("".into())
        );
    }

    // ── parse_daemon_prefix ───────────────────────────────────────────────

    #[test]
    fn daemon_create_prefix() {
        assert_eq!(
            parse_daemon_prefix("CREATE:newroom"),
            DaemonPrefix::Create("newroom".into())
        );
    }

    #[test]
    fn daemon_destroy_prefix() {
        assert_eq!(
            parse_daemon_prefix("DESTROY:myroom"),
            DaemonPrefix::Destroy("myroom".into())
        );
    }

    #[test]
    fn daemon_room_join() {
        assert_eq!(
            parse_daemon_prefix("ROOM:myroom:JOIN:alice"),
            DaemonPrefix::Room {
                room_id: "myroom".into(),
                rest: "JOIN:alice".into()
            }
        );
    }

    #[test]
    fn daemon_room_token() {
        assert_eq!(
            parse_daemon_prefix("ROOM:myroom:TOKEN:abc-123"),
            DaemonPrefix::Room {
                room_id: "myroom".into(),
                rest: "TOKEN:abc-123".into()
            }
        );
    }

    #[test]
    fn daemon_room_send() {
        assert_eq!(
            parse_daemon_prefix("ROOM:myroom:SEND:bob"),
            DaemonPrefix::Room {
                room_id: "myroom".into(),
                rest: "SEND:bob".into()
            }
        );
    }

    #[test]
    fn daemon_room_interactive() {
        assert_eq!(
            parse_daemon_prefix("ROOM:chat:alice"),
            DaemonPrefix::Room {
                room_id: "chat".into(),
                rest: "alice".into()
            }
        );
    }

    #[test]
    fn daemon_room_id_with_hyphens() {
        assert_eq!(
            parse_daemon_prefix("ROOM:agent-room-2:JOIN:r2d2"),
            DaemonPrefix::Room {
                room_id: "agent-room-2".into(),
                rest: "JOIN:r2d2".into()
            }
        );
    }

    #[test]
    fn daemon_room_empty_rest() {
        assert_eq!(
            parse_daemon_prefix("ROOM:myroom:"),
            DaemonPrefix::Room {
                room_id: "myroom".into(),
                rest: "".into()
            }
        );
    }

    #[test]
    fn daemon_unknown_plain_join() {
        assert_eq!(parse_daemon_prefix("JOIN:alice"), DaemonPrefix::Unknown);
    }

    #[test]
    fn daemon_unknown_plain_username() {
        assert_eq!(parse_daemon_prefix("alice"), DaemonPrefix::Unknown);
    }

    #[test]
    fn daemon_unknown_token_without_room() {
        assert_eq!(parse_daemon_prefix("TOKEN:abc"), DaemonPrefix::Unknown);
    }

    #[test]
    fn daemon_room_empty_room_id_is_unknown() {
        // "ROOM::JOIN:alice" — room_id is empty string, should be Unknown.
        assert_eq!(
            parse_daemon_prefix("ROOM::JOIN:alice"),
            DaemonPrefix::Unknown
        );
    }
}
