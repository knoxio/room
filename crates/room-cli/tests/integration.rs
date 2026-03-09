/// Integration tests for the room broker.
///
/// Each test spins up a real broker (bound to a temp socket), connects raw
/// Unix-socket clients, and verifies behaviour at the wire level.
mod common;

use std::{path::PathBuf, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use room_cli::{
    broker::Broker,
    history,
    message::{self, Message},
    paths,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};
use tokio_tungstenite::{connect_async, tungstenite::Message as TungsteniteMsg};

// ── Helpers ───────────────────────────────────────────────────────────────────

struct TestBroker {
    pub socket_path: PathBuf,
    pub chat_path: PathBuf,
    /// Keep TempDir alive for the duration of the test.
    _dir: TempDir,
}

impl TestBroker {
    /// Start a broker and wait until the socket is ready.
    async fn start(room_id: &str) -> Self {
        Self::start_inner(room_id, None).await
    }

    /// Start a broker with both UDS and WebSocket/REST transport.
    /// Returns (TestBroker, ws_port).
    async fn start_with_ws(room_id: &str) -> (Self, u16) {
        let port = common::free_port();
        let broker = Self::start_inner(room_id, Some(port)).await;
        common::wait_for_tcp(port, Duration::from_secs(1)).await;
        (broker, port)
    }

    async fn start_inner(room_id: &str, ws_port: Option<u16>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join(format!("{room_id}.sock"));
        let chat_path = dir.path().join(format!("{room_id}.chat"));

        let broker = Broker::new(
            room_id,
            chat_path.clone(),
            chat_path.with_extension("tokens"),
            chat_path.with_extension("subscriptions"),
            socket_path.clone(),
            ws_port,
        );
        tokio::spawn(async move {
            broker.run().await.ok();
        });

        common::wait_for_socket(&socket_path, Duration::from_secs(1)).await;

        Self {
            socket_path,
            chat_path,
            _dir: dir,
        }
    }
}

struct TestClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl TestClient {
    async fn connect(socket_path: &PathBuf, username: &str) -> Self {
        let stream = UnixStream::connect(socket_path)
            .await
            .expect("client could not connect to broker socket");
        let (r, mut w) = stream.into_split();
        w.write_all(format!("{username}\n").as_bytes())
            .await
            .unwrap();
        Self {
            reader: BufReader::new(r),
            writer: w,
        }
    }

    /// Read the next JSON line from the broker. Fails the test after 1 s.
    async fn recv(&mut self) -> Message {
        let mut line = String::new();
        timeout(Duration::from_secs(1), self.reader.read_line(&mut line))
            .await
            .expect("timed out waiting for message")
            .expect("read error");
        serde_json::from_str(line.trim()).expect("broker sent invalid JSON")
    }

    /// Drain messages until the predicate matches, or fail after 2 s.
    async fn recv_until<F: Fn(&Message) -> bool>(&mut self, pred: F) -> Message {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .unwrap_or_default();
            if remaining.is_zero() {
                panic!("timed out waiting for expected message");
            }
            let mut line = String::new();
            timeout(remaining, self.reader.read_line(&mut line))
                .await
                .expect("timed out")
                .expect("read error");
            if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                if pred(&msg) {
                    return msg;
                }
            }
        }
    }

    /// Send a plain-text message.
    async fn send_text(&mut self, text: &str) {
        self.writer
            .write_all(format!("{text}\n").as_bytes())
            .await
            .unwrap();
    }

    /// Send a JSON envelope.
    async fn send_json(&mut self, json: &str) {
        self.writer
            .write_all(format!("{json}\n").as_bytes())
            .await
            .unwrap();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Connecting to the broker sends a Join event back to the connecting client.
#[tokio::test]
async fn client_receives_own_join_event() {
    let broker = TestBroker::start("t_join").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;

    let msg = alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;
    assert_eq!(msg.user(), "alice");
    assert_eq!(msg.room(), "t_join");
}

/// A message sent by one client is broadcast to another connected client.
#[tokio::test]
async fn message_is_broadcast_to_all_clients() {
    let broker = TestBroker::start("t_broadcast").await;

    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    // drain alice's join
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    // drain alice's perspective: bob's join
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    // drain bob's perspective: his own join
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // Alice sends a message
    alice.send_text("hey bob").await;

    // Bob should receive it
    let msg = bob
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "hey bob"))
        .await;
    assert_eq!(msg.user(), "alice");
    assert_eq!(msg.room(), "t_broadcast");

    // Alice should also receive her own message (broker echoes to all)
    let echo = alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "hey bob"))
        .await;
    assert_eq!(echo.user(), "alice");
}

/// Sent messages are persisted to the chat file in NDJSON format.
#[tokio::test]
async fn messages_are_persisted_to_chat_file() {
    let broker = TestBroker::start("t_persist").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    alice.send_text("persist me").await;
    // wait for echo confirming broker processed it
    alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "persist me"))
        .await;

    // Give broker a moment to flush
    tokio::time::sleep(Duration::from_millis(50)).await;

    let history = history::load(&broker.chat_path).await.unwrap();
    let texts: Vec<&str> = history
        .iter()
        .filter_map(|m| {
            if let Message::Message { content, .. } = m {
                Some(content.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        texts.contains(&"persist me"),
        "expected message not found in chat file; got: {texts:?}"
    );
}

/// The join event itself is persisted to the chat file.
#[tokio::test]
async fn join_event_is_persisted() {
    let broker = TestBroker::start("t_join_persist").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    let history = history::load(&broker.chat_path).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| matches!(m, Message::Join { user, .. } if user == "alice")),
        "join event not found in history"
    );
}

/// On joining a room with existing history, the broker replays the last N messages.
#[tokio::test]
async fn history_is_replayed_on_join() {
    let broker = TestBroker::start("t_history").await;

    // Alice sends 5 messages
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;
    for i in 0..5usize {
        alice.send_text(&format!("msg {i}")).await;
        alice
            .recv_until(
                |m| matches!(m, Message::Message { content, .. } if content == &format!("msg {i}")),
            )
            .await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Bob connects — should receive the history before his join
    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;

    // Collect everything until bob's own join event
    let mut pre_join: Vec<Message> = Vec::new();
    loop {
        let msg = bob.recv().await;
        let is_own_join = matches!(&msg, Message::Join { user, .. } if user == "bob");
        if is_own_join {
            break;
        }
        pre_join.push(msg);
    }

    // History should include alice's messages
    let history_texts: Vec<&str> = pre_join
        .iter()
        .filter_map(|m| {
            if let Message::Message { content, .. } = m {
                Some(content.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        !history_texts.is_empty(),
        "expected history replay before join event"
    );
}

/// A JSON command envelope from a client is parsed and broadcast correctly.
#[tokio::test]
async fn json_command_envelope_is_broadcast() {
    let broker = TestBroker::start("t_cmd").await;

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

    // Use a non-built-in command so it passes through to broadcast unchanged.
    alice
        .send_json(r#"{"type":"command","cmd":"custom-cmd","params":["arg-1"]}"#)
        .await;

    let msg = bob
        .recv_until(|m| matches!(m, Message::Command { cmd, .. } if cmd == "custom-cmd"))
        .await;
    assert_eq!(msg.user(), "alice");
    if let Message::Command { params, .. } = &msg {
        assert_eq!(params, &["arg-1"]);
    }
}

/// A JSON reply envelope from a client is broadcast correctly.
#[tokio::test]
async fn json_reply_envelope_is_broadcast() {
    let broker = TestBroker::start("t_reply").await;

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

    alice
        .send_json(r#"{"type":"reply","reply_to":"dead1234","content":"got it"}"#)
        .await;

    let msg = bob
        .recv_until(|m| matches!(m, Message::Reply { content, .. } if content == "got it"))
        .await;
    if let Message::Reply { reply_to, .. } = &msg {
        assert_eq!(reply_to, "dead1234");
    }
}

/// When a client disconnects, a Leave event is broadcast to remaining clients.
#[tokio::test]
async fn disconnect_sends_leave_event() {
    let broker = TestBroker::start("t_leave").await;

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

    // Drop alice to simulate disconnect
    drop(alice);

    // Bob should see alice's leave event
    bob.recv_until(|m| matches!(m, Message::Leave { user, .. } if user == "alice"))
        .await;
}

/// Each message has a unique UUID id field.
#[tokio::test]
async fn messages_have_unique_ids() {
    let broker = TestBroker::start("t_ids").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    alice.send_text("one").await;
    let m1 = alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "one"))
        .await;

    alice.send_text("two").await;
    let m2 = alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "two"))
        .await;

    assert_ne!(m1.id(), m2.id(), "messages must have unique ids");
}

/// The broker assigns the correct room id to all messages.
#[tokio::test]
async fn broker_stamps_correct_room_id() {
    let broker = TestBroker::start("my_special_room").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    let msg = alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;
    assert_eq!(msg.room(), "my_special_room");
}

/// A second process connecting to an already-listening socket becomes a client,
/// not a second broker. We test this by verifying a second TestClient connects
/// cleanly and receives messages from the first (which would be impossible if
/// it had accidentally rebound the socket and kicked the broker).
#[tokio::test]
async fn second_connection_is_client_not_broker() {
    let broker = TestBroker::start("t_two_clients").await;

    let mut c1 = TestClient::connect(&broker.socket_path, "user1").await;
    c1.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "user1"))
        .await;

    let mut c2 = TestClient::connect(&broker.socket_path, "user2").await;
    // If c2 accidentally became a broker, c1 would not receive user2's join
    c1.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "user2"))
        .await;
    c2.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "user2"))
        .await;

    // c1 sends, c2 receives
    c1.send_text("from c1").await;
    c2.recv_until(|m| matches!(m, Message::Message { content, .. } if content == "from c1"))
        .await;
}

/// If the chat file already has history when the broker starts, new clients
/// receive that history before their own join event.
#[tokio::test]
async fn pre_existing_history_is_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("pre.sock");
    let chat_path = dir.path().join("pre.chat");

    // Write some history directly to the chat file before the broker starts
    let pre_msgs = vec![
        message::make_join("pre", "ghost"),
        message::make_message("pre", "ghost", "ancient message"),
    ];
    for m in &pre_msgs {
        history::append(&chat_path, m).await.unwrap();
    }

    let broker = Broker::new(
        "pre",
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

    let mut alice = TestClient::connect(&socket_path, "alice").await;

    let mut pre_join: Vec<Message> = Vec::new();
    loop {
        let msg = alice.recv().await;
        if matches!(&msg, Message::Join { user, .. } if user == "alice") {
            break;
        }
        pre_join.push(msg);
    }

    assert!(
        pre_join
            .iter()
            .any(|m| matches!(m, Message::Message { content, .. } if content == "ancient message")),
        "pre-existing history not replayed; got: {pre_join:?}"
    );
}

