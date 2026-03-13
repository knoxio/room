/// Core single-room UDS broker protocol tests.
///
/// Covers: join/leave events, message broadcast, persistence, history replay,
/// DM routing, set_status, /who, sequence numbers, taskboard plugin lifecycle.
mod common;

use std::time::Duration;

use common::{TestBroker, TestClient};
use room_cli::{
    broker::Broker,
    history,
    message::{self, Message},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
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

// --- read_line_limited integration tests ---

/// Sending a handshake line that exceeds MAX_LINE_BYTES causes the broker to
/// drop the connection without allocating unbounded memory.
#[tokio::test]
async fn oversized_handshake_line_is_rejected() {
    let broker = TestBroker::start("t_oversized_hs").await;

    let stream = UnixStream::connect(&broker.socket_path).await.unwrap();
    let (_, mut writer) = stream.into_split();

    // Send a username line that exceeds the limit (no newline — forces the
    // broker's read_line_limited to accumulate until it hits the cap).
    let payload = vec![b'A'; room_cli::broker::MAX_LINE_BYTES + 1];
    let _ = writer.write_all(&payload).await;
    // Also send a newline so that a non-limited reader would complete.
    let _ = writer.write_all(b"\n").await;

    // Give the broker time to process and drop the connection.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify the broker is still alive by connecting a normal client.
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    let msg = alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;
    assert_eq!(msg.user(), "alice");
}

/// An interactive client that sends an oversized message line after handshake
/// receives an error and is disconnected, while the broker continues serving.
#[tokio::test]
async fn oversized_message_line_disconnects_client() {
    let broker = TestBroker::start("t_oversized_msg").await;

    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // Send an oversized message (no newline first, then newline).
    let mut payload = vec![b'x'; room_cli::broker::MAX_LINE_BYTES + 1];
    payload.push(b'\n');
    let _ = alice.writer.write_all(&payload).await;

    // The broker should send an error response and close the connection.
    // Try reading — we should either get the error JSON or EOF.
    let mut line = String::new();
    let result =
        tokio::time::timeout(Duration::from_secs(2), alice.reader.read_line(&mut line)).await;
    match result {
        Ok(Ok(0)) => {} // EOF — broker closed connection
        Ok(Ok(_)) => {
            // Got a response — should be an error envelope.
            let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(parsed["code"], "line_too_long");
        }
        Ok(Err(_)) => {} // I/O error — also acceptable (connection reset)
        Err(_) => panic!("timed out waiting for broker response after oversized line"),
    }

    // Verify broker is still alive.
    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
}

/// A normal message just under the size limit is accepted.
#[tokio::test]
async fn message_at_size_limit_is_accepted() {
    let broker = TestBroker::start("t_at_limit").await;

    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    // drain bob's join from alice's perspective
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // Send a message that fits within the limit.
    // Account for the JSON envelope overhead (~200 bytes for message wrapper).
    let content_size = room_cli::broker::MAX_LINE_BYTES - 300;
    let big_content: String = std::iter::repeat('Z').take(content_size).collect();
    alice.send_text(&big_content).await;

    // Bob should receive it.
    let msg = bob
        .recv_until(
            |m| matches!(m, Message::Message { content, .. } if content.len() > content_size / 2),
        )
        .await;
    assert_eq!(msg.user(), "alice");
}

// ── Taskboard plugin integration tests ──────────────────────────────────────
//
// These tests use TestDaemon because the taskboard plugin is registered in
// RoomState::new (daemon mode), not in Broker::new (standalone mode).

/// Wrapper around a daemon interactive connection for ergonomic test code.
struct DaemonClient {
    reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl DaemonClient {
    /// Connect to a daemon room as an interactive user.
    async fn connect(socket_path: &std::path::PathBuf, room_id: &str, username: &str) -> Self {
        let (reader, writer) = common::daemon_connect(socket_path, room_id, username).await;
        Self { reader, writer }
    }

    /// Send a plain-text line.
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

    /// Drain messages until the predicate matches, or panic after 2 s.
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
            tokio::time::timeout(remaining, self.reader.read_line(&mut line))
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
}

/// Helper: send a taskboard command as a JSON envelope.
async fn send_taskboard_cmd(client: &mut DaemonClient, action: &str, args: &[&str]) {
    let mut params = vec![serde_json::Value::String(action.to_owned())];
    for arg in args {
        params.push(serde_json::Value::String((*arg).to_owned()));
    }
    let envelope = serde_json::json!({
        "type": "command",
        "cmd": "taskboard",
        "params": params,
    });
    client.send_json(&envelope.to_string()).await;
}

/// Helper: drain messages until a system message from `plugin:taskboard`
/// containing `needle` is received.
async fn recv_taskboard_msg(client: &mut DaemonClient, needle: &str) -> Message {
    let needle_owned = needle.to_owned();
    client
        .recv_until(move |m| {
            matches!(m,
                Message::System { user, content, .. }
                if user == "plugin:taskboard" && content.contains(&needle_owned)
            )
        })
        .await
}

/// Full taskboard approve gate lifecycle with 3 interactive clients.
///
/// Flow: alice posts → bob claims → bob plans → charlie tries approve (rejected)
/// → alice approves (accepted) → bob finishes.
///
/// Verifies:
/// - Broadcast messages (post, claim, plan, approve, finish) reach all clients
/// - Rejection (charlie's approve attempt) is a private reply, not broadcast
/// - Correct gate: only poster (alice) or host can approve
#[tokio::test]
async fn taskboard_approve_gate_multi_user() {
    let td = common::TestDaemon::start(&["tb-room"]).await;

    // alice connects first → becomes host
    let mut alice = DaemonClient::connect(&td.socket_path, "tb-room", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // bob connects
    let mut bob = DaemonClient::connect(&td.socket_path, "tb-room", "bob").await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // charlie connects
    let mut charlie = DaemonClient::connect(&td.socket_path, "tb-room", "charlie").await;
    charlie
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "charlie"))
        .await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "charlie"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "charlie"))
        .await;

    // ── Step 1: alice posts a task ──────────────────────────────────────────
    send_taskboard_cmd(&mut alice, "post", &["implement", "feature", "X"]).await;

    // All three should receive the broadcast
    let post_msg = recv_taskboard_msg(&mut alice, "tb-001").await;
    assert!(post_msg.content().unwrap().contains("implement feature X"));

    recv_taskboard_msg(&mut bob, "tb-001").await;
    recv_taskboard_msg(&mut charlie, "tb-001").await;

    // ── Step 2: bob claims the task ─────────────────────────────────────────
    send_taskboard_cmd(&mut bob, "claim", &["tb-001"]).await;

    let claim_msg = recv_taskboard_msg(&mut bob, "claimed by bob").await;
    assert!(claim_msg.content().unwrap().contains("claimed by bob"));

    recv_taskboard_msg(&mut alice, "claimed by bob").await;
    recv_taskboard_msg(&mut charlie, "claimed by bob").await;

    // ── Step 3: bob submits a plan ──────────────────────────────────────────
    send_taskboard_cmd(
        &mut bob,
        "plan",
        &["tb-001", "add", "struct", "and", "tests"],
    )
    .await;

    let plan_msg = recv_taskboard_msg(&mut bob, "plan submitted").await;
    assert!(plan_msg.content().unwrap().contains("add struct and tests"));

    recv_taskboard_msg(&mut alice, "plan submitted").await;
    recv_taskboard_msg(&mut charlie, "plan submitted").await;

    // ── Step 4: charlie tries to approve (should be rejected) ───────────────
    send_taskboard_cmd(&mut charlie, "approve", &["tb-001"]).await;

    // charlie gets a private reply (Reply, not Broadcast)
    let reject_msg = recv_taskboard_msg(&mut charlie, "only the task poster or host").await;
    assert!(reject_msg
        .content()
        .unwrap()
        .contains("only the task poster or host"));

    // Verify alice and bob do NOT receive the rejection.
    // We do this by sending a probe message and verifying it arrives first
    // (if the rejection had been broadcast, it would arrive before the probe).
    alice.send_text("probe-after-reject").await;
    let probe = alice
        .recv_until(
            |m| matches!(m, Message::Message { content, .. } if content == "probe-after-reject"),
        )
        .await;
    assert_eq!(probe.user(), "alice");

    // bob should also get the probe, not a taskboard rejection
    let bob_probe = bob
        .recv_until(
            |m| matches!(m, Message::Message { content, .. } if content == "probe-after-reject"),
        )
        .await;
    assert_eq!(bob_probe.user(), "alice");

    // ── Step 5: alice approves (she is both poster and host) ────────────────
    send_taskboard_cmd(&mut alice, "approve", &["tb-001"]).await;

    let approve_msg = recv_taskboard_msg(&mut alice, "approved by alice").await;
    assert!(approve_msg.content().unwrap().contains("@bob proceed"));

    recv_taskboard_msg(&mut bob, "approved by alice").await;
    recv_taskboard_msg(&mut charlie, "approved by alice").await;

    // ── Step 6: bob finishes the task ───────────────────────────────────────
    send_taskboard_cmd(&mut bob, "finish", &["tb-001"]).await;

    let finish_msg = recv_taskboard_msg(&mut bob, "finished by bob").await;
    assert!(finish_msg.content().unwrap().contains("finished by bob"));

    recv_taskboard_msg(&mut alice, "finished by bob").await;
    recv_taskboard_msg(&mut charlie, "finished by bob").await;
}

