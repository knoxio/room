/// Oneshot command tests: send, poll, pull, /who, /set_status.
///
/// Tests verify that one-shot UDS connections (SEND:, TOKEN:, JOIN: prefixes)
/// correctly handle the send, poll, pull, and who subcommands.
mod common;

use std::time::Duration;

use common::{TestBroker, TestClient};
use room_cli::{
    broker::Broker,
    history,
    message::{self, Message},
    oneshot, paths,
};
use tokio::io::AsyncBufReadExt;
use tokio::time::timeout;

#[tokio::test]
async fn oneshot_set_status_returns_system_echo_not_eof() {
    let broker = TestBroker::start("t_set_status_oneshot").await;

    let mut watcher = TestClient::connect(&broker.socket_path, "watcher").await;
    watcher
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "watcher"))
        .await;

    let (_, token) = room_cli::oneshot::join_session(&broker.socket_path, "agent")
        .await
        .expect("join_session failed");

    let wire = serde_json::json!({
        "type": "command",
        "cmd": "set_status",
        "params": ["drafting auth handler"]
    })
    .to_string();

    // Before the fix this would fail with "EOF while parsing a value".
    let msg = room_cli::oneshot::send_message_with_token(&broker.socket_path, &token, &wire)
        .await
        .expect("send_message_with_token should not return EOF for set_status");

    assert!(
        matches!(&msg, Message::System { content, .. } if content.contains("drafting auth handler")),
        "expected System echo with status text, got: {msg:?}"
    );

    // The broadcast should also have reached the watcher.
    watcher
        .recv_until(|m| {
            matches!(m, Message::System { content, .. } if content.contains("drafting auth handler"))
        })
        .await;
}

/// who responds only to the requesting client with the current online list.
/// The response is a System message and is not broadcast to other clients.
#[tokio::test]
async fn who_responds_only_to_requester() {
    let broker = TestBroker::start("t_who").await;

    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // Bob sets a status so alice can verify it appears in who output
    bob.send_json(r#"{"type":"command","cmd":"set_status","params":["idle"]}"#)
        .await;
    // Drain the system broadcast on both sides
    alice
        .recv_until(|m| matches!(m, Message::System { content, .. } if content.contains("idle")))
        .await;
    bob.recv_until(|m| matches!(m, Message::System { content, .. } if content.contains("idle")))
        .await;

    // Alice sends /who
    alice
        .send_json(r#"{"type":"command","cmd":"who","params":[]}"#)
        .await;

    // Alice should receive a System message listing online users
    let response = alice
        .recv_until(|m| matches!(m, Message::System { content, .. } if content.contains("online")))
        .await;
    if let Message::System { content, .. } = &response {
        assert!(content.contains("alice"), "alice should be in who list");
        assert!(content.contains("bob"), "bob should be in who list");
        assert!(content.contains("idle"), "bob's status should appear");
    }

    // Bob should NOT receive the who response — it's private to alice.
    // We verify by checking that bob's next message (if any within 200ms) is not a System "online" response.
    let bob_got_who = timeout(Duration::from_millis(200), async {
        bob.recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("online")),
        )
        .await
    })
    .await;
    assert!(
        bob_got_who.is_err(),
        "who response should not be broadcast to other clients"
    );
}

// ── send / poll tests ─────────────────────────────────────────────────────────

/// `send_message` delivers a message to connected clients without generating
/// join or leave events for the sender.
#[tokio::test]
#[allow(deprecated)]
async fn send_delivers_message_without_join_leave() {
    let broker = TestBroker::start("t_send_no_join").await;

    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    room_cli::oneshot::send_message(&broker.socket_path, "bot", "hello from bot")
        .await
        .unwrap();

    // Alice receives the message content
    let msg = alice
        .recv_until(
            |m| matches!(m, Message::Message { content, .. } if content == "hello from bot"),
        )
        .await;
    assert_eq!(msg.user(), "bot");

    // No join event for "bot" should arrive
    let bot_join = timeout(Duration::from_millis(200), async {
        alice
            .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bot"))
            .await
    })
    .await;
    assert!(bot_join.is_err(), "bot should not generate a join event");
}