/// Stale socket file (file present, nothing listening) is cleaned up and the
/// new broker binds successfully.
#[tokio::test]
async fn stale_socket_is_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("stale.sock");
    let chat_path = dir.path().join("stale.chat");

    // Create a file at the socket path to simulate a stale socket
    tokio::fs::write(&socket_path, b"").await.unwrap();
    assert!(socket_path.exists());

    // Broker should remove the stale file and bind
    let broker = Broker::new(
        "stale",
        chat_path.clone(),
        chat_path.with_extension("tokens"),
        chat_path.with_extension("subscriptions"),
        socket_path.clone(),
        None,
    );
    tokio::spawn(async move { broker.run().await.ok() });

    for _ in 0..100 {
        if socket_path.exists() {
            // Verify we can actually connect (not just file present)
            if UnixStream::connect(&socket_path).await.is_ok() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("broker did not recover from stale socket");
}

/// A DM is delivered to the sender, the recipient, and the broker host.
/// It must NOT be delivered to other connected clients.
#[tokio::test]
async fn dm_delivered_to_sender_recipient_and_host() {
    let broker = TestBroker::start("t_dm_fanout").await;

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

    // dan is a bystander — not sender, recipient, or host
    let mut dan = TestClient::connect(&broker.socket_path, "dan").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "dan"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "dan"))
        .await;
    carol
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "dan"))
        .await;
    dan.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "dan"))
        .await;

    // bob sends a DM to carol
    bob.send_json(r#"{"type":"dm","to":"carol","content":"psst"}"#)
        .await;

    // carol (recipient) receives it
    let msg = carol
        .recv_until(|m| matches!(m, Message::DirectMessage { content, .. } if content == "psst"))
        .await;
    assert_eq!(msg.user(), "bob");
    if let Message::DirectMessage { to, .. } = &msg {
        assert_eq!(to, "carol");
    }

    // bob (sender) receives the echo
    bob.recv_until(|m| matches!(m, Message::DirectMessage { content, .. } if content == "psst"))
        .await;

    // alice (host) receives it
    alice
        .recv_until(|m| matches!(m, Message::DirectMessage { content, .. } if content == "psst"))
        .await;

    // dan (bystander) must NOT receive it — collect all of dan's messages for 300 ms
    let got_dm = tokio::time::timeout(Duration::from_millis(300), async {
        let mut line = String::new();
        loop {
            line.clear();
            if dan.reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break false;
            }
            if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                if matches!(&msg, Message::DirectMessage { content, .. } if content == "psst") {
                    break true;
                }
            }
        }
    })
    .await;
    assert!(
        got_dm.is_err() || !got_dm.unwrap(),
        "dan should not receive the DM"
    );
}

/// DMs are persisted to the chat history file.
#[tokio::test]
async fn dm_is_persisted_to_history() {
    let broker = TestBroker::start("t_dm_persist").await;

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

    alice
        .send_json(r#"{"type":"dm","to":"bob","content":"private"}"#)
        .await;
    // wait for echo to confirm the broker processed it
    alice
        .recv_until(|m| matches!(m, Message::DirectMessage { content, .. } if content == "private"))
        .await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    let history = history::load(&broker.chat_path).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| matches!(m, Message::DirectMessage { content, .. } if content == "private")),
        "DM not found in history"
    );
}

/// When the host is also the DM sender, delivery still works correctly.
#[tokio::test]
async fn dm_from_host_is_delivered_to_recipient() {
    let broker = TestBroker::start("t_dm_host_sender").await;

    // alice is host (first connect)
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

    alice
        .send_json(r#"{"type":"dm","to":"bob","content":"host dm"}"#)
        .await;

    // bob receives it
    bob.recv_until(|m| matches!(m, Message::DirectMessage { content, .. } if content == "host dm"))
        .await;

    // alice receives the echo (she is both sender and host)
    alice
        .recv_until(|m| matches!(m, Message::DirectMessage { content, .. } if content == "host dm"))
        .await;
}

/// A DM sent to an offline user is still persisted and the sender gets the echo.
#[tokio::test]
async fn dm_to_offline_user_is_persisted() {
    let broker = TestBroker::start("t_dm_offline").await;

    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // ghost is not connected
    alice
        .send_json(r#"{"type":"dm","to":"ghost","content":"nobody home"}"#)
        .await;

    // alice (sender = host) gets the echo
    alice
        .recv_until(
            |m| matches!(m, Message::DirectMessage { content, .. } if content == "nobody home"),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    let history = history::load(&broker.chat_path).await.unwrap();
    assert!(
        history.iter().any(
            |m| matches!(m, Message::DirectMessage { content, .. } if content == "nobody home")
        ),
        "DM to offline user not found in history"
    );
}

#[tokio::test]
async fn writes_to_tmp_slash_directly() {
    let path = std::path::PathBuf::from("/tmp/room_test_direct.chat");
    let _ = tokio::fs::remove_file(&path).await;
    let msg = room_cli::message::make_message("r", "u", "test to /tmp");
    room_cli::history::append(&path, &msg).await.unwrap();
    let loaded = room_cli::history::load(&path).await.unwrap();
    assert_eq!(loaded.len(), 1);
    let _ = tokio::fs::remove_file(&path).await;
}

/// set_status broadcasts a System message to all connected clients announcing
/// the status change. The command itself is not echoed — only the system notice.
#[tokio::test]
async fn set_status_broadcasts_system_message_to_all() {
    let broker = TestBroker::start("t_set_status").await;

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

    alice
        .send_json(r#"{"type":"command","cmd":"set_status","params":["working on auth"]}"#)
        .await;

    // Both alice and bob should receive the system broadcast
    let alice_notice = alice
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("working on auth")),
        )
        .await;
    assert!(alice_notice.user() == "broker");

    let bob_notice = bob
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("working on auth")),
        )
        .await;
    assert!(bob_notice.user() == "broker");

    // The raw Command must NOT have been broadcast to bob
    // (no Command with cmd="set_status" should appear — we verify by checking
    // that what bob received was a System, not a Command)
    assert!(
        matches!(bob_notice, Message::System { .. }),
        "expected System message, got something else"
    );
}

/// `send_message_with_token` with a `/set_status` command returns the system echo
/// instead of producing an EOF error. Regression test for #234.
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

    tokio::time::sleep(Duration::from_millis(50)).await;

    let dir = tempfile::tempdir().unwrap();
    let cursor_path = dir.path().join("test.cursor");

    let msgs =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_path, None, Some(m1.id()))
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
    tokio::time::sleep(Duration::from_millis(50)).await;

    let dir = tempfile::tempdir().unwrap();
    let cursor_path = dir.path().join("alice.cursor");

    // First poll: returns messages, writes cursor
    let msgs = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_path, None, None)
        .await
        .unwrap();
    assert!(!msgs.is_empty(), "first poll should return messages");
    assert!(
        cursor_path.exists(),
        "cursor file must be written after poll"
    );

    // Second poll: cursor is up to date, nothing new
    let msgs2 = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_path, None, None)
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

    let msgs = room_cli::oneshot::poll_messages(&chat_path, &cursor_path, None, None)
        .await
        .unwrap();

    assert_eq!(msgs.len(), 1);
    assert!(matches!(&msgs[0], Message::Message { content, .. } if content == "written directly"));
}

/// `send_message` returns an error when no broker socket exists.
#[tokio::test]
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
    let msgs = room_cli::oneshot::poll_messages(&chat_path, &cursor_path, Some("carol"), None)
        .await
        .unwrap();
    assert!(
        msgs.is_empty(),
        "carol should not see a DM she is not party to"
    );

    // alice (sender) should see it
    let alice_cursor = dir.path().join("alice.cursor");
    let msgs = room_cli::oneshot::poll_messages(&chat_path, &alice_cursor, Some("alice"), None)
        .await
        .unwrap();
    assert_eq!(msgs.len(), 1, "alice should see the DM she sent");

    // bob (recipient) should see it
    let bob_cursor = dir.path().join("bob.cursor");
    let msgs = room_cli::oneshot::poll_messages(&chat_path, &bob_cursor, Some("bob"), None)
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

// ── Token auth tests ──────────────────────────────────────────────────────────

/// `join_session` returns a non-empty token and the correct username on success.
#[tokio::test]
async fn join_session_returns_token() {
    let broker = TestBroker::start("t_join_token").await;
    let (username, token) = room_cli::oneshot::join_session(&broker.socket_path, "agent1")
        .await
        .expect("join_session failed");
    assert_eq!(username, "agent1");
    assert!(!token.is_empty(), "token must be non-empty");
}

/// A second `join_session` for the same username is rejected with a clear error.
#[tokio::test]
async fn join_session_rejects_duplicate_username() {
    let broker = TestBroker::start("t_join_dup").await;
    room_cli::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .expect("first join failed");
    let err = room_cli::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .expect_err("second join should have failed");
    assert!(
        err.to_string().contains("already in use"),
        "error should mention 'already in use': {err}"
    );
}

/// Two distinct usernames can both register in the same room.
#[tokio::test]
async fn join_session_allows_different_usernames() {
    let broker = TestBroker::start("t_join_two").await;
    let (_, tok1) = room_cli::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .expect("alice join failed");
    let (_, tok2) = room_cli::oneshot::join_session(&broker.socket_path, "bob")
        .await
        .expect("bob join failed");
    assert_ne!(tok1, tok2, "each user must receive a distinct token");
}

/// `send_message_with_token` delivers a message and returns the broadcast echo.
#[tokio::test]
async fn send_with_token_delivers_message() {
    let broker = TestBroker::start("t_send_token").await;

    let mut watcher = TestClient::connect(&broker.socket_path, "watcher").await;
    watcher
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "watcher"))
        .await;

    let (_, token) = room_cli::oneshot::join_session(&broker.socket_path, "agent")
        .await
        .expect("join_session failed");

    let wire = serde_json::json!({"type": "message", "content": "hello from token"}).to_string();
    let msg = room_cli::oneshot::send_message_with_token(&broker.socket_path, &token, &wire)
        .await
        .expect("send_message_with_token failed");

    assert!(
        matches!(&msg, Message::Message { user, content, .. }
            if user == "agent" && content == "hello from token"),
        "unexpected message: {msg:?}"
    );

    let received = watcher
        .recv_until(|m| matches!(m, Message::Message { user, .. } if user == "agent"))
        .await;
    assert!(
        matches!(&received, Message::Message { content, .. } if content == "hello from token"),
        "watcher got unexpected message: {received:?}"
    );
}

/// An invalid (unknown) token returns a clear error from the broker.
#[tokio::test]
async fn send_with_invalid_token_returns_error() {
    let broker = TestBroker::start("t_invalid_token").await;
    let wire = serde_json::json!({"type": "message", "content": "hi"}).to_string();
    let err =
        room_cli::oneshot::send_message_with_token(&broker.socket_path, "not-a-real-token", &wire)
            .await
            .expect_err("should have failed with invalid token");
    assert!(
        err.to_string().contains("invalid token"),
        "error should mention 'invalid token': {err}"
    );
}

/// After a `join_session` the token can be used from two sequential sends.
#[tokio::test]
async fn token_is_reusable_across_sends() {
    let broker = TestBroker::start("t_token_reuse").await;
    let (_, token) = room_cli::oneshot::join_session(&broker.socket_path, "agent")
        .await
        .expect("join_session failed");

    for i in 0..2u8 {
        let wire =
            serde_json::json!({"type": "message", "content": format!("msg {i}")}).to_string();
        room_cli::oneshot::send_message_with_token(&broker.socket_path, &token, &wire)
            .await
            .unwrap_or_else(|e| panic!("send {i} failed: {e}"));
    }
}

// ── Admin command tests ───────────────────────────────────────────────────────

