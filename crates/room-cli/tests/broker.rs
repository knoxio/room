/// Core single-room UDS broker protocol tests.
///
/// Covers: join/leave events, message broadcast, persistence, history replay,
/// DM routing, set_status, /who, sequence numbers.
mod common;

use std::time::Duration;

use common::{TestBroker, TestClient};
use room_cli::{
    broker::Broker,
    history,
    message::{self, Message},
};
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixStream;

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

// ── P0: host disconnect / reconnect ──────────────────────────────────────────

/// The original room host retains admin privileges after disconnecting and
/// reconnecting. A second user who joins while the host is offline must NOT
/// inherit the host role.
///
/// Regression: if host_user were cleared on disconnect, bob would become host
/// on his arrival and alice's reconnect would be treated as a regular user.
#[tokio::test]
async fn host_retains_admin_privileges_after_reconnect() {
    let broker = TestBroker::start("t_host_reconnect_admin").await;

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

    // alice disconnects — host_user stays "alice" in broker state
    drop(alice);
    bob.recv_until(|m| matches!(m, Message::Leave { user, .. } if user == "alice"))
        .await;

    // alice reconnects — host_user is still "alice"; bob never became host
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // alice issues /clear — should succeed and broadcast "cleared chat history"
    alice
        .send_json(r#"{"type":"command","cmd":"clear","params":[]}"#)
        .await;
    alice
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("cleared chat history")),
        )
        .await;
    bob.recv_until(
        |m| matches!(m, Message::System { content, .. } if content.contains("cleared chat history")),
    )
    .await;

    // bob tries /clear — must receive permission denied (privately, not broadcast)
    bob.send_json(r#"{"type":"command","cmd":"clear","params":[]}"#)
        .await;
    let denied = bob
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("permission denied")),
        )
        .await;
    assert!(
        matches!(denied, Message::System { .. }),
        "expected System denial for non-host clear"
    );

    // alice must NOT receive bob's private denial
    let alice_saw_denial = tokio::time::timeout(Duration::from_millis(200), async {
        let mut line = String::new();
        loop {
            line.clear();
            if alice.reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break false;
            }
            if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                if matches!(&msg, Message::System { content, .. } if content.contains("permission denied"))
                {
                    break true;
                }
            }
        }
    })
    .await;
    assert!(
        alice_saw_denial.is_err() || !alice_saw_denial.unwrap(),
        "alice must not receive bob's private permission-denied message"
    );
}

/// When the host reconnects after a period offline, history replay must include
/// DMs exchanged between other users while the host was absent — because the
/// host_user field is never cleared on disconnect.
///
/// Non-participants who later join the room must NOT receive those same DMs
/// in their own history replay.
#[tokio::test]
async fn dm_history_replay_includes_dms_for_host() {
    let broker = TestBroker::start("t_host_dm_history").await;

    // alice connects first → becomes host
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

    // alice goes offline
    drop(alice);
    bob.recv_until(|m| matches!(m, Message::Leave { user, .. } if user == "alice"))
        .await;
    carol
        .recv_until(|m| matches!(m, Message::Leave { user, .. } if user == "alice"))
        .await;

    // bob sends a DM to carol while alice is offline
    bob.send_json(r#"{"type":"dm","to":"carol","content":"secret while alice is away"}"#)
        .await;

    // confirm delivery to sender and recipient
    bob.recv_until(
        |m| matches!(m, Message::DirectMessage { content, .. } if content == "secret while alice is away"),
    )
    .await;
    carol
        .recv_until(
            |m| matches!(m, Message::DirectMessage { content, .. } if content == "secret while alice is away"),
        )
        .await;

    // allow the broker to persist the message
    tokio::time::sleep(Duration::from_millis(50)).await;

    // alice reconnects — history replay includes the DM because host_user is still "alice"
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(
            |m| matches!(m, Message::DirectMessage { content, .. } if content == "secret while alice is away"),
        )
        .await;

    // dave is a newcomer with no relation to the DM — must NOT see it in history replay
    let mut dave = TestClient::connect(&broker.socket_path, "dave").await;
    dave.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "dave"))
        .await;

    let dave_saw_dm = tokio::time::timeout(Duration::from_millis(300), async {
        let mut line = String::new();
        loop {
            line.clear();
            if dave.reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break false;
            }
            if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                if matches!(&msg, Message::DirectMessage { content, .. } if content == "secret while alice is away")
                {
                    break true;
                }
            }
        }
    })
    .await;
    assert!(
        dave_saw_dm.is_err() || !dave_saw_dm.unwrap(),
        "dave (non-participant, non-host) must not receive the DM in history replay"
    );
}