/// `send_message` returns a fully-populated Message with the correct fields.
#[tokio::test]
#[allow(deprecated)]
async fn send_returns_echo_json() {
    let broker = TestBroker::start("t_send_echo").await;

    let msg = room_cli::oneshot::send_message(&broker.socket_path, "bot", "test echo")
        .await
        .unwrap();

    assert!(
        matches!(&msg, Message::Message { content, .. } if content == "test echo"),
        "expected Message variant with correct content"
    );
    assert_eq!(msg.user(), "bot");
    assert_eq!(msg.room(), "t_send_echo");
    assert!(!msg.id().is_empty(), "message must have a non-empty id");
}

/// `poll_messages` returns only messages that come after the given `since` ID.
#[tokio::test]
async fn poll_returns_messages_since_id() {
    let broker = TestBroker::start("t_poll_since").await;

    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    alice.send_text("msg1").await;
    let m1 = alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "msg1"))
        .await;
    alice.send_text("msg2").await;
    alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "msg2"))
        .await;
    alice.send_text("msg3").await;
    alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "msg3"))
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cursor_path = dir.path().join("test.cursor");

    let msgs = room_cli::oneshot::poll_messages(
        &broker.chat_path,
        &cursor_path,
        None,
        None,
        Some(m1.id()),
    )
    .await
    .unwrap();

    let contents: Vec<&str> = msgs
        .iter()
        .filter_map(|m| {
            if let Message::Message { content, .. } = m {
                Some(content.as_str())
            } else {
                None
            }
        })
        .collect();

    assert!(contents.contains(&"msg2"), "expected msg2 in results");
    assert!(contents.contains(&"msg3"), "expected msg3 in results");
    assert!(!contents.contains(&"msg1"), "msg1 should be excluded");
}

/// After a poll the cursor file holds the last message ID; a subsequent poll
/// with no explicit `since` returns nothing (already seen).
#[tokio::test]
async fn poll_updates_cursor_file() {
    let broker = TestBroker::start("t_poll_cursor").await;

    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    alice.send_text("cursor test").await;
    alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "cursor test"))
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cursor_path = dir.path().join("alice.cursor");

    // First poll: returns messages, writes cursor
    let msgs = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_path, None, None, None)
        .await
        .unwrap();
    assert!(!msgs.is_empty(), "first poll should return messages");
    assert!(
        cursor_path.exists(),
        "cursor file must be written after poll"
    );

    // Second poll: cursor is up to date, nothing new
    let msgs2 = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_path, None, None, None)
        .await
        .unwrap();
    assert!(
        msgs2.is_empty(),
        "second poll with current cursor should return nothing"
    );
}

/// `poll_messages` works without a running broker — it reads the chat file directly.
#[tokio::test]
async fn poll_with_no_broker_reads_file_directly() {
    let dir = tempfile::tempdir().unwrap();
    let chat_path = dir.path().join("offline.chat");
    let cursor_path = dir.path().join("offline.cursor");

    let msg = room_cli::message::make_message("offline", "ghost", "written directly");
    room_cli::history::append(&chat_path, &msg).await.unwrap();

    let msgs = room_cli::oneshot::poll_messages(&chat_path, &cursor_path, None, None, None)
        .await
        .unwrap();

    assert_eq!(msgs.len(), 1);
    assert!(matches!(&msgs[0], Message::Message { content, .. } if content == "written directly"));
}

/// `send_message` returns an error when no broker socket exists.
#[tokio::test]
#[allow(deprecated)]
async fn send_fails_when_no_broker() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("nonexistent.sock");

    let result = room_cli::oneshot::send_message(&socket_path, "bot", "hello").await;
    assert!(
        result.is_err(),
        "send should fail when no broker is running"
    );
}