/// `\kick <username>` invalidates the target's token; subsequent sends fail.
#[tokio::test]
async fn admin_kick_invalidates_token() {
    let broker = TestBroker::start("t_kick").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    // Drain admin's own join
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    let (_, victim_token) = room_cli::oneshot::join_session(&broker.socket_path, "victim")
        .await
        .expect("join failed");

    // Admin kicks victim
    admin
        .send_json(r#"{"type":"command","cmd":"kick","params":["victim"]}"#)
        .await;

    // Kick produces a system broadcast
    let sys = admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;
    assert!(
        matches!(&sys, Message::System { content, .. } if content.contains("kicked victim")),
        "expected kick system message, got: {sys:?}"
    );

    // Victim's token is now invalid
    let wire = serde_json::json!({"type": "message", "content": "sneaky"}).to_string();
    let err = room_cli::oneshot::send_message_with_token(&broker.socket_path, &victim_token, &wire)
        .await
        .expect_err("send with kicked token should fail");
    assert!(
        err.to_string().contains("invalid token"),
        "expected invalid token error, got: {err}"
    );
}

/// Kicking two users must not unblock the first when the second is kicked.
#[tokio::test]
async fn admin_kick_two_users_both_blocked() {
    let broker = TestBroker::start("t_kick2").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    let (_, tok_a) = room_cli::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .unwrap();
    let (_, tok_b) = room_cli::oneshot::join_session(&broker.socket_path, "bob")
        .await
        .unwrap();

    admin
        .send_json(r#"{"type":"command","cmd":"kick","params":["alice"]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;

    admin
        .send_json(r#"{"type":"command","cmd":"kick","params":["bob"]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;

    let wire = serde_json::json!({"type": "message", "content": "x"}).to_string();
    for (user, tok) in [("alice", &tok_a), ("bob", &tok_b)] {
        let result =
            room_cli::oneshot::send_message_with_token(&broker.socket_path, tok, &wire).await;
        assert!(
            result.is_err(),
            "{user} should still be blocked after two kicks"
        );
    }
}

/// `/reauth <username>` removes the token so the user can rejoin.
#[tokio::test]
async fn admin_reauth_allows_rejoin() {
    let broker = TestBroker::start("t_reauth").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    // Join as "alice" to register the username
    let (_, _alice_token) = room_cli::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .expect("alice join failed");

    // Alice is now registered; a second join would fail
    let err = room_cli::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .expect_err("duplicate alice join should fail");
    assert!(err.to_string().contains("username_taken") || err.to_string().contains("alice"));

    // Admin reauthenticates alice
    admin
        .send_json(r#"{"type":"command","cmd":"reauth","params":["alice"]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;

    // Now alice can rejoin
    let result = room_cli::oneshot::join_session(&broker.socket_path, "alice").await;
    assert!(
        result.is_ok(),
        "alice should be able to rejoin after reauth"
    );
}

/// `/clear-tokens` removes all tokens; no user can send until they rejoin.
#[tokio::test]
async fn admin_clear_tokens_blocks_all_sends() {
    let broker = TestBroker::start("t_clear_tokens").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    let (_, tok1) = room_cli::oneshot::join_session(&broker.socket_path, "u1")
        .await
        .unwrap();
    let (_, tok2) = room_cli::oneshot::join_session(&broker.socket_path, "u2")
        .await
        .unwrap();

    admin
        .send_json(r#"{"type":"command","cmd":"clear-tokens","params":[]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { content, .. } if content.contains("cleared all tokens")))
        .await;

    for (user, tok) in [("u1", &tok1), ("u2", &tok2)] {
        let wire = serde_json::json!({"type": "message", "content": "hi"}).to_string();
        let result =
            room_cli::oneshot::send_message_with_token(&broker.socket_path, tok, &wire).await;
        assert!(
            result.is_err(),
            "send from {user} should fail after clear-tokens"
        );
    }
}

/// `/clear` truncates the history file and broadcasts a system message.
#[tokio::test]
async fn admin_clear_history() {
    let broker = TestBroker::start("t_clear_history").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    // Write something to history first
    let wire = serde_json::json!({"type": "message", "content": "keep me"}).to_string();
    room_cli::oneshot::send_message(&broker.socket_path, "admin", &wire)
        .await
        .unwrap();

    // Verify history has content
    let before = history::load(&broker.chat_path).await.unwrap();
    assert!(
        !before.is_empty(),
        "history should have at least join + message"
    );

    admin
        .send_json(r#"{"type":"command","cmd":"clear","params":[]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { content, .. } if content.contains("cleared chat history")))
        .await;

    // The /clear system message itself is written after truncation, so history
    // should contain only that one entry now.
    let after = history::load(&broker.chat_path).await.unwrap();
    assert_eq!(
        after.len(),
        1,
        "history should have only the clear notice: {after:?}"
    );
    assert!(
        matches!(&after[0], Message::System { content, .. } if content.contains("cleared chat history"))
    );
}

/// `/exit` broadcasts a shutdown notice and the broker stops accepting connections.
#[tokio::test]
async fn admin_exit_shuts_down_broker() {
    let broker = TestBroker::start("t_exit").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    admin
        .send_json(r#"{"type":"command","cmd":"exit","params":[]}"#)
        .await;
    admin
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("shutting down")),
        )
        .await;

    // Give the broker a moment to stop listening
    tokio::time::sleep(Duration::from_millis(100)).await;

    // New connections should fail
    let result = UnixStream::connect(&broker.socket_path).await;
    assert!(
        result.is_err(),
        "broker should have stopped accepting connections after /exit"
    );
}

/// After `/exit`, connected clients receive EOF (Ok(0)) on their socket read,
/// which the TUI detects to exit cleanly without requiring Ctrl-C.
#[tokio::test]
async fn exit_causes_broker_to_close_client_connections() {
    let broker = TestBroker::start("t_exit_eof").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    // A second client to verify all connections are closed, not just admin's.
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    admin
        .send_json(r#"{"type":"command","cmd":"exit","params":[]}"#)
        .await;

    // Both clients should see the shutdown message.
    admin
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("shutting down")),
        )
        .await;
    alice
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("shutting down")),
        )
        .await;

    // After the shutdown message the broker closes the sockets. Both clients
    // should get EOF (read returns 0 bytes) within a short window.
    let admin_eof = tokio::time::timeout(Duration::from_millis(500), async {
        let mut buf = String::new();
        admin.reader.read_line(&mut buf).await.unwrap_or(0)
    })
    .await;
    assert!(
        matches!(admin_eof, Ok(0)),
        "admin socket should receive EOF after /exit"
    );

    let alice_eof = tokio::time::timeout(Duration::from_millis(500), async {
        let mut buf = String::new();
        alice.reader.read_line(&mut buf).await.unwrap_or(0)
    })
    .await;
    assert!(
        matches!(alice_eof, Ok(0)),
        "alice socket should receive EOF after /exit"
    );
}

// ── pull_messages tests ────────────────────────────────────────────────────────

/// `pull_messages` returns the last N messages from history.
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

    // Give broker time to flush writes
    tokio::time::sleep(Duration::from_millis(50)).await;

    let msgs = room_cli::oneshot::pull_messages(&broker.chat_path, 3, None)
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

    tokio::time::sleep(Duration::from_millis(50)).await;

    let msgs = room_cli::oneshot::pull_messages(&broker.chat_path, 100, None)
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
    let msgs = room_cli::oneshot::pull_messages(&chat_path, 20, None)
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
    let token_path = room_cli::oneshot::token_file_path(room_id, "alice");
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
    tokio::time::sleep(Duration::from_millis(50)).await;

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
    tokio::time::sleep(Duration::from_millis(50)).await;

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

    tokio::time::sleep(Duration::from_millis(50)).await;

    // carol (a third party) pulls history — should not see the DM
    let carol_msgs = room_cli::oneshot::pull_messages(&broker.chat_path, 50, Some("carol"))
        .await
        .unwrap();
    assert!(
        !carol_msgs
            .iter()
            .any(|m| matches!(m, Message::DirectMessage { content, .. } if content == "secret")),
        "carol should not see the DM between alice and bob"
    );

    // alice pulls — should see the DM
    let alice_msgs = room_cli::oneshot::pull_messages(&broker.chat_path, 50, Some("alice"))
        .await
        .unwrap();
    assert!(
        alice_msgs
            .iter()
            .any(|m| matches!(m, Message::DirectMessage { content, .. } if content == "secret")),
        "alice should see the DM addressed to her"
    );
}

/// After `/kick`, the kicked user must not appear in subsequent `/who` responses.
#[tokio::test]
async fn kick_removes_user_from_who() {
    let broker = TestBroker::start("t_kick_who").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    let mut victim = TestClient::connect(&broker.socket_path, "victim").await;
    victim
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "victim"))
        .await;

    // Admin kicks victim
    admin
        .send_json(r#"{"type":"command","cmd":"kick","params":["victim"]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;

    // Admin queries /who — victim should no longer appear
    admin
        .send_json(r#"{"type":"command","cmd":"who","params":[]}"#)
        .await;
    let sys = admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;
    let Message::System { content, .. } = &sys else {
        panic!("expected system message");
    };
    assert!(
        !content.contains("victim"),
        "kicked user should not appear in /who, got: {content}"
    );
    assert!(
        content.contains("admin"),
        "admin should still appear in /who, got: {content}"
    );
}

/// `/who` via oneshot send returns the user list to the sender without broadcasting.
#[tokio::test]
async fn oneshot_who_returns_user_list() {
    let broker = TestBroker::start("t_oneshot_who").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    let (_, tok) = room_cli::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .unwrap();

    let wire = serde_json::json!({"type":"command","cmd":"who","params":[]}).to_string();
    let msg = room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok, &wire)
        .await
        .expect("oneshot /who should succeed");

    let Message::System { content, .. } = msg else {
        panic!("expected system message, got: {msg:?}");
    };
    assert!(
        content.contains("alice"),
        "/who should list alice, got: {content}"
    );

    // The Command should NOT have been broadcast to alice
    let got_cmd = tokio::time::timeout(std::time::Duration::from_millis(200), async {
        alice
            .recv_until(|m| matches!(m, Message::Command { cmd, .. } if cmd == "who"))
            .await
    })
    .await;
    assert!(
        got_cmd.is_err(),
        "/who command should not be broadcast to other clients"
    );
}

/// A non-host user sending an admin command receives a private "permission denied"
/// system message. The command is not executed and no broadcast is sent.
#[tokio::test]
async fn non_host_admin_cmd_is_rejected() {
    let broker = TestBroker::start("t_admin_auth_reject").await;

    // alice is host (first to join)
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // bob joins — not the host
    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // bot joins via token (oneshot JOIN), then attempts /kick alice
    let (_, tok) = room_cli::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .unwrap();
    let msg = room_cli::oneshot::send_message_with_token(
        &broker.socket_path,
        &tok,
        r#"{"type":"command","cmd":"kick","params":["alice"]}"#,
    )
    .await
    .expect("oneshot send should succeed even when permission is denied");

    let Message::System { content, .. } = msg else {
        panic!("expected system message, got: {msg:?}");
    };
    assert!(
        content.contains("permission denied"),
        "non-host admin cmd should return permission denied, got: {content}"
    );

    // alice must NOT have received a kick broadcast
    let got_kick = tokio::time::timeout(Duration::from_millis(200), async {
        alice
            .recv_until(
                |m| matches!(m, Message::System { content, .. } if content.contains("kicked")),
            )
            .await
    })
    .await;
    assert!(
        got_kick.is_err(),
        "kick must not execute; alice should not see a kick system message"
    );
}

/// The room host (first interactive user) can execute admin commands.
#[tokio::test]
async fn host_can_run_admin_commands() {
    let broker = TestBroker::start("t_admin_auth_host").await;

    // alice is host (first to join)
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut victim = TestClient::connect(&broker.socket_path, "victim").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "victim"))
        .await;
    victim
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "victim"))
        .await;

    // alice (host) kicks via JSON command (matching TUI build_payload output)
    alice
        .send_json(r#"{"type":"command","cmd":"kick","params":["victim"]}"#)
        .await;

    // broadcast: all connected users receive the kick system message
    let sys = alice
        .recv_until(|m| matches!(m, Message::System { content, .. } if content.contains("kicked")))
        .await;
    let Message::System { content, .. } = &sys else {
        panic!("expected system message");
    };
    assert!(
        content.contains("victim"),
        "kick message should name the victim, got: {content}"
    );
}
// ── seq numbering tests ───────────────────────────────────────────────────────