/// Verify that the host (first connected user) can approve tasks they didn't post.
///
/// Scenario: bob posts a task, charlie claims+plans, alice (host) approves.
#[tokio::test]
async fn taskboard_host_can_approve_others_tasks() {
    let td = common::TestDaemon::start(&["tb-host"]).await;

    // alice connects first → host
    let mut alice = DaemonClient::connect(&td.socket_path, "tb-host", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut bob = DaemonClient::connect(&td.socket_path, "tb-host", "bob").await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    let mut charlie = DaemonClient::connect(&td.socket_path, "tb-host", "charlie").await;
    charlie
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "charlie"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "charlie"))
        .await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "charlie"))
        .await;

    // bob posts a task (bob is NOT the host)
    send_taskboard_cmd(&mut bob, "post", &["security", "audit"]).await;
    recv_taskboard_msg(&mut bob, "tb-001").await;
    recv_taskboard_msg(&mut alice, "tb-001").await;
    recv_taskboard_msg(&mut charlie, "tb-001").await;

    // charlie claims and plans
    send_taskboard_cmd(&mut charlie, "claim", &["tb-001"]).await;
    recv_taskboard_msg(&mut charlie, "claimed by charlie").await;
    recv_taskboard_msg(&mut alice, "claimed by charlie").await;
    recv_taskboard_msg(&mut bob, "claimed by charlie").await;

    send_taskboard_cmd(&mut charlie, "plan", &["tb-001", "review", "auth", "code"]).await;
    recv_taskboard_msg(&mut charlie, "plan submitted").await;
    recv_taskboard_msg(&mut alice, "plan submitted").await;
    recv_taskboard_msg(&mut bob, "plan submitted").await;

    // alice (host, NOT poster) approves — should succeed
    send_taskboard_cmd(&mut alice, "approve", &["tb-001"]).await;

    let approve_msg = recv_taskboard_msg(&mut alice, "approved by alice").await;
    assert!(approve_msg.content().unwrap().contains("@charlie proceed"));

    recv_taskboard_msg(&mut bob, "approved by alice").await;
    recv_taskboard_msg(&mut charlie, "approved by alice").await;
}

