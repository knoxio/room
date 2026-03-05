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
        w.write_all(format!("{username}\n").as_bytes()).await.unwrap();
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
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice")).await;

    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    // drain alice's perspective: bob's join
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob")).await;
    // drain bob's perspective: his own join
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob")).await;

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
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice")).await;

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
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice")).await;

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
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice")).await;
    for i in 0..5usize {
        alice.send_text(&format!("msg {i}")).await;
        alice
            .recv_until(|m| matches!(m, Message::Message { content, .. } if content == &format!("msg {i}")))
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
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice")).await;

    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob")).await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob")).await;

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
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice")).await;
    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob")).await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob")).await;

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
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice")).await;

    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob")).await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob")).await;

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
    alice.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice")).await;

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
    c1.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "user1")).await;

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