/// Every broadcast message carries a monotonically increasing seq number.
#[tokio::test]
async fn broadcast_messages_have_monotonically_increasing_seq() {
    let broker = TestBroker::start("t_seq_mono").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    alice.send_text("first").await;
    let m1 = alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "first"))
        .await;

    alice.send_text("second").await;
    let m2 = alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "second"))
        .await;

    alice.send_text("third").await;
    let m3 = alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "third"))
        .await;

    let s1 = m1.seq().expect("first message must have seq");
    let s2 = m2.seq().expect("second message must have seq");
    let s3 = m3.seq().expect("third message must have seq");

    assert!(s1 < s2, "seq must be strictly increasing: {s1} < {s2}");
    assert!(s2 < s3, "seq must be strictly increasing: {s2} < {s3}");
    assert!(s1 >= 1, "seq must start at 1 or higher");
}

/// Join and leave events also receive seq numbers.
#[tokio::test]
async fn join_and_leave_events_have_seq() {
    let broker = TestBroker::start("t_seq_join_leave").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    let join_msg = alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;
    assert!(
        join_msg.seq().is_some(),
        "join event must carry a seq number"
    );

    // Connect bob then disconnect to generate a leave event.
    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    drop(bob);

    let leave_msg = alice
        .recv_until(|m| matches!(m, Message::Leave { user, .. } if user == "bob"))
        .await;
    assert!(
        leave_msg.seq().is_some(),
        "leave event must carry a seq number"
    );

    let join_seq = join_msg.seq().unwrap();
    let leave_seq = leave_msg.seq().unwrap();
    // There may be intervening messages (bob's join), but leave must be strictly after alice's join.
    assert!(
        leave_seq > join_seq,
        "leave seq ({leave_seq}) must be greater than alice's join seq ({join_seq})"
    );
}

/// Messages persisted to the chat file include seq numbers.
#[tokio::test]
async fn persisted_messages_have_seq_in_history() {
    let broker = TestBroker::start("t_seq_persist").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    alice.send_text("check seq").await;
    alice
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "check seq"))
        .await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    let history = history::load(&broker.chat_path).await.unwrap();
    for msg in &history {
        assert!(
            msg.seq().is_some(),
            "every persisted message must have seq, but {:?} does not",
            msg
        );
    }

    // Verify strictly increasing order.
    let seqs: Vec<u64> = history.iter().map(|m| m.seq().unwrap()).collect();
    for pair in seqs.windows(2) {
        assert!(
            pair[0] < pair[1],
            "history seq must be strictly increasing: {} then {}",
            pair[0],
            pair[1]
        );
    }
}

/// History files without seq fields (old format) are still parsed without error.
#[tokio::test]
async fn history_without_seq_parses_as_none() {
    let raw = r#"{"type":"message","id":"abc","room":"r","user":"alice","ts":"2026-03-05T10:00:00Z","content":"hi"}
{"type":"join","id":"def","room":"r","user":"bob","ts":"2026-03-05T10:00:01Z"}
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.chat");
    std::fs::write(&path, raw).unwrap();

    let msgs = history::load(&path).await.unwrap();
    assert_eq!(msgs.len(), 2, "both lines should parse");
    for msg in &msgs {
        assert!(
            msg.seq().is_none(),
            "old messages without seq should deserialize to seq=None"
        );
    }
}

// ── WebSocket / REST integration tests ───────────────────────────────────────

/// Helper: connect a WebSocket client and perform the username handshake.
/// Returns the split (sink, stream) after sending the first frame.
async fn ws_connect(
    port: u16,
    room_id: &str,
    first_frame: &str,
) -> (
    futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        TungsteniteMsg,
    >,
    futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
) {
    let url = format!("ws://127.0.0.1:{port}/ws/{room_id}");
    let (ws, _) = connect_async(&url).await.expect("WS connect failed");
    let (mut tx, rx) = ws.split();
    tx.send(TungsteniteMsg::Text(first_frame.into()))
        .await
        .unwrap();
    (tx, rx)
}

/// Read the next text frame as raw JSON Value.
async fn ws_recv_json(
    rx: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
) -> serde_json::Value {
    let deadline = Duration::from_secs(2);
    match timeout(deadline, rx.next()).await {
        Ok(Some(Ok(TungsteniteMsg::Text(text)))) => {
            serde_json::from_str(&text).expect("WS broker sent invalid JSON value")
        }
        Ok(Some(Ok(other))) => panic!("unexpected WS frame: {other:?}"),
        Ok(Some(Err(e))) => panic!("WS read error: {e}"),
        Ok(None) => panic!("WS stream ended unexpectedly"),
        Err(_) => panic!("timed out waiting for WS message"),
    }
}

/// Drain WS frames until predicate matches a Message, or panic after 2s.
async fn ws_recv_until<F: Fn(&Message) -> bool>(
    rx: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    pred: F,
) -> Message {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            panic!("timed out waiting for expected WS message");
        }
        match timeout(remaining, rx.next()).await {
            Ok(Some(Ok(TungsteniteMsg::Text(text)))) => {
                if let Ok(msg) = serde_json::from_str::<Message>(&text) {
                    if pred(&msg) {
                        return msg;
                    }
                }
            }
            Ok(Some(Ok(_))) => continue,
            _ => panic!("WS stream ended or errored while waiting for message"),
        }
    }
}

// ── WS: interactive session ─────────────────────────────────────────────────

#[tokio::test]
async fn ws_interactive_join_and_message() {
    let (tb, port) = TestBroker::start_with_ws("ws_join").await;
    let _ = &tb.chat_path; // keep broker alive

    let (mut tx, mut rx) = ws_connect(port, "ws_join", "alice").await;

    // Should receive the join event for alice.
    let join = ws_recv_until(
        &mut rx,
        |m| matches!(m, Message::Join { user, .. } if user == "alice"),
    )
    .await;
    assert!(matches!(join, Message::Join { user, .. } if user == "alice"));

    // Send a message.
    tx.send(TungsteniteMsg::Text("hello from ws".into()))
        .await
        .unwrap();

    // Should receive the broadcast back.
    let msg = ws_recv_until(
        &mut rx,
        |m| matches!(m, Message::Message { content, .. } if content == "hello from ws"),
    )
    .await;
    assert!(matches!(msg, Message::Message { content, .. } if content == "hello from ws"));

    // Verify it was persisted.
    let history = history::load(&tb.chat_path).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| matches!(m, Message::Message { content, .. } if content == "hello from ws")),
        "WS message should be persisted to chat file"
    );
}

#[tokio::test]
async fn ws_oneshot_join_returns_token() {
    let (_tb, port) = TestBroker::start_with_ws("ws_osjoin").await;

    let (mut _tx, mut rx) = ws_connect(port, "ws_osjoin", "JOIN:bob").await;

    let v = ws_recv_json(&mut rx).await;
    assert_eq!(v["type"], "token");
    assert_eq!(v["username"], "bob");
    assert!(
        v["token"].as_str().unwrap().len() > 10,
        "token should be a UUID"
    );
}

#[tokio::test]
async fn ws_oneshot_join_duplicate_returns_error() {
    let (_tb, port) = TestBroker::start_with_ws("ws_osjdup").await;

    // First join succeeds.
    let (_tx1, mut rx1) = ws_connect(port, "ws_osjdup", "JOIN:carol").await;
    let v1 = ws_recv_json(&mut rx1).await;
    assert_eq!(v1["type"], "token");

    // Second join with same username fails.
    let (_tx2, mut rx2) = ws_connect(port, "ws_osjdup", "JOIN:carol").await;
    let v2 = ws_recv_json(&mut rx2).await;
    assert_eq!(v2["type"], "error");
    assert_eq!(v2["code"], "username_taken");
}

#[tokio::test]
async fn ws_oneshot_send_with_token() {
    let (tb, port) = TestBroker::start_with_ws("ws_ossend").await;

    // First, get a token via JOIN.
    let (_tx_j, mut rx_j) = ws_connect(port, "ws_ossend", "JOIN:dave").await;
    let token_resp = ws_recv_json(&mut rx_j).await;
    let token = token_resp["token"].as_str().unwrap();

    // Use TOKEN: prefix to send a one-shot message.
    let first_frame = format!("TOKEN:{token}");
    let (mut tx_s, mut rx_s) = ws_connect(port, "ws_ossend", &first_frame).await;

    // Send the actual message content as the second frame.
    tx_s.send(TungsteniteMsg::Text("one-shot hello".into()))
        .await
        .unwrap();

    // Should get the echo back.
    let echo = ws_recv_json(&mut rx_s).await;
    assert_eq!(echo["type"], "message");
    assert_eq!(echo["content"], "one-shot hello");
    assert_eq!(echo["user"], "dave");
    assert!(
        echo["seq"].as_u64().is_some(),
        "echo should have a seq number"
    );

    // Verify persistence.
    let history = history::load(&tb.chat_path).await.unwrap();
    assert!(history
        .iter()
        .any(|m| matches!(m, Message::Message { content, .. } if content == "one-shot hello")));
}

#[tokio::test]
async fn ws_invalid_token_returns_error() {
    let (_tb, port) = TestBroker::start_with_ws("ws_badtok").await;

    let (_tx, mut rx) = ws_connect(port, "ws_badtok", "TOKEN:not-a-real-token").await;

    let v = ws_recv_json(&mut rx).await;
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "invalid_token");
}

#[tokio::test]
async fn ws_wrong_room_returns_not_found() {
    let (_tb, port) = TestBroker::start_with_ws("ws_room404").await;

    let url = format!("ws://127.0.0.1:{port}/ws/nonexistent");
    // The server should reject the upgrade. connect_async may get an HTTP error.
    let result = connect_async(&url).await;
    assert!(result.is_err(), "connecting to wrong room should fail");
}

// ── WS ↔ UDS cross-transport ───────────────────────────────────────────────

#[tokio::test]
async fn cross_transport_uds_sees_ws_message() {
    let (tb, port) = TestBroker::start_with_ws("ws_cross1").await;

    // UDS client connects first.
    let mut uds = TestClient::connect(&tb.socket_path, "uds_user").await;
    // Drain the join event for uds_user.
    uds.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "uds_user"))
        .await;

    // WS client connects.
    let (mut ws_tx, _ws_rx) = ws_connect(port, "ws_cross1", "ws_user").await;

    // UDS client should see ws_user's join.
    uds.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "ws_user"))
        .await;

    // WS client sends a message.
    ws_tx
        .send(TungsteniteMsg::Text("from websocket".into()))
        .await
        .unwrap();

    // UDS client should receive it.
    let msg = uds
        .recv_until(
            |m| matches!(m, Message::Message { content, .. } if content == "from websocket"),
        )
        .await;
    assert!(
        matches!(msg, Message::Message { user, content, .. } if user == "ws_user" && content == "from websocket")
    );
}

#[tokio::test]
async fn cross_transport_ws_sees_uds_message() {
    let (tb, port) = TestBroker::start_with_ws("ws_cross2").await;

    // WS client connects first.
    let (_ws_tx, mut ws_rx) = ws_connect(port, "ws_cross2", "ws_user2").await;

    // Drain ws_user2's own join.
    ws_recv_until(
        &mut ws_rx,
        |m| matches!(m, Message::Join { user, .. } if user == "ws_user2"),
    )
    .await;

    // UDS client connects.
    let mut uds = TestClient::connect(&tb.socket_path, "uds_user2").await;

    // WS should see uds_user2's join.
    ws_recv_until(
        &mut ws_rx,
        |m| matches!(m, Message::Join { user, .. } if user == "uds_user2"),
    )
    .await;

    // UDS client sends a message.
    uds.send_text("from unix socket").await;

    // WS client should receive it.
    let msg = ws_recv_until(
        &mut ws_rx,
        |m| matches!(m, Message::Message { content, .. } if content == "from unix socket"),
    )
    .await;
    assert!(
        matches!(msg, Message::Message { user, content, .. } if user == "uds_user2" && content == "from unix socket")
    );
}

// ── REST API tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn rest_health_returns_ok() {
    let (_tb, port) = TestBroker::start_with_ws("ws_health").await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/api/health"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["room"], "ws_health");
}