/// One-shot SEND with a DM envelope routes the message selectively: recipient and host
/// receive it, bystanders do not. Also verifies the DM is persisted to history.
/// (Tests broker routing of a `{"type":"dm",...}` envelope over the one-shot SEND path;
/// see `cmd_send` in `oneshot.rs` for the CLI layer that builds this envelope from `--to`.)
#[tokio::test]
#[allow(deprecated)]
async fn oneshot_send_dm_is_routed_privately() {
    let broker = TestBroker::start("t_oneshot_dm").await;

    // alice connects first → she becomes the host
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    let mut carol = TestClient::connect(&broker.socket_path, "carol").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "carol"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "carol"))
        .await;
    carol
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "carol"))
        .await;

    // "agent" sends a one-shot DM to bob
    let wire = serde_json::json!({"type": "dm", "to": "bob", "content": "secret"}).to_string();
    room_cli::oneshot::send_message(&broker.socket_path, "agent", &wire)
        .await
        .unwrap();

    // bob (recipient) receives the DM
    let msg = bob
        .recv_until(|m| matches!(m, Message::DirectMessage { content, .. } if content == "secret"))
        .await;
    assert_eq!(msg.user(), "agent");
    if let Message::DirectMessage { to, .. } = &msg {
        assert_eq!(to, "bob");
    }

    // alice (host) receives the DM
    alice
        .recv_until(|m| matches!(m, Message::DirectMessage { content, .. } if content == "secret"))
        .await;

    // carol (bystander) must NOT receive it
    let carol_got_dm = timeout(Duration::from_millis(300), async {
        let mut line = String::new();
        loop {
            line.clear();
            if carol.reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break false;
            }
            if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                if matches!(&msg, Message::DirectMessage { content, .. } if content == "secret") {
                    break true;
                }
            }
        }
    })
    .await;
    assert!(
        carol_got_dm.is_err() || !carol_got_dm.unwrap(),
        "carol should not receive the one-shot DM"
    );

    // DM is persisted to history. No sleep needed: send_message awaits the broker echo,
    // and the echo is only written after dm_and_persist (which awaits history::append) returns.
    let history = history::load(&broker.chat_path).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| matches!(m, Message::DirectMessage { content, .. } if content == "secret")),
        "one-shot DM not found in history"
    );
}

/// `poll_messages` with a viewer username filters out DMs the viewer is not party to.
#[tokio::test]
async fn poll_filters_dm_for_non_party_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let chat_path = dir.path().join("poll_dm_filter.chat");
    let cursor_path = dir.path().join("carol.cursor");

    // Write a DM between alice and bob directly into the chat file
    let dm = room_cli::message::make_dm("r", "alice", "bob", "eyes only");
    room_cli::history::append(&chat_path, &dm).await.unwrap();

    // carol is not party to the DM — she should see nothing
    let msgs =
        room_cli::oneshot::poll_messages(&chat_path, &cursor_path, Some("carol"), None, None)
            .await
            .unwrap();
    assert!(
        msgs.is_empty(),
        "carol should not see a DM she is not party to"
    );

    // alice (sender) should see it
    let alice_cursor = dir.path().join("alice.cursor");
    let msgs =
        room_cli::oneshot::poll_messages(&chat_path, &alice_cursor, Some("alice"), None, None)
            .await
            .unwrap();
    assert_eq!(msgs.len(), 1, "alice should see the DM she sent");

    // bob (recipient) should see it
    let bob_cursor = dir.path().join("bob.cursor");
    let msgs = room_cli::oneshot::poll_messages(&chat_path, &bob_cursor, Some("bob"), None, None)
        .await
        .unwrap();
    assert_eq!(msgs.len(), 1, "bob should see the DM addressed to him");
}

/// DMs in history are not replayed to clients who are not the sender, recipient, or host.
#[tokio::test]
async fn history_replay_filters_dm_for_non_party() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("replay_dm.sock");
    let chat_path = dir.path().join("replay_dm.chat");

    // Pre-seed history with a DM between bob and carol (alice will be host later)
    let dm = room_cli::message::make_dm("replay_dm", "bob", "carol", "for bob and carol only");
    room_cli::history::append(&chat_path, &dm).await.unwrap();

    let broker = Broker::new(
        "replay_dm",
        chat_path.clone(),
        chat_path.with_extension("tokens"),
        chat_path.with_extension("subscriptions"),
        socket_path.clone(),
        None,
    );
    tokio::spawn(async move { broker.run().await.ok() });
    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // alice connects first → becomes host; she should receive the DM in history replay
    let mut alice = TestClient::connect(&socket_path, "alice").await;
    let mut pre_join_alice: Vec<Message> = Vec::new();
    loop {
        let msg = alice.recv().await;
        if matches!(&msg, Message::Join { user, .. } if user == "alice") {
            break;
        }
        pre_join_alice.push(msg);
    }
    assert!(
        pre_join_alice
            .iter()
            .any(|m| matches!(m, Message::DirectMessage { content, .. } if content == "for bob and carol only")),
        "host alice should receive the DM in history replay"
    );

    // dan is a bystander — should NOT receive the DM in history replay
    let mut dan = TestClient::connect(&socket_path, "dan").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "dan"))
        .await;
    let mut pre_join_dan: Vec<Message> = Vec::new();
    loop {
        let msg = dan.recv().await;
        if matches!(&msg, Message::Join { user, .. } if user == "dan") {
            break;
        }
        pre_join_dan.push(msg);
    }
    assert!(
        !pre_join_dan.iter().any(
            |m| matches!(m, Message::DirectMessage { content, .. } if content == "for bob and carol only")
        ),
        "bystander dan should not receive the DM in history replay"
    );
}

