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

// ── Token auth tests ──────────────────────────────────────────────────────────

/// `join_session` returns a non-empty token and the correct username on success.
#[tokio::test]
async fn join_session_returns_token() {
    let broker = TestBroker::start("t_join_token").await;
    let (username, token) = room::oneshot::join_session(&broker.socket_path, "agent1")
        .await
        .expect("join_session failed");
    assert_eq!(username, "agent1");
    assert!(!token.is_empty(), "token must be non-empty");
}

/// A second `join_session` for the same username is rejected with a clear error.
#[tokio::test]
async fn join_session_rejects_duplicate_username() {
    let broker = TestBroker::start("t_join_dup").await;
    room::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .expect("first join failed");
    let err = room::oneshot::join_session(&broker.socket_path, "bot")
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
    let (_, tok1) = room::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .expect("alice join failed");
    let (_, tok2) = room::oneshot::join_session(&broker.socket_path, "bob")
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

    let (_, token) = room::oneshot::join_session(&broker.socket_path, "agent")
        .await
        .expect("join_session failed");

    let wire = serde_json::json!({"type": "message", "content": "hello from token"}).to_string();
    let msg = room::oneshot::send_message_with_token(&broker.socket_path, &token, &wire)
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
        room::oneshot::send_message_with_token(&broker.socket_path, "not-a-real-token", &wire)
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
    let (_, token) = room::oneshot::join_session(&broker.socket_path, "agent")
        .await
        .expect("join_session failed");

    for i in 0..2u8 {
        let wire =
            serde_json::json!({"type": "message", "content": format!("msg {i}")}).to_string();
        room::oneshot::send_message_with_token(&broker.socket_path, &token, &wire)
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

    let (_, victim_token) = room::oneshot::join_session(&broker.socket_path, "victim")
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
    let err = room::oneshot::send_message_with_token(&broker.socket_path, &victim_token, &wire)
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

    let (_, tok_a) = room::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .unwrap();
    let (_, tok_b) = room::oneshot::join_session(&broker.socket_path, "bob")
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
        let result = room::oneshot::send_message_with_token(&broker.socket_path, tok, &wire).await;
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
    let (_, _alice_token) = room::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .expect("alice join failed");

    // Alice is now registered; a second join would fail
    let err = room::oneshot::join_session(&broker.socket_path, "alice")
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
    let result = room::oneshot::join_session(&broker.socket_path, "alice").await;
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

    let (_, tok1) = room::oneshot::join_session(&broker.socket_path, "u1")
        .await
        .unwrap();
    let (_, tok2) = room::oneshot::join_session(&broker.socket_path, "u2")
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
        let result = room::oneshot::send_message_with_token(&broker.socket_path, tok, &wire).await;
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
    room::oneshot::send_message(&broker.socket_path, "admin", &wire)
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

    let msgs = room::oneshot::pull_messages(&broker.chat_path, 3, None)
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

    let msgs = room::oneshot::pull_messages(&broker.chat_path, 100, None)
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
    let msgs = room::oneshot::pull_messages(&chat_path, 20, None)
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
    let meta_path = PathBuf::from(format!("/tmp/room-{room_id}.meta"));
    let meta = serde_json::json!({ "chat_path": broker.chat_path.to_string_lossy() });
    std::fs::write(&meta_path, format!("{meta}\n")).unwrap();

    // Join to obtain a token and write the token file.
    let (_user, token) = room::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .unwrap();
    let token_path = room::oneshot::token_file_path(room_id, "alice");
    let token_data = serde_json::json!({ "username": "alice", "token": token });
    std::fs::write(&token_path, format!("{token_data}\n")).unwrap();

    // Send first message via one-shot.
    room::oneshot::send_message_with_token(
        &broker.socket_path,
        &token,
        r#"{"type":"message","content":"first"}"#,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // cmd_poll advances the canonical cursor.
    room::oneshot::cmd_poll(room_id, &token, None)
        .await
        .unwrap();

    let cursor_path = PathBuf::from(format!("/tmp/room-{room_id}-alice.cursor"));
    let cursor_after_poll = std::fs::read_to_string(&cursor_path).unwrap();

    // Send a second message after the cursor.
    room::oneshot::send_message_with_token(
        &broker.socket_path,
        &token,
        r#"{"type":"message","content":"second"}"#,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // cmd_pull must not move the cursor.
    room::oneshot::cmd_pull(room_id, &token, 5).await.unwrap();

    let cursor_after_pull = std::fs::read_to_string(&cursor_path).unwrap();
    assert_eq!(
        cursor_after_poll, cursor_after_pull,
        "cmd_pull must not advance the poll cursor at /tmp/room-{room_id}-alice.cursor"
    );

    // Verify poll still returns "second" (cursor was not consumed by pull).
    let msgs = room::oneshot::poll_messages(
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
    let carol_msgs = room::oneshot::pull_messages(&broker.chat_path, 50, Some("carol"))
        .await
        .unwrap();
    assert!(
        !carol_msgs
            .iter()
            .any(|m| matches!(m, Message::DirectMessage { content, .. } if content == "secret")),
        "carol should not see the DM between alice and bob"
    );

    // alice pulls — should see the DM
    let alice_msgs = room::oneshot::pull_messages(&broker.chat_path, 50, Some("alice"))
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

    let (_, tok) = room::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .unwrap();

    let wire = serde_json::json!({"type":"command","cmd":"who","params":[]}).to_string();
    let msg = room::oneshot::send_message_with_token(&broker.socket_path, &tok, &wire)
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
    let (_, tok) = room::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .unwrap();
    let msg = room::oneshot::send_message_with_token(
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