#[tokio::test]
async fn rest_join_send_poll_lifecycle() {
    let (_tb, port) = TestBroker::start_with_ws("ws_rest").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // JOIN via REST.
    let join_resp = client
        .post(format!("{base}/api/ws_rest/join"))
        .json(&serde_json::json!({"username": "rest_user"}))
        .send()
        .await
        .unwrap();
    assert_eq!(join_resp.status(), 200);
    let join_body: serde_json::Value = join_resp.json().await.unwrap();
    assert_eq!(join_body["type"], "token");
    let token = join_body["token"].as_str().unwrap();

    // SEND via REST.
    let send_resp = client
        .post(format!("{base}/api/ws_rest/send"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"content": "hello from REST"}))
        .send()
        .await
        .unwrap();
    assert_eq!(send_resp.status(), 200);
    let send_body: serde_json::Value = send_resp.json().await.unwrap();
    assert_eq!(send_body["type"], "message");
    assert_eq!(send_body["content"], "hello from REST");
    assert_eq!(send_body["user"], "rest_user");
    let msg_id = send_body["id"].as_str().unwrap().to_owned();

    // POLL via REST — no since param, should get the message.
    let poll_resp = client
        .get(format!("{base}/api/ws_rest/poll"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(poll_resp.status(), 200);
    let poll_body: serde_json::Value = poll_resp.json().await.unwrap();
    let messages = poll_body["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(|m| m["content"] == "hello from REST"),
        "poll should contain the sent message"
    );

    // POLL with since= the message ID — should return empty.
    let poll2_resp = client
        .get(format!("{base}/api/ws_rest/poll?since={msg_id}"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    let poll2_body: serde_json::Value = poll2_resp.json().await.unwrap();
    let messages2 = poll2_body["messages"].as_array().unwrap();
    assert!(
        messages2.is_empty(),
        "poll with since=last_id should return no messages"
    );
}

#[tokio::test]
async fn rest_send_without_token_returns_401() {
    let (_tb, port) = TestBroker::start_with_ws("ws_noauth").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/ws_noauth/send"))
        .json(&serde_json::json!({"content": "should fail"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "missing_token");
}

#[tokio::test]
async fn rest_send_with_invalid_token_returns_401() {
    let (_tb, port) = TestBroker::start_with_ws("ws_badauth").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/ws_badauth/send"))
        .header("Authorization", "Bearer fake-token-123")
        .json(&serde_json::json!({"content": "should fail"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_token");
}

#[tokio::test]
async fn rest_wrong_room_returns_404() {
    let (_tb, port) = TestBroker::start_with_ws("ws_404room").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/wrong_room/join"))
        .json(&serde_json::json!({"username": "nobody"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "room_not_found");
}

#[tokio::test]
async fn rest_duplicate_join_returns_409() {
    let (_tb, port) = TestBroker::start_with_ws("ws_dupjoin").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // First join succeeds.
    let r1 = client
        .post(format!("{base}/api/ws_dupjoin/join"))
        .json(&serde_json::json!({"username": "dup_user"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 200);

    // Second join with same name returns 409.
    let r2 = client
        .post(format!("{base}/api/ws_dupjoin/join"))
        .json(&serde_json::json!({"username": "dup_user"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 409);
    let body: serde_json::Value = r2.json().await.unwrap();
    assert_eq!(body["code"], "username_taken");
}

#[tokio::test]
async fn rest_send_dm_is_persisted() {
    let (tb, port) = TestBroker::start_with_ws("ws_restdm").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Join sender.
    let r1 = client
        .post(format!("{base}/api/ws_restdm/join"))
        .json(&serde_json::json!({"username": "sender"}))
        .send()
        .await
        .unwrap();
    let t1: serde_json::Value = r1.json().await.unwrap();
    let token = t1["token"].as_str().unwrap();

    // Join recipient (so the name is registered).
    let _r2 = client
        .post(format!("{base}/api/ws_restdm/join"))
        .json(&serde_json::json!({"username": "recipient"}))
        .send()
        .await
        .unwrap();

    // Send DM.
    let send_resp = client
        .post(format!("{base}/api/ws_restdm/send"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"content": "secret DM", "to": "recipient"}))
        .send()
        .await
        .unwrap();
    assert_eq!(send_resp.status(), 200);
    let send_body: serde_json::Value = send_resp.json().await.unwrap();
    assert_eq!(send_body["type"], "dm");
    assert_eq!(send_body["to"], "recipient");

    // Verify persisted.
    let history = history::load(&tb.chat_path).await.unwrap();
    assert!(history
        .iter()
        .any(|m| matches!(m, Message::DirectMessage { content, to, .. } if content == "secret DM" && to == "recipient")));
}

// ── Daemon (multi-room) integration tests ────────────────────────────────────

use room_cli::broker::daemon::{DaemonConfig, DaemonState};
use room_protocol::{dm_room_id, RoomConfig, RoomVisibility};

struct TestDaemon {
    pub socket_path: PathBuf,
    pub state: Arc<DaemonState>,
    _dir: TempDir,
}

impl TestDaemon {
    /// Start a daemon with configured rooms (room_id, optional RoomConfig).
    async fn start_with_configs(rooms: Vec<(&str, Option<RoomConfig>)>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("roomd.sock");

        let config = DaemonConfig {
            socket_path: socket_path.clone(),
            data_dir: dir.path().to_owned(),
            state_dir: dir.path().to_owned(),
            ws_port: None,
            grace_period_secs: 30,
        };

        let daemon = Arc::new(DaemonState::new(config));
        for (room_id, room_config) in rooms {
            match room_config {
                Some(cfg) => daemon.create_room_with_config(room_id, cfg).await.unwrap(),
                None => daemon.create_room(room_id).await.unwrap(),
            }
        }

        let daemon_run = daemon.clone();
        tokio::spawn(async move {
            daemon_run.run().await.ok();
        });

        common::wait_for_socket(&socket_path, Duration::from_secs(1)).await;

        Self {
            socket_path,
            state: daemon,
            _dir: dir,
        }
    }

    /// Start a daemon with WS/REST support and configured rooms.
    /// Returns (TestDaemon, ws_port).
    async fn start_with_ws_configs(rooms: Vec<(&str, Option<RoomConfig>)>) -> (Self, u16) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("roomd.sock");
        let port = common::free_port();

        let config = DaemonConfig {
            socket_path: socket_path.clone(),
            data_dir: dir.path().to_owned(),
            state_dir: dir.path().to_owned(),
            ws_port: Some(port),
            grace_period_secs: 30,
        };

        let daemon = Arc::new(DaemonState::new(config));
        for (room_id, room_config) in rooms {
            match room_config {
                Some(cfg) => daemon.create_room_with_config(room_id, cfg).await.unwrap(),
                None => daemon.create_room(room_id).await.unwrap(),
            }
        }

        let daemon_run = daemon.clone();
        tokio::spawn(async move {
            daemon_run.run().await.ok();
        });

        common::wait_for_socket(&socket_path, Duration::from_secs(1)).await;
        common::wait_for_tcp(port, Duration::from_secs(1)).await;

        (
            Self {
                socket_path,
                state: daemon,
                _dir: dir,
            },
            port,
        )
    }

    async fn start(rooms: &[&str]) -> Self {
        let rooms_with_config: Vec<(&str, Option<RoomConfig>)> =
            rooms.iter().map(|id| (*id, None)).collect();
        Self::start_with_configs(rooms_with_config).await
    }
}

/// Connect to the daemon socket and perform a ROOM:-prefixed handshake.
async fn daemon_connect(
    socket_path: &PathBuf,
    room_id: &str,
    username: &str,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("ROOM:{room_id}:{username}\n").as_bytes())
        .await
        .unwrap();
    (BufReader::new(r), w)
}

/// One-shot join via the daemon protocol, returns the token UUID.
async fn daemon_join(socket_path: &PathBuf, room_id: &str, username: &str) -> String {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("ROOM:{room_id}:JOIN:{username}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "token", "expected token response: {v}");
    v["token"].as_str().unwrap().to_owned()
}

/// One-shot send via the daemon protocol, returns the broadcast JSON.
async fn daemon_send(
    socket_path: &PathBuf,
    room_id: &str,
    token: &str,
    content: &str,
) -> serde_json::Value {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("ROOM:{room_id}:TOKEN:{token}\n").as_bytes())
        .await
        .unwrap();
    w.write_all(format!("{content}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

// ── Test: daemon rejects connections without ROOM: prefix ─────────────────

#[tokio::test]
async fn daemon_rejects_missing_room_prefix() {
    let td = TestDaemon::start(&["test-room"]).await;

    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    // Send without ROOM: prefix
    w.write_all(b"alice\n").await.unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "missing_room_prefix");
}

// ── Test: daemon rejects connections to nonexistent rooms ─────────────────

#[tokio::test]
async fn daemon_rejects_nonexistent_room() {
    let td = TestDaemon::start(&["real-room"]).await;

    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:fake-room:JOIN:alice\n").await.unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "room_not_found");
}

// ── Test: daemon join + send + receive ────────────────────────────────────

#[tokio::test]
async fn daemon_join_send_receive() {
    let td = TestDaemon::start(&["chat"]).await;

    let token = daemon_join(&td.socket_path, "chat", "alice").await;
    assert!(!token.is_empty());

    let resp = daemon_send(&td.socket_path, "chat", &token, "hello from daemon").await;
    assert_eq!(resp["type"], "message");
    assert_eq!(resp["content"], "hello from daemon");
    assert_eq!(resp["user"], "alice");
}

// ── Test: multi-room message isolation ────────────────────────────────────

#[tokio::test]
async fn daemon_multi_room_message_isolation() {
    let td = TestDaemon::start(&["room-alpha", "room-beta"]).await;

    // Join both rooms with different tokens.
    let token_a = daemon_join(&td.socket_path, "room-alpha", "agent-a").await;
    let token_b = daemon_join(&td.socket_path, "room-beta", "agent-b").await;

    // Connect an interactive client to room-alpha to observe messages.
    let (mut reader_a, _writer_a) =
        daemon_connect(&td.socket_path, "room-alpha", "observer-a").await;

    // Drain history/join messages from the interactive connection.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send a message to room-beta.
    let resp_b = daemon_send(&td.socket_path, "room-beta", &token_b, "beta-only msg").await;
    assert_eq!(resp_b["type"], "message");
    assert_eq!(resp_b["room"], "room-beta");

    // Send a message to room-alpha.
    let resp_a = daemon_send(&td.socket_path, "room-alpha", &token_a, "alpha msg").await;
    assert_eq!(resp_a["type"], "message");
    assert_eq!(resp_a["room"], "room-alpha");

    // The observer in room-alpha should see "alpha msg" but NOT "beta-only msg".
    // Read with a short timeout — we should get the alpha message.
    let mut line = String::new();
    let read_result = timeout(Duration::from_millis(500), reader_a.read_line(&mut line)).await;

    // We may get the join/leave messages first, so drain until we find "alpha msg"
    // or run out of data.
    let mut saw_alpha = false;
    let mut saw_beta = false;

    if read_result.is_ok() {
        // Check this line and keep reading.
        loop {
            if line.contains("alpha msg") {
                saw_alpha = true;
            }
            if line.contains("beta-only msg") {
                saw_beta = true;
            }
            line.clear();
            match timeout(Duration::from_millis(200), reader_a.read_line(&mut line)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
            }
        }
    }

    assert!(saw_alpha, "observer in room-alpha should see alpha msg");
    assert!(
        !saw_beta,
        "observer in room-alpha should NOT see beta-only msg"
    );
}

// ── Test: system-level tokens are valid across all rooms in a daemon ──────
//
// Tokens are daemon-scoped (not room-scoped): a token issued by any room JOIN
// can be used to send to any other room managed by the same daemon (#293).

#[tokio::test]
async fn daemon_token_valid_across_rooms() {
    let td = TestDaemon::start(&["room-x", "room-y"]).await;

    // Join room-x — the resulting token is system-level.
    let token_x = daemon_join(&td.socket_path, "room-x", "user-x").await;

    // Use the token issued in room-x to send to room-y.
    let resp = daemon_send(&td.socket_path, "room-y", &token_x, "cross-room msg").await;
    assert_eq!(
        resp["type"], "message",
        "system-level token should be valid in a different room: {resp}"
    );
    assert_eq!(resp["content"], "cross-room msg");
    assert_eq!(resp["user"], "user-x");
    assert_eq!(resp["room"], "room-y");
}

// ── DM room visibility integration tests ─────────────────────────────────

#[tokio::test]
async fn daemon_dm_room_participants_can_join() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let config = RoomConfig::dm("alice", "bob");
    let td = TestDaemon::start_with_configs(vec![(&dm_id, Some(config))]).await;

    let token_a = daemon_join(&td.socket_path, &dm_id, "alice").await;
    assert!(!token_a.is_empty());

    let token_b = daemon_join(&td.socket_path, &dm_id, "bob").await;
    assert!(!token_b.is_empty());
}

#[tokio::test]
async fn daemon_dm_room_non_participant_rejected() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let config = RoomConfig::dm("alice", "bob");
    let td = TestDaemon::start_with_configs(vec![(&dm_id, Some(config))]).await;

    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("ROOM:{dm_id}:JOIN:eve\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "join_denied");
}

#[tokio::test]
async fn daemon_dm_room_send_and_receive() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let config = RoomConfig::dm("alice", "bob");
    let td = TestDaemon::start_with_configs(vec![(&dm_id, Some(config))]).await;

    let token = daemon_join(&td.socket_path, &dm_id, "alice").await;
    let resp = daemon_send(&td.socket_path, &dm_id, &token, "private hello").await;
    assert_eq!(resp["type"], "message");
    assert_eq!(resp["content"], "private hello");
    assert_eq!(resp["user"], "alice");
    assert_eq!(resp["room"], dm_id);
}

#[tokio::test]
async fn daemon_private_room_requires_invite() {
    let config = RoomConfig {
        visibility: RoomVisibility::Private,
        max_members: None,
        invite_list: ["member".to_owned()].into(),
        created_by: "owner".to_owned(),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    let td = TestDaemon::start_with_configs(vec![("secret-room", Some(config))]).await;

    // Owner can join (creator privilege).
    let token_owner = daemon_join(&td.socket_path, "secret-room", "owner").await;
    assert!(!token_owner.is_empty());

    // Invited member can join.
    let token_member = daemon_join(&td.socket_path, "secret-room", "member").await;
    assert!(!token_member.is_empty());

    // Uninvited user is rejected.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:secret-room:JOIN:stranger\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "join_denied");
}

#[tokio::test]
async fn daemon_public_room_allows_anyone() {
    let config = RoomConfig::public("owner");
    let td = TestDaemon::start_with_configs(vec![("open-room", Some(config))]).await;

    let token = daemon_join(&td.socket_path, "open-room", "random-user").await;
    assert!(!token.is_empty());
}

#[tokio::test]
async fn dm_room_id_both_directions_same() {
    assert_eq!(
        dm_room_id("alice", "bob").unwrap(),
        dm_room_id("bob", "alice").unwrap()
    );
}

// ── Scripted multi-agent test sequences (#180) ──────────────────────────────
//
// Deterministic, pre-scripted coordination scenarios that exercise the exact
// patterns agents use in production: join → send → poll → verify ordering.

/// Three agents join, send messages in a deterministic sequence, then poll.
/// Verifies message ordering and completeness from each agent's perspective.
#[tokio::test]
async fn scripted_three_agent_join_send_poll() {
    let broker = TestBroker::start("t_3agent").await;

    // Phase 1: all three agents join and get tokens
    let (_, tok_a) = room_cli::oneshot::join_session(&broker.socket_path, "agent-a")
        .await
        .unwrap();
    let (_, tok_b) = room_cli::oneshot::join_session(&broker.socket_path, "agent-b")
        .await
        .unwrap();
    let (_, tok_c) = room_cli::oneshot::join_session(&broker.socket_path, "agent-c")
        .await
        .unwrap();

    // Phase 2: scripted message exchange — strict A→B→C→A→B ordering
    let msgs = [
        (&tok_a, "starting task #42"),
        (&tok_b, "ack, reading target file"),
        (&tok_c, "standing by"),
        (&tok_a, "first draft done, running tests"),
        (&tok_b, "review ready"),
    ];
    for (token, content) in &msgs {
        let wire = serde_json::json!({"type": "message", "content": content}).to_string();
        room_cli::oneshot::send_message_with_token(&broker.socket_path, token, &wire)
            .await
            .unwrap();
    }

    // Phase 3: small delay so broker flushes to chat file
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Phase 4: each agent polls and sees all 5 messages in order
    let dir = tempfile::tempdir().unwrap();
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let cursor = dir.path().join(format!("{agent}.cursor"));
        let polled = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor, None, None)
            .await
            .unwrap();

        let contents: Vec<&str> = polled
            .iter()
            .filter_map(|m| match m {
                Message::Message { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(
            contents,
            vec![
                "starting task #42",
                "ack, reading target file",
                "standing by",
                "first draft done, running tests",
                "review ready",
            ],
            "{agent} should see all 5 messages in order"
        );
    }
}

/// Agent sets status, other agents see it via /who response.
#[tokio::test]
async fn scripted_status_visibility_across_agents() {
    let broker = TestBroker::start("t_status_vis").await;

    // host connects interactively to observe
    let mut host = TestClient::connect(&broker.socket_path, "host").await;
    host.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "host"))
        .await;

    // agent-a joins, sets status
    let (_, tok_a) = room_cli::oneshot::join_session(&broker.socket_path, "agent-a")
        .await
        .unwrap();

    let status_wire = serde_json::json!({
        "type": "command",
        "cmd": "set_status",
        "params": ["coding #42"]
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_a, &status_wire)
        .await
        .unwrap();

    // host should see the status system message
    let status_msg = host
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("set status")),
        )
        .await;
    assert!(
        matches!(&status_msg, Message::System { content, .. } if content.contains("coding #42")),
        "status message should contain 'coding #42': {status_msg:?}"
    );
}

/// DM round-trip: agent-a sends DM to agent-b via token auth.
/// agent-b sees it in poll, agent-c (bystander) does not.
#[tokio::test]
async fn scripted_dm_exchange_with_bystander_isolation() {
    let broker = TestBroker::start("t_dm_script").await;

    // host connects first (needed for DM routing — host also sees DMs)
    let mut host = TestClient::connect(&broker.socket_path, "host").await;
    host.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "host"))
        .await;

    // Three agents join (agent-c is a bystander — token unused)
    let (_, tok_a) = room_cli::oneshot::join_session(&broker.socket_path, "agent-a")
        .await
        .unwrap();
    let (_, tok_b) = room_cli::oneshot::join_session(&broker.socket_path, "agent-b")
        .await
        .unwrap();
    let _ = room_cli::oneshot::join_session(&broker.socket_path, "agent-c")
        .await
        .unwrap();

    // agent-a sends DM to agent-b
    let dm_wire = serde_json::json!({
        "type": "dm",
        "to": "agent-b",
        "content": "secret plan"
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_a, &dm_wire)
        .await
        .unwrap();

    // also send a public message so we have a cursor anchor
    let pub_wire = serde_json::json!({"type": "message", "content": "public msg"}).to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_b, &pub_wire)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let dir = tempfile::tempdir().unwrap();

    // agent-b polls: should see the DM
    let cursor_b = dir.path().join("b.cursor");
    let polled_b =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_b, Some("agent-b"), None)
            .await
            .unwrap();
    let b_contents: Vec<&str> = polled_b
        .iter()
        .filter_map(|m| match m {
            Message::DirectMessage { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        b_contents.contains(&"secret plan"),
        "agent-b should see the DM"
    );

    // agent-c polls: should NOT see the DM
    let cursor_c = dir.path().join("c.cursor");
    let polled_c =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_c, Some("agent-c"), None)
            .await
            .unwrap();
    let c_dm_contents: Vec<&str> = polled_c
        .iter()
        .filter_map(|m| match m {
            Message::DirectMessage { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !c_dm_contents.contains(&"secret plan"),
        "agent-c should NOT see the DM"
    );
}

/// Token-authenticated agents can send and poll without interactive sessions.
/// Simulates the exact oneshot workflow agents use in production.
#[tokio::test]
async fn scripted_token_auth_send_poll_workflow() {
    let broker = TestBroker::start("t_token_workflow").await;

    // Phase 1: two agents register
    let (_, tok1) = room_cli::oneshot::join_session(&broker.socket_path, "bot-1")
        .await
        .unwrap();
    let (_, tok2) = room_cli::oneshot::join_session(&broker.socket_path, "bot-2")
        .await
        .unwrap();

    // Phase 2: bot-1 sends a message, bot-2 sends a reply
    let wire1 =
        serde_json::json!({"type": "message", "content": "plan: modify src/lib.rs"}).to_string();
    let echo1 = room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok1, &wire1)
        .await
        .unwrap();
    let anchor_id = echo1.id().to_string();

    let wire2 = serde_json::json!({"type": "message", "content": "ack, no overlap"}).to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok2, &wire2)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Phase 3: bot-1 polls since its own message — should see bot-2's reply only
    let dir = tempfile::tempdir().unwrap();
    let cursor = dir.path().join("bot1.cursor");
    let polled =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor, None, Some(&anchor_id))
            .await
            .unwrap();

    let contents: Vec<&str> = polled
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();

    assert!(
        contents.contains(&"ack, no overlap"),
        "bot-1 should see bot-2's reply"
    );
    assert!(
        !contents.contains(&"plan: modify src/lib.rs"),
        "bot-1's own message should be excluded (before anchor)"
    );
}

/// Daemon multi-room: two agents in separate rooms, messages are isolated.
/// Simulates the production daemon pattern where each room is independent.
#[tokio::test]
async fn scripted_daemon_multi_room_isolation() {
    let td = TestDaemon::start(&["room-alpha", "room-beta"]).await;

    // agent-a joins room-alpha, agent-b joins room-beta
    let tok_a = daemon_join(&td.socket_path, "room-alpha", "agent-a").await;
    let tok_b = daemon_join(&td.socket_path, "room-beta", "agent-b").await;

    // Each sends a message to their own room
    daemon_send(&td.socket_path, "room-alpha", &tok_a, "alpha msg").await;
    daemon_send(&td.socket_path, "room-beta", &tok_b, "beta msg").await;

    // agent-c joins room-alpha — should see alpha msg but NOT beta msg
    let (mut reader_c, _writer_c) = daemon_connect(&td.socket_path, "room-alpha", "agent-c").await;

    // Drain all messages with a short timeout, check for alpha/beta
    let mut saw_alpha = false;
    let mut saw_beta = false;
    let mut line = String::new();
    loop {
        line.clear();
        match timeout(Duration::from_millis(500), reader_c.read_line(&mut line)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(_)) => {
                if line.contains("alpha msg") {
                    saw_alpha = true;
                }
                if line.contains("beta msg") {
                    saw_beta = true;
                }
            }
            Ok(Err(_)) => break,
        }
    }
    assert!(saw_alpha, "agent-c should see alpha msg in room-alpha");
    assert!(!saw_beta, "beta msg must not leak into room-alpha");
}

/// Mention-based message filtering: agent-a sends messages mentioning agent-b,
/// agent-b can filter poll results to only @-mentioned messages.
#[tokio::test]
async fn scripted_mention_filter_in_poll() {
    let broker = TestBroker::start("t_mention_filter").await;

    let (_, tok_a) = room_cli::oneshot::join_session(&broker.socket_path, "agent-a")
        .await
        .unwrap();
    let (_, _tok_b) = room_cli::oneshot::join_session(&broker.socket_path, "agent-b")
        .await
        .unwrap();

    // agent-a sends 3 messages: 2 mention agent-b, 1 does not
    for content in &[
        "@agent-b please review PR #42",
        "running tests now",
        "@agent-b tests pass, merging",
    ] {
        let wire = serde_json::json!({"type": "message", "content": content}).to_string();
        room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_a, &wire)
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Poll all messages — should see all 3
    let dir = tempfile::tempdir().unwrap();
    let cursor_all = dir.path().join("all.cursor");
    let all_msgs = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_all, None, None)
        .await
        .unwrap();
    let all_contents: Vec<&str> = all_msgs
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(all_contents.len(), 3, "should see all 3 messages");

    // Filter to mentions of agent-b using Message::mentions()
    let cursor_mentions = dir.path().join("mentions.cursor");
    let mention_msgs =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_mentions, None, None)
            .await
            .unwrap();
    let mentioned: Vec<&str> = mention_msgs
        .iter()
        .filter(|m| m.mentions().contains(&"agent-b".to_string()))
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        mentioned.len(),
        2,
        "should find 2 messages mentioning agent-b"
    );
    assert!(mentioned.contains(&"@agent-b please review PR #42"));
    assert!(mentioned.contains(&"@agent-b tests pass, merging"));
}

/// Full coordination lifecycle: join → announce → status → work → poll → PR.
/// This mirrors the exact sequence from CLAUDE.md's "Expected behaviour" section.
#[tokio::test]
async fn scripted_full_coordination_lifecycle() {
    let broker = TestBroker::start("t_lifecycle").await;

    // BA (host) connects interactively
    let mut ba = TestClient::connect(&broker.socket_path, "ba").await;
    ba.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "ba"))
        .await;

    // Two worker agents register
    let (_, tok_r2d2) = room_cli::oneshot::join_session(&broker.socket_path, "r2d2")
        .await
        .unwrap();
    let (_, tok_bb) = room_cli::oneshot::join_session(&broker.socket_path, "bb")
        .await
        .unwrap();

    // Step 1: r2d2 announces plan
    let wire = serde_json::json!({
        "type": "message",
        "content": "plan: implement #42. files: src/lib.rs, src/main.rs"
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_r2d2, &wire)
        .await
        .unwrap();

    // BA sees the announcement
    let plan_msg = ba
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content.contains("plan:")))
        .await;
    assert_eq!(plan_msg.user(), "r2d2");

    // Step 2: BA approves
    ba.send_text("go ahead").await;

    // Step 3: r2d2 sets status
    let status_wire = serde_json::json!({
        "type": "command",
        "cmd": "set_status",
        "params": ["coding #42"]
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_r2d2, &status_wire)
        .await
        .unwrap();

    // Step 4: r2d2 sends milestone update
    let milestone = serde_json::json!({
        "type": "message",
        "content": "first draft done, running tests"
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_r2d2, &milestone)
        .await
        .unwrap();

    // Step 5: bb sends a message too (concurrent work)
    let bb_msg = serde_json::json!({
        "type": "message",
        "content": "PR #43 ready for review @ba"
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_bb, &bb_msg)
        .await
        .unwrap();

    // Step 6: r2d2 announces PR
    let pr_wire = serde_json::json!({
        "type": "message",
        "content": "opening PR for #42. modified: src/lib.rs, src/main.rs"
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_r2d2, &pr_wire)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify: poll the full history — messages are in order with correct senders
    let dir = tempfile::tempdir().unwrap();
    let cursor = dir.path().join("verify.cursor");
    let history = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor, None, None)
        .await
        .unwrap();

    let chat_msgs: Vec<(&str, &str)> = history
        .iter()
        .filter_map(|m| match m {
            Message::Message { user, content, .. } => Some((user.as_str(), content.as_str())),
            _ => None,
        })
        .collect();

    // Verify ordering: r2d2 plan → ba go → r2d2 milestone → bb PR → r2d2 PR
    assert_eq!(chat_msgs[0].0, "r2d2");
    assert!(chat_msgs[0].1.contains("plan:"));
    assert_eq!(chat_msgs[1].0, "ba");
    assert!(chat_msgs[1].1.contains("go ahead"));
    assert_eq!(chat_msgs[2].0, "r2d2");
    assert!(chat_msgs[2].1.contains("first draft"));
    assert_eq!(chat_msgs[3].0, "bb");
    assert!(chat_msgs[3].1.contains("PR #43"));
    assert_eq!(chat_msgs[4].0, "r2d2");
    assert!(chat_msgs[4].1.contains("opening PR"));
}

/// Cursor isolation: two agents polling the same room maintain independent cursors.
/// Agent-a polls first (advances cursor), agent-b polls and sees everything.
#[tokio::test]
async fn scripted_cursor_isolation_between_agents() {
    let broker = TestBroker::start("t_cursor_iso").await;

    let (_, tok) = room_cli::oneshot::join_session(&broker.socket_path, "sender")
        .await
        .unwrap();

    // Send 3 messages
    for i in 1..=3 {
        let wire =
            serde_json::json!({"type": "message", "content": format!("msg-{i}")}).to_string();
        room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok, &wire)
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    let dir = tempfile::tempdir().unwrap();

    // agent-a polls — sees all 3, advances its cursor
    let cursor_a = dir.path().join("a.cursor");
    let polled_a = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_a, None, None)
        .await
        .unwrap();
    let a_contents: Vec<&str> = polled_a
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(a_contents, vec!["msg-1", "msg-2", "msg-3"]);

    // agent-a polls again — nothing new
    let polled_a2 = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_a, None, None)
        .await
        .unwrap();
    let a2_msgs: Vec<&str> = polled_a2
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert!(a2_msgs.is_empty(), "agent-a second poll should be empty");

    // agent-b polls with independent cursor — sees all 3
    let cursor_b = dir.path().join("b.cursor");
    let polled_b = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_b, None, None)
        .await
        .unwrap();
    let b_contents: Vec<&str> = polled_b
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(b_contents, vec!["msg-1", "msg-2", "msg-3"]);

    // New message arrives — only agents who haven't seen it get it
    let wire4 = serde_json::json!({"type": "message", "content": "msg-4"}).to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok, &wire4)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let polled_a3 = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_a, None, None)
        .await
        .unwrap();
    let a3_contents: Vec<&str> = polled_a3
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(a3_contents, vec!["msg-4"], "agent-a should see only msg-4");

    let polled_b2 = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_b, None, None)
        .await
        .unwrap();
    let b2_contents: Vec<&str> = polled_b2
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(b2_contents, vec!["msg-4"], "agent-b should see only msg-4");
}