/// Users removed from status map on disconnect do not appear in subsequent who responses.
#[tokio::test]
async fn who_excludes_disconnected_users() {
    let broker = TestBroker::start("t_who_disconnect").await;

    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // Bob disconnects
    drop(bob);
    alice
        .recv_until(|m| matches!(m, Message::Leave { user, .. } if user == "bob"))
        .await;

    // Alice queries who — bob should not appear
    alice
        .send_json(r#"{"type":"command","cmd":"who","params":[]}"#)
        .await;

    let response = alice
        .recv_until(|m| matches!(m, Message::System { content, .. } if content.contains("online")))
        .await;
    if let Message::System { content, .. } = &response {
        assert!(
            !content.contains("bob"),
            "disconnected user should not appear in who list"
        );
        assert!(
            content.contains("alice"),
            "alice should still be in who list"
        );
    }
}

#[tokio::test]
async fn pull_messages_returns_last_n() {
    let broker = TestBroker::start("t_pull_last_n").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    for i in 0..5 {
        alice.send_text(&format!("msg {i}")).await;
        alice
            .recv_until(
                |m| matches!(m, Message::Message { content, .. } if content == &format!("msg {i}")),
            )
            .await;
    }

    let msgs = room_cli::oneshot::pull_messages(&broker.chat_path, 3, None, None)
        .await
        .unwrap();
    // Last 3 of 5 messages sent
    let contents: Vec<&str> = msgs
        .iter()
        .filter_map(|m| {
            if let Message::Message { content, .. } = m {
                Some(content.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(contents, ["msg 2", "msg 3", "msg 4"]);
}

/// `pull_messages` returns all messages when history is shorter than N.
#[tokio::test]
async fn pull_messages_returns_all_when_fewer_than_n() {
    let broker = TestBroker::start("t_pull_short").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    alice.send_text("only message").await;
    alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "only message"))
        .await;

    let msgs = room_cli::oneshot::pull_messages(&broker.chat_path, 100, None, None)
        .await
        .unwrap();
    assert!(
        msgs.iter()
            .any(|m| matches!(m, Message::Message { content, .. } if content == "only message")),
        "expected the single message to be returned"
    );
}

/// `pull_messages` returns an empty vec for an empty history file.
#[tokio::test]
async fn pull_messages_empty_history_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let chat_path = dir.path().join("empty.chat");
    // file does not exist yet
    let msgs = room_cli::oneshot::pull_messages(&chat_path, 20, None, None)
        .await
        .unwrap();
    assert!(msgs.is_empty());
}

/// `cmd_pull` does not advance the poll cursor.
///
/// Registers a user, polls to establish the canonical cursor at
/// `/tmp/room-<id>-<username>.cursor`, then calls `cmd_pull` and asserts
/// the cursor file is unchanged. A subsequent poll must still return the
/// message that was sent after the initial poll.
#[tokio::test]
async fn pull_messages_does_not_update_cursor() {
    let room_id = "t_pull_cursor_e2e";
    let broker = TestBroker::start(room_id).await;

    // Write the meta file so cmd_poll / cmd_pull can locate the chat file.
    paths::ensure_room_dirs().unwrap();
    let meta_path = paths::room_meta_path(room_id);
    let meta = serde_json::json!({ "chat_path": broker.chat_path.to_string_lossy() });
    std::fs::write(&meta_path, format!("{meta}\n")).unwrap();

    // Join to obtain a token and write the token file.
    let (_user, token) = room_cli::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .unwrap();
    let token_path = room_cli::paths::global_token_path("alice");
    let token_data = serde_json::json!({ "username": "alice", "token": token });
    std::fs::write(&token_path, format!("{token_data}\n")).unwrap();

    // Send first message via one-shot.
    room_cli::oneshot::send_message_with_token(
        &broker.socket_path,
        &token,
        r#"{"type":"message","content":"first"}"#,
    )
    .await
    .unwrap();

    // cmd_poll advances the canonical cursor.
    room_cli::oneshot::cmd_poll(room_id, &token, None, false)
        .await
        .unwrap();

    let cursor_path = paths::cursor_path(room_id, "alice");
    let cursor_after_poll = std::fs::read_to_string(&cursor_path).unwrap();

    // Send a second message after the cursor.
    room_cli::oneshot::send_message_with_token(
        &broker.socket_path,
        &token,
        r#"{"type":"message","content":"second"}"#,
    )
    .await
    .unwrap();

    // cmd_pull must not move the cursor.
    room_cli::oneshot::cmd_pull(room_id, &token, 5)
        .await
        .unwrap();

    let cursor_after_pull = std::fs::read_to_string(&cursor_path).unwrap();
    assert_eq!(
        cursor_after_poll, cursor_after_pull,
        "cmd_pull must not advance the poll cursor"
    );

    // Verify poll still returns "second" (cursor was not consumed by pull).
    let msgs = room_cli::oneshot::poll_messages(
        &broker.chat_path,
        &cursor_path,
        Some("alice"),
        None,
        Some(&cursor_after_poll),
    )
    .await
    .unwrap();
    assert!(
        msgs.iter()
            .any(|m| matches!(m, Message::Message { content, .. } if content == "second")),
        "second message must still be available after pull"
    );

    // Clean up /tmp files written by this test.
    let _ = std::fs::remove_file(&meta_path);
    let _ = std::fs::remove_file(&token_path);
    let _ = std::fs::remove_file(&cursor_path);
}

/// `pull_messages` with a viewer filters out DMs the viewer is not party to.
#[tokio::test]
async fn pull_messages_filters_dms_for_viewer() {
    let broker = TestBroker::start("t_pull_dm_filter").await;

    // alice connects (becomes host)
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // bob connects
    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // alice sends a plain message
    alice.send_text("hi all").await;
    alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "hi all"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Message { content, .. } if content == "hi all"))
        .await;

    // bob DMs alice (only alice/bob/host should see it)
    let dm = serde_json::json!({"type": "dm", "to": "alice", "content": "secret"}).to_string();
    bob.send_json(&dm).await;
    bob.recv_until(|m| matches!(m, Message::DirectMessage { content, .. } if content == "secret"))
        .await;

    // carol (a third party) pulls history — should not see the DM
    let carol_msgs = room_cli::oneshot::pull_messages(&broker.chat_path, 50, Some("carol"), None)
        .await
        .unwrap();
    assert!(
        !carol_msgs
            .iter()
            .any(|m| matches!(m, Message::DirectMessage { content, .. } if content == "secret")),
        "carol should not see the DM between alice and bob"
    );

    // alice pulls — should see the DM
    let alice_msgs = room_cli::oneshot::pull_messages(&broker.chat_path, 50, Some("alice"), None)
        .await
        .unwrap();
    assert!(
        alice_msgs
            .iter()
            .any(|m| matches!(m, Message::DirectMessage { content, .. } if content == "secret")),
        "alice should see the DM addressed to her"
    );
}