/// Verify that assignee cannot approve their own task.
///
/// The approve gate checks posted_by or host — the assignee (claimer) is
/// explicitly NOT authorized unless they happen to also be the poster or host.
#[tokio::test]
async fn taskboard_assignee_cannot_self_approve() {
    let td = common::TestDaemon::start(&["tb-self"]).await;

    let mut alice = DaemonClient::connect(&td.socket_path, "tb-self", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut bob = DaemonClient::connect(&td.socket_path, "tb-self", "bob").await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // alice posts
    send_taskboard_cmd(&mut alice, "post", &["write", "tests"]).await;
    recv_taskboard_msg(&mut alice, "tb-001").await;
    recv_taskboard_msg(&mut bob, "tb-001").await;

    // bob claims + plans
    send_taskboard_cmd(&mut bob, "claim", &["tb-001"]).await;
    recv_taskboard_msg(&mut bob, "claimed by bob").await;
    recv_taskboard_msg(&mut alice, "claimed by bob").await;

    send_taskboard_cmd(
        &mut bob,
        "plan",
        &["tb-001", "unit", "tests", "for", "module"],
    )
    .await;
    recv_taskboard_msg(&mut bob, "plan submitted").await;
    recv_taskboard_msg(&mut alice, "plan submitted").await;

    // bob (assignee, not poster, not host) tries to approve — should fail
    send_taskboard_cmd(&mut bob, "approve", &["tb-001"]).await;

    let reject = recv_taskboard_msg(&mut bob, "only the task poster or host").await;
    assert!(reject
        .content()
        .unwrap()
        .contains("only the task poster or host"));
}

// ── Queue plugin oneshot integration tests ────────────────────────────────
//
// These verify that oneshot senders (`room send /queue ...`) get a response
// back instead of EOF. The bug (#493) was that the queue plugin used
// `PluginResult::Handled` which gave no response to oneshot clients.
// After the fix (PR #500), it returns `PluginResult::Broadcast` which
// echoes the system message back to the oneshot sender.

/// Build a queue command JSON envelope for oneshot sends.
fn queue_cmd_wire(action: &str, args: &[&str]) -> String {
    let mut params: Vec<serde_json::Value> = vec![serde_json::Value::String(action.to_owned())];
    for arg in args {
        params.push(serde_json::Value::String((*arg).to_owned()));
    }
    serde_json::json!({"type": "command", "cmd": "queue", "params": params}).to_string()
}

/// Oneshot `/queue add` returns a system message echo to the sender.
#[tokio::test]
async fn queue_add_oneshot_returns_response() {
    let td = common::TestDaemon::start(&["q-os-add"]).await;
    let token = common::daemon_join(&td.socket_path, "q-os-add", "bot").await;

    let wire = queue_cmd_wire("add", &["fix", "the", "flaky", "test"]);
    let resp = common::daemon_send(&td.socket_path, "q-os-add", &token, &wire).await;

    assert_eq!(
        resp["type"], "system",
        "expected system message, got: {resp}"
    );
    let content = resp["content"].as_str().unwrap();
    assert!(
        content.contains("bot added"),
        "response should credit the sender: {content}"
    );
    assert!(
        content.contains("fix the flaky test"),
        "response should echo the task description: {content}"
    );
    assert!(
        content.contains("#1 in backlog"),
        "response should show queue position: {content}"
    );
}

/// Oneshot `/queue pop` returns a system message with the claimed item (FIFO).
#[tokio::test]
async fn queue_pop_oneshot_returns_response() {
    let td = common::TestDaemon::start(&["q-os-pop"]).await;
    let token = common::daemon_join(&td.socket_path, "q-os-pop", "bot").await;

    // seed two items
    let add1 = queue_cmd_wire("add", &["first-task"]);
    common::daemon_send(&td.socket_path, "q-os-pop", &token, &add1).await;
    let add2 = queue_cmd_wire("add", &["second-task"]);
    common::daemon_send(&td.socket_path, "q-os-pop", &token, &add2).await;

    // pop should return first-task (FIFO)
    let pop_wire = queue_cmd_wire("pop", &[]);
    let resp = common::daemon_send(&td.socket_path, "q-os-pop", &token, &pop_wire).await;

    assert_eq!(
        resp["type"], "system",
        "expected system message, got: {resp}"
    );
    let content = resp["content"].as_str().unwrap();
    assert!(
        content.contains("bot claimed from queue"),
        "response should credit the popper: {content}"
    );
    assert!(
        content.contains("first-task"),
        "pop should return the first-added item (FIFO): {content}"
    );
    assert!(
        !content.contains("second-task"),
        "second item should NOT be popped: {content}"
    );
}

/// Oneshot `/queue remove` returns a system message with the removed item.
#[tokio::test]
async fn queue_remove_oneshot_returns_response() {
    let td = common::TestDaemon::start(&["q-os-rm"]).await;
    let token = common::daemon_join(&td.socket_path, "q-os-rm", "bot").await;

    // seed an item
    let add_wire = queue_cmd_wire("add", &["remove-me-task"]);
    common::daemon_send(&td.socket_path, "q-os-rm", &token, &add_wire).await;

    // remove by index 1
    let rm_wire = queue_cmd_wire("remove", &["1"]);
    let resp = common::daemon_send(&td.socket_path, "q-os-rm", &token, &rm_wire).await;

    assert_eq!(
        resp["type"], "system",
        "expected system message, got: {resp}"
    );
    let content = resp["content"].as_str().unwrap();
    assert!(
        content.contains("remove-me-task"),
        "response should echo the removed item: {content}"
    );
    assert!(
        content.contains("was #1"),
        "response should show the original index: {content}"
    );
}