// ── WS/REST DM permission tests (#301) ───────────────────────────────────────
// Defence-in-depth: check_send_permission unit tests live in auth.rs (6 tests).
// These integration tests verify that the join gate blocks non-participants at
// the REST and WS layers, and that the send path is wired correctly for
// authorised participants.

/// REST POST /api/{room}/join as a non-DM-participant returns 403 join_denied.
#[tokio::test]
async fn rest_dm_room_non_participant_join_rejected() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let dm_config = RoomConfig::dm("alice", "bob");
    let (_td, port) =
        TestDaemon::start_with_ws_configs(vec![(dm_id.as_str(), Some(dm_config))]).await;

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // eve is not a DM participant — join should be rejected.
    let join_resp = client
        .post(format!("{base}/api/{dm_id}/join"))
        .json(&serde_json::json!({"username": "eve"}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        join_resp.status(),
        403,
        "non-participant join should be 403"
    );
    let body: serde_json::Value = join_resp.json().await.unwrap();
    assert_eq!(body["code"], "join_denied");
}

/// WS interactive handshake as a non-DM-participant should be rejected
/// (the server closes the connection without issuing a token).
#[tokio::test]
async fn ws_dm_room_non_participant_join_rejected() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let dm_config = RoomConfig::dm("alice", "bob");
    let (_td, port) =
        TestDaemon::start_with_ws_configs(vec![(dm_id.as_str(), Some(dm_config))]).await;

    // eve tries an interactive WS handshake with her username.
    let url = format!("ws://127.0.0.1:{port}/ws/{dm_id}");
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();
    tx.send(TungsteniteMsg::Text("eve".into())).await.unwrap();

    // The server should reject eve — expect an error frame or connection close.
    let resp = tokio::time::timeout(Duration::from_secs(2), rx.next()).await;
    match resp {
        Ok(Some(Ok(TungsteniteMsg::Text(text)))) => {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["type"], "error", "expected join error, got: {v}");
        }
        Ok(Some(Ok(TungsteniteMsg::Close(_)))) => {
            // Connection closed without a message — also acceptable rejection.
        }
        Ok(None) => {
            // Stream ended — connection was closed, which is a valid rejection.
        }
        _ => {
            // Timeout or other — eve was not allowed in either way.
        }
    }
}

