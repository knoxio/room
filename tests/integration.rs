/// Integration tests for the room broker.
///
/// Each test spins up a real broker (bound to a temp socket), connects raw
/// Unix-socket clients, and verifies behaviour at the wire level.
use std::{path::PathBuf, time::Duration};

use room::{
    broker::Broker,
    history,
    message::{self, Message},
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};

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
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join(format!("{room_id}.sock"));
        let chat_path = dir.path().join(format!("{room_id}.chat"));

        let broker = Broker::new(room_id, chat_path.clone(), socket_path.clone());
        tokio::spawn(async move {
            broker.run().await.ok();
        });

        // Wait until the socket file appears (broker has bound)
        for _ in 0..100 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(socket_path.exists(), "broker did not start in time");

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

    alice
        .send_json(r#"{"type":"command","cmd":"claim","params":["task-42"]}"#)
        .await;

    let msg = bob
        .recv_until(|m| matches!(m, Message::Command { cmd, .. } if cmd == "claim"))
        .await;
    assert_eq!(msg.user(), "alice");
    if let Message::Command { params, .. } = &msg {
        assert_eq!(params, &["task-42"]);
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

    let broker = Broker::new("pre", chat_path.clone(), socket_path.clone());
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
    let broker = Broker::new("stale", chat_path, socket_path.clone());
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
    let msg = room::message::make_message("r", "u", "test to /tmp");
    room::history::append(&path, &msg).await.unwrap();
    let loaded = room::history::load(&path).await.unwrap();
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

    room::oneshot::send_message(&broker.socket_path, "bot", "hello from bot")
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

    let msg = room::oneshot::send_message(&broker.socket_path, "bot", "test echo")
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

    let msgs = room::oneshot::poll_messages(&broker.chat_path, &cursor_path, None, Some(m1.id()))
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
    let msgs = room::oneshot::poll_messages(&broker.chat_path, &cursor_path, None, None)
        .await
        .unwrap();
    assert!(!msgs.is_empty(), "first poll should return messages");
    assert!(
        cursor_path.exists(),
        "cursor file must be written after poll"
    );

    // Second poll: cursor is up to date, nothing new
    let msgs2 = room::oneshot::poll_messages(&broker.chat_path, &cursor_path, None, None)
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

    let msg = room::message::make_message("offline", "ghost", "written directly");
    room::history::append(&chat_path, &msg).await.unwrap();

    let msgs = room::oneshot::poll_messages(&chat_path, &cursor_path, None, None)
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

    let result = room::oneshot::send_message(&socket_path, "bot", "hello").await;
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
    room::oneshot::send_message(&broker.socket_path, "agent", &wire)
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
    let dm = room::message::make_dm("r", "alice", "bob", "eyes only");
    room::history::append(&chat_path, &dm).await.unwrap();

    // carol is not party to the DM — she should see nothing
    let msgs = room::oneshot::poll_messages(&chat_path, &cursor_path, Some("carol"), None)
        .await
        .unwrap();
    assert!(
        msgs.is_empty(),
        "carol should not see a DM she is not party to"
    );

    // alice (sender) should see it
    let alice_cursor = dir.path().join("alice.cursor");
    let msgs = room::oneshot::poll_messages(&chat_path, &alice_cursor, Some("alice"), None)
        .await
        .unwrap();
    assert_eq!(msgs.len(), 1, "alice should see the DM she sent");

    // bob (recipient) should see it
    let bob_cursor = dir.path().join("bob.cursor");
    let msgs = room::oneshot::poll_messages(&chat_path, &bob_cursor, Some("bob"), None)
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
    let dm = room::message::make_dm("replay_dm", "bob", "carol", "for bob and carol only");
    room::history::append(&chat_path, &dm).await.unwrap();

    let broker = Broker::new("replay_dm", chat_path, socket_path.clone());
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
