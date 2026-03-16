/// Auth and error edge-case tests.
///
/// Covers: bare-username (deprecated) interactive sessions, path traversal in
/// room IDs, JSON metacharacter preservation in message content, long username
/// handling, broker kill recovery (client disconnect without hang), and chat
/// file deletion mid-session.
mod common;

use std::time::Duration;

use common::{daemon_create, daemon_global_join, TestBroker, TestDaemon};
use room_cli::{history, message::Message};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

// ── bare username interactive session ─────────────────────────────────────────

/// Connecting with a bare username (the deprecated interactive handshake path)
/// still works: the broker accepts the connection and broadcasts a join event
/// containing the supplied username.
///
/// `TestClient::connect` uses this path — it sends `<username>\n` directly,
/// which is the unauthenticated interactive handshake documented in the broker
/// handshake module.
#[tokio::test]
async fn bare_username_session_compat() {
    let broker = TestBroker::start("t_edge_bare_username").await;

    // Connect using raw bare-username handshake (no SESSION:/TOKEN: prefix).
    let stream = UnixStream::connect(&broker.socket_path)
        .await
        .expect("could not connect to broker");
    let (r, mut w) = stream.into_split();
    w.write_all(b"legacy-user\n").await.unwrap();

    let mut reader = tokio::io::BufReader::new(r);
    // Drain lines until we see the join event for "legacy-user".
    let join = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read error");
            if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                if matches!(&msg, Message::Join { user, .. } if user == "legacy-user") {
                    return msg;
                }
            }
        }
    })
    .await
    .expect("timed out waiting for join event");

    assert!(
        matches!(&join, Message::Join { user, .. } if user == "legacy-user"),
        "bare username handshake must produce a join event: {join:?}"
    );

    // Also verify the user can send a plain-text message in this session.
    w.write_all(b"hello from legacy\n").await.unwrap();

    let _echo = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read error");
            if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                if matches!(&msg, Message::Message { content, .. } if content == "hello from legacy") {
                    return msg;
                }
            }
        }
    })
    .await
    .expect("timed out waiting for message echo");
}

// ── path traversal in room ID ─────────────────────────────────────────────────

/// Attempting to create a room whose ID contains `..` (path traversal) via the
/// daemon CREATE: protocol must be rejected with `invalid_room_id`.
///
/// This guards against an attacker naming a room `../etc` to escape the data
/// directory when the broker writes chat/token files.
#[tokio::test]
async fn path_traversal_in_room_id_rejected() {
    let td = TestDaemon::start(&[]).await;
    let token = daemon_global_join(&td.socket_path, "edge-admin").await;

    // Classic `..` traversal component.
    let resp = daemon_create(
        &td.socket_path,
        "../etc",
        r#"{"visibility":"public"}"#,
        &token,
    )
    .await;
    assert_eq!(
        resp["type"], "error",
        "path traversal room ID should be rejected: {resp}"
    );
    assert_eq!(
        resp["code"], "invalid_room_id",
        "error code must be invalid_room_id: {resp}"
    );

    // Embedded traversal — not at the start.
    let resp2 = daemon_create(
        &td.socket_path,
        "room/../etc",
        r#"{"visibility":"public"}"#,
        &token,
    )
    .await;
    assert_eq!(
        resp2["type"], "error",
        "embedded traversal should also be rejected: {resp2}"
    );
    assert_eq!(resp2["code"], "invalid_room_id");

    // Forward slash alone — unsafe character.
    let resp3 = daemon_create(
        &td.socket_path,
        "room/sub",
        r#"{"visibility":"public"}"#,
        &token,
    )
    .await;
    assert_eq!(
        resp3["type"], "error",
        "slash in room ID should be rejected: {resp3}"
    );
    assert_eq!(resp3["code"], "invalid_room_id");
}

// ── JSON metacharacters in message content ────────────────────────────────────

/// Sending a message whose content contains JSON metacharacters (double quotes,
/// curly braces, backslashes) must preserve those characters as-is in the
/// broadcast echo.  The broker must not interpret or mangle the payload.
#[tokio::test]
async fn json_injection_in_message_content() {
    let broker = TestBroker::start("t_edge_json_injection").await;

    // Join as a watcher to receive the broadcast.
    let mut watcher = common::TestClient::connect(&broker.socket_path, "watcher").await;
    watcher
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "watcher"))
        .await;

    // Get a sender token.
    let (_, token) = room_cli::oneshot::join_session(&broker.socket_path, "sender")
        .await
        .expect("join_session failed");

    // Content with JSON metacharacters.
    let tricky = r#"{"key":"val\"ue","nested":{"a":1},"bs":"back\\slash"}"#;
    let wire = serde_json::json!({"type": "message", "content": tricky}).to_string();

    let echo = room_cli::oneshot::send_message_with_token(&broker.socket_path, &token, &wire)
        .await
        .expect("send failed");

    assert!(
        matches!(&echo, Message::Message { content, .. } if content == tricky),
        "broker must preserve JSON metacharacters verbatim; got: {echo:?}"
    );

    // The watcher must also see the content unchanged.
    let received = watcher
        .recv_until(|m| matches!(m, Message::Message { user, .. } if user == "sender"))
        .await;
    assert!(
        matches!(&received, Message::Message { content, .. } if content == tricky),
        "broadcast must preserve metacharacters; got: {received:?}"
    );
}