/// REST POST /api/{room}/send as a DM participant succeeds.
#[tokio::test]
async fn rest_dm_room_participant_send_allowed() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let dm_config = RoomConfig::dm("alice", "bob");
    let (_td, port) =
        TestDaemon::start_with_ws_configs(vec![(dm_id.as_str(), Some(dm_config))]).await;

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // alice is a DM participant — join to get a token.
    let join_resp = client
        .post(format!("{base}/api/{dm_id}/join"))
        .json(&serde_json::json!({"username": "alice"}))
        .send()
        .await
        .unwrap();
    assert_eq!(join_resp.status(), 200);
    let join_body: serde_json::Value = join_resp.json().await.unwrap();
    let token = join_body["token"].as_str().unwrap();

    // alice sends a message — should succeed.
    let send_resp = client
        .post(format!("{base}/api/{dm_id}/send"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"content": "hello bob"}))
        .send()
        .await
        .unwrap();

    assert_eq!(send_resp.status(), 200, "participant send should succeed");
    let body: serde_json::Value = send_resp.json().await.unwrap();
    assert_eq!(body["content"], "hello bob");
    assert_eq!(body["user"], "alice");
}

/// A non-participant with a valid token (injected directly, simulating
/// future system-level tokens) must be rejected with send_denied via REST.
#[tokio::test]
async fn rest_dm_room_non_participant_send_rejected() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let dm_config = RoomConfig::dm("alice", "bob");
    let (td, port) =
        TestDaemon::start_with_ws_configs(vec![(dm_id.as_str(), Some(dm_config))]).await;

    // Inject a token for eve directly, bypassing join permission.
    // This simulates future system-level token issuance (issue #293).
    let eve_token = "test-token-eve-12345";
    td.state
        .test_inject_token(&dm_id, "eve", eve_token)
        .await
        .unwrap();

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Eve tries to send — should be rejected with 403 send_denied.
    let send_resp = client
        .post(format!("{base}/api/{dm_id}/send"))
        .header("Authorization", format!("Bearer {eve_token}"))
        .json(&serde_json::json!({"content": "unauthorized message"}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        send_resp.status(),
        403,
        "non-participant send should be 403"
    );
    let body: serde_json::Value = send_resp.json().await.unwrap();
    assert_eq!(body["code"], "send_denied");
}

// ── REST /api/{room_id}/query tests ────────────────────────────────────────────

/// Helper: join a room via REST and return the token string.
async fn rest_join(client: &reqwest::Client, base: &str, room_id: &str, username: &str) -> String {
    let resp = client
        .post(format!("{base}/api/{room_id}/join"))
        .json(&serde_json::json!({"username": username}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "rest_join failed for {username}");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_owned()
}

/// Helper: send a message via REST and return the response body.
async fn rest_send(
    client: &reqwest::Client,
    base: &str,
    room_id: &str,
    token: &str,
    content: &str,
) -> serde_json::Value {
    let resp = client
        .post(format!("{base}/api/{room_id}/send"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"content": content}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "rest_send failed for content={content}");
    resp.json().await.unwrap()
}

#[tokio::test]
async fn rest_query_without_token_returns_401() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_noauth").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://127.0.0.1:{port}/api/ws_query_noauth/query"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "missing_token");
}