// ── SEND: deprecation tests (#467) ──────────────────────────────────────────

/// The deprecated `SEND:` handshake still delivers messages (backward compat).
/// This test uses the raw UDS protocol to verify the broker still processes
/// `SEND:<username>` connections correctly, even though the handshake is
/// deprecated in favor of `TOKEN:<uuid>`.
#[tokio::test]
async fn deprecated_send_handshake_still_works() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let broker = TestBroker::start("t_send_deprecated").await;

    // Connect using raw SEND: handshake.
    let stream = UnixStream::connect(&broker.socket_path).await.unwrap();
    let (read_half, mut write_half) = stream.into_split();

    write_half.write_all(b"SEND:legacy-bot\n").await.unwrap();
    write_half
        .write_all(b"hello from deprecated path\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let echo: Message = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(echo.user(), "legacy-bot");
    assert!(
        matches!(&echo, Message::Message { content, .. } if content == "hello from deprecated path"),
        "SEND: handshake should still deliver messages"
    );
}

/// The deprecated `send_message` function still works when called with
/// `#[allow(deprecated)]`. Verifies backward compatibility is preserved.
#[tokio::test]
async fn deprecated_send_message_fn_still_delivers() {
    let broker = TestBroker::start("t_send_fn_deprecated").await;

    #[allow(deprecated)]
    let msg = room_cli::oneshot::send_message(&broker.socket_path, "old-bot", "still works")
        .await
        .unwrap();

    assert_eq!(msg.user(), "old-bot");
    assert!(
        matches!(&msg, Message::Message { content, .. } if content == "still works"),
        "deprecated send_message should still deliver messages"
    );
}