// ── long username handling ────────────────────────────────────────────────────

/// Joining with a username that exceeds 256 characters must either succeed or
/// fail with a structured error — no panic, no hang, no silent data corruption.
///
/// The test accepts both outcomes; what it guards against is a crash or
/// deadlock in the broker when a very long username is submitted.
#[tokio::test]
async fn long_username_handling() {
    let broker = TestBroker::start("t_edge_long_username").await;

    // 260-character username — well over any reasonable limit.
    let long_name = "x".repeat(260);

    // `join_session` issues a `JOIN:` handshake.  The broker may accept or reject
    // the username, but must respond within the timeout and not crash.
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        room_cli::oneshot::join_session(&broker.socket_path, &long_name),
    )
    .await;

    assert!(
        result.is_ok(),
        "join_session must complete (not hang) for a long username"
    );

    // Whether the join succeeded or failed, the broker must still accept new
    // connections (it hasn't crashed).
    let still_alive = room_cli::oneshot::join_session(&broker.socket_path, "short-name").await;
    assert!(
        still_alive.is_ok(),
        "broker must still accept connections after a long-username join attempt"
    );
}

// ── broker kill recovery ──────────────────────────────────────────────────────

/// A connected interactive client must receive EOF (not hang indefinitely)
/// when the remote end of the socket is closed.
///
/// This test models broker-side socket closure by sending `/exit` so the broker
/// closes all client connections cleanly, then verifies the client side sees EOF
/// within a bounded timeout.  The concern is not the exit mechanism itself but
/// that the client read loop terminates rather than blocking forever.
#[tokio::test]
async fn broker_kill_recovery() {
    let broker = TestBroker::start("t_edge_broker_kill").await;

    // First client: the "host" that must connect first to acquire host privileges.
    // The broker grants admin permissions to the first connected interactive user.
    let mut admin = common::TestClient::connect(&broker.socket_path, "host").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "host"))
        .await;

    // Second client: the "victim" that will observe the disconnect.
    let stream = UnixStream::connect(&broker.socket_path)
        .await
        .expect("could not connect");
    let (r, mut w) = stream.into_split();
    w.write_all(b"victim\n").await.unwrap();

    let mut reader = tokio::io::BufReader::new(r);

    // Drain the join event so we know the session is active.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.ok();
            if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                if matches!(&msg, Message::Join { user, .. } if user == "victim") {
                    return;
                }
            }
        }
    })
    .await
    .expect("timed out waiting for join event");

    // Trigger broker shutdown — this closes all connected sockets.
    admin
        .send_json(r#"{"type":"command","cmd":"exit","params":[]}"#)
        .await;

    // The victim's next read must return 0 bytes (EOF) or an error — not hang.
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => return Ok(0usize), // clean EOF
                Ok(_) => {
                    // Drain residual messages (e.g. join events, system messages)
                    // until we hit EOF.
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "client read loop must not hang after broker /exit — got timeout"
    );
    // Either EOF (Ok(0)) or a read error are both valid disconnect signals.
    match result.unwrap() {
        Ok(0) => { /* clean EOF — expected */ }
        Ok(_) => unreachable!("loop only returns on EOF or error"),
        Err(_) => { /* ECONNRESET or similar — acceptable */ }
    }
}

// ── chat file deleted mid-session ─────────────────────────────────────────────

/// Deleting the chat file while a broker is running must not crash the broker.
/// A subsequent send must either succeed (broker recreates the file) or fail
/// gracefully — no panic, no hang, no silent data loss on the next message.
#[tokio::test]
async fn chat_file_deleted_mid_session() {
    let broker = TestBroker::start("t_edge_chat_delete").await;

    let mut alice = common::TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // Send a first message — broker writes it to the chat file.
    alice.send_text("before delete").await;
    alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "before delete"))
        .await;

    // Verify the chat file exists and has content.
    let before = history::load(&broker.chat_path)
        .await
        .expect("history::load failed before delete");
    assert!(
        !before.is_empty(),
        "chat file should have at least one entry before delete"
    );

    // Delete the chat file while the broker is still running.
    std::fs::remove_file(&broker.chat_path).expect("could not delete chat file");

    // Give the broker a brief moment to notice (file descriptor stays open
    // on Linux, so this mainly tests the write path on the next message).
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send a second message — broker must not crash or hang.
    let send_result = tokio::time::timeout(Duration::from_secs(3), async {
        alice.send_text("after delete").await;
        alice
            .recv_until(
                |m| matches!(m, Message::Message { content, .. } if content == "after delete"),
            )
            .await
    })
    .await;

    assert!(
        send_result.is_ok(),
        "broker must not hang after chat file deletion"
    );
}