#[tokio::test]
async fn rest_query_with_invalid_token_returns_401() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_badauth").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/api/ws_query_badauth/query"
        ))
        .header("Authorization", "Bearer not-a-valid-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_token");
}

#[tokio::test]
async fn rest_query_wrong_room_returns_404() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_404room").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://127.0.0.1:{port}/api/nosuchroom/query"))
        .header("Authorization", "Bearer dummy")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn rest_query_no_params_returns_all_messages() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_all").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_all", "alice").await;
    rest_send(&client, &base, "ws_query_all", &token, "first message").await;
    rest_send(&client, &base, "ws_query_all", &token, "second message").await;

    let resp = client
        .get(format!("{base}/api/ws_query_all/query"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(|m| m["content"] == "first message"),
        "first message should appear"
    );
    assert!(
        messages.iter().any(|m| m["content"] == "second message"),
        "second message should appear"
    );
}

#[tokio::test]
async fn rest_query_user_filter() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_user").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let alice_tok = rest_join(&client, &base, "ws_query_user", "alice_qu").await;
    let bob_tok = rest_join(&client, &base, "ws_query_user", "bob_qu").await;

    rest_send(&client, &base, "ws_query_user", &alice_tok, "from alice").await;
    rest_send(&client, &base, "ws_query_user", &bob_tok, "from bob").await;

    let resp = client
        .get(format!("{base}/api/ws_query_user/query?user=alice_qu"))
        .header("Authorization", format!("Bearer {alice_tok}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert!(
        messages.iter().all(|m| m["user"] == "alice_qu"),
        "only alice's messages should be returned"
    );
    assert!(
        messages.iter().any(|m| m["content"] == "from alice"),
        "alice's message should be present"
    );
    assert!(
        !messages.iter().any(|m| m["content"] == "from bob"),
        "bob's message should not appear"
    );
}

#[tokio::test]
async fn rest_query_limit_and_ordering() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_limit").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_limit", "alice_lim").await;
    rest_send(&client, &base, "ws_query_limit", &token, "msg1").await;
    rest_send(&client, &base, "ws_query_limit", &token, "msg2").await;
    rest_send(&client, &base, "ws_query_limit", &token, "msg3").await;

    // n=2 newest-first (default).
    let resp = client
        .get(format!("{base}/api/ws_query_limit/query?n=2"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "should return exactly 2 messages");
    // Newest-first: msg3 then msg2.
    assert_eq!(messages[0]["content"], "msg3", "newest message first");
    assert_eq!(messages[1]["content"], "msg2");

    // asc=true: oldest-first, n=2.
    let resp_asc = client
        .get(format!("{base}/api/ws_query_limit/query?n=2&asc=true"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    let body_asc: serde_json::Value = resp_asc.json().await.unwrap();
    let msgs_asc = body_asc["messages"].as_array().unwrap();
    assert_eq!(msgs_asc.len(), 2);
    assert_eq!(
        msgs_asc[0]["content"], "msg1",
        "oldest message first with asc=true"
    );
}

#[tokio::test]
async fn rest_query_content_filter() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_content").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_content", "alice_cnt").await;
    rest_send(&client, &base, "ws_query_content", &token, "hello world").await;
    rest_send(&client, &base, "ws_query_content", &token, "goodbye world").await;
    rest_send(&client, &base, "ws_query_content", &token, "nothing here").await;

    let resp = client
        .get(format!("{base}/api/ws_query_content/query?content=world"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "two messages contain 'world'");
    assert!(messages.iter().any(|m| m["content"] == "hello world"));
    assert!(messages.iter().any(|m| m["content"] == "goodbye world"));
    assert!(!messages.iter().any(|m| m["content"] == "nothing here"));
}

#[tokio::test]
async fn rest_query_since_filter() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_since").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_since", "alice_s").await;
    rest_send(&client, &base, "ws_query_since", &token, "msg_a").await;
    rest_send(&client, &base, "ws_query_since", &token, "msg_b").await;
    rest_send(&client, &base, "ws_query_since", &token, "msg_c").await;

    // Get all messages oldest-first to find msg_b's seq.
    let all_resp = client
        .get(format!("{base}/api/ws_query_since/query?asc=true"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    let all_body: serde_json::Value = all_resp.json().await.unwrap();
    let all_msgs = all_body["messages"].as_array().unwrap();
    let msg_b_seq = all_msgs
        .iter()
        .find(|m| m["content"] == "msg_b")
        .and_then(|m| m["seq"].as_u64())
        .expect("msg_b should have a seq");

    // since=msg_b_seq — should only return msg_c (strictly after).
    let resp = client
        .get(format!(
            "{base}/api/ws_query_since/query?since={msg_b_seq}&asc=true"
        ))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert!(
        messages.iter().all(|m| m["content"] != "msg_a"),
        "msg_a should be excluded"
    );
    assert!(
        messages.iter().all(|m| m["content"] != "msg_b"),
        "msg_b itself should be excluded"
    );
    assert!(
        messages.iter().any(|m| m["content"] == "msg_c"),
        "msg_c should be included"
    );
}

#[tokio::test]
async fn rest_query_dm_privacy_enforced() {
    // Non-participant cannot see DM messages via /query.
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let dm_config = RoomConfig::dm("alice", "bob");
    let (td, port) =
        TestDaemon::start_with_ws_configs(vec![(dm_id.as_str(), Some(dm_config))]).await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // alice sends a DM to bob (inject token directly, bypassing join permission check).
    let alice_tok = "tok-alice-query";
    td.state
        .test_inject_token(&dm_id, "alice", alice_tok)
        .await
        .unwrap();
    let send_resp = client
        .post(format!("{base}/api/{dm_id}/send"))
        .header("Authorization", format!("Bearer {alice_tok}"))
        .json(&serde_json::json!({"content": "secret dm", "to": "bob"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        send_resp.status(),
        200,
        "alice should be able to send to dm room"
    );

    // eve has a token but is not a participant.
    let eve_tok = "tok-eve-query";
    td.state
        .test_inject_token(&dm_id, "eve", eve_tok)
        .await
        .unwrap();
    let resp = client
        .get(format!("{base}/api/{dm_id}/query"))
        .header("Authorization", format!("Bearer {eve_tok}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert!(
        !messages.iter().any(|m| m["content"] == "secret dm"),
        "non-participant must not see DM messages via /query"
    );
}

#[tokio::test]
async fn rest_query_public_alone_returns_400() {
    // ?public=true without any other narrowing param should be rejected.
    let (_tb, port) = TestBroker::start_with_ws("ws_query_pub400").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_pub400", "alice_pub").await;

    let resp = client
        .get(format!("{base}/api/ws_query_pub400/query?public=true"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "public_requires_filter");
}

#[tokio::test]
async fn rest_query_public_with_narrowing_param_allowed() {
    // ?public=true with at least one narrowing param should succeed.
    let (_tb, port) = TestBroker::start_with_ws("ws_query_pub_ok").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_pub_ok", "alice_pubq").await;
    rest_send(&client, &base, "ws_query_pub_ok", &token, "hello").await;

    let resp = client
        .get(format!("{base}/api/ws_query_pub_ok/query?public=true&n=10"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["messages"].is_array());
}

// ── CREATE: protocol tests ──────────────────────────────────────────────────

/// Send a CREATE:<room_id> request to the daemon socket.
/// Returns the response JSON.
async fn daemon_create(
    socket_path: &PathBuf,
    room_id: &str,
    config_json: &str,
) -> serde_json::Value {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("CREATE:{room_id}\n").as_bytes())
        .await
        .unwrap();
    w.write_all(format!("{config_json}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

#[tokio::test]
async fn create_room_via_uds_then_join_and_send() {
    // Start a daemon with no pre-created rooms.
    let td = TestDaemon::start(&[]).await;

    // Create a room dynamically.
    let resp = daemon_create(
        &td.socket_path,
        "dynamic-room",
        r#"{"visibility":"public"}"#,
    )
    .await;
    assert_eq!(resp["type"], "room_created");
    assert_eq!(resp["room"], "dynamic-room");

    // Join the newly created room.
    let token = daemon_join(&td.socket_path, "dynamic-room", "alice").await;
    assert!(!token.is_empty());

    // Send a message to it.
    let msg = daemon_send(&td.socket_path, "dynamic-room", &token, "hello dynamic").await;
    assert_eq!(msg["type"], "message");
    assert_eq!(msg["content"], "hello dynamic");
    assert_eq!(msg["user"], "alice");
}

#[tokio::test]
async fn create_room_duplicate_returns_error() {
    let td = TestDaemon::start(&["existing-room"]).await;

    // Try to create a room that already exists.
    let resp = daemon_create(
        &td.socket_path,
        "existing-room",
        r#"{"visibility":"public"}"#,
    )
    .await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "room_exists");
}

#[tokio::test]
async fn create_room_invalid_id_returns_error() {
    let td = TestDaemon::start(&[]).await;

    // Room ID with path traversal.
    let resp = daemon_create(&td.socket_path, "../escape", r#"{"visibility":"public"}"#).await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_room_id");

    // Empty room ID.
    let resp2 = daemon_create(&td.socket_path, "", r#"{"visibility":"public"}"#).await;
    assert_eq!(resp2["type"], "error");
    assert_eq!(resp2["code"], "invalid_room_id");
}

#[tokio::test]
async fn create_dm_room_via_uds() {
    let td = TestDaemon::start(&[]).await;

    // Create a DM room with exactly 2 users.
    let resp = daemon_create(
        &td.socket_path,
        "dm-alice-bob",
        r#"{"visibility":"dm","invite":["alice","bob"]}"#,
    )
    .await;
    assert_eq!(resp["type"], "room_created");
    assert_eq!(resp["room"], "dm-alice-bob");

    // alice can join.
    let token = daemon_join(&td.socket_path, "dm-alice-bob", "alice").await;
    assert!(!token.is_empty());

    // eve cannot join (not in invite list).
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:dm-alice-bob:JOIN:eve\n").await.unwrap();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error", "eve should be rejected: {v}");
    assert_eq!(v["code"], "join_denied");
}

#[tokio::test]
async fn create_dm_room_wrong_invite_count_returns_error() {
    let td = TestDaemon::start(&[]).await;

    // DM with only 1 user — should fail.
    let resp = daemon_create(
        &td.socket_path,
        "dm-solo",
        r#"{"visibility":"dm","invite":["alice"]}"#,
    )
    .await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_config");

    // DM with 3 users — should fail.
    let resp2 = daemon_create(
        &td.socket_path,
        "dm-three",
        r#"{"visibility":"dm","invite":["alice","bob","carol"]}"#,
    )
    .await;
    assert_eq!(resp2["type"], "error");
    assert_eq!(resp2["code"], "invalid_config");
}

#[tokio::test]
async fn create_room_default_config() {
    let td = TestDaemon::start(&[]).await;

    // Empty config line — should default to public.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"CREATE:default-room\n\n").await.unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "room_created");
    assert_eq!(v["room"], "default-room");

    // Should be joinable (public).
    let token = daemon_join(&td.socket_path, "default-room", "user1").await;
    assert!(!token.is_empty());
}

#[tokio::test]
async fn create_room_invalid_json_returns_error() {
    let td = TestDaemon::start(&[]).await;

    let resp = daemon_create(&td.socket_path, "bad-config", "not valid json").await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_config");
}

#[tokio::test]
async fn create_room_unknown_visibility_returns_error() {
    let td = TestDaemon::start(&[]).await;

    let resp = daemon_create(&td.socket_path, "weird-room", r#"{"visibility":"secret"}"#).await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_config");
}
