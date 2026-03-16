/// Section 2 CLI workflow tests from manual test plan (#675).
///
/// Tests verify core CLI workflows at the daemon protocol level:
/// send+poll cycle, interactive message delivery, multi-room isolation,
/// and pull via history file.
mod common;

use std::time::Duration;

use common::{daemon_connect, daemon_join, daemon_send, TestDaemon};
use room_cli::message::Message;
use tokio::io::AsyncBufReadExt;
use tokio::time::timeout;

/// Read all available JSON lines from a reader within a timeout,
/// returning parsed Message values. Stops on timeout (no error).
async fn drain_messages(
    reader: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    dur: Duration,
) -> Vec<Message> {
    let mut msgs = Vec::new();
    let deadline = tokio::time::Instant::now() + dur;
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            break;
        }
        let mut line = String::new();
        match timeout(remaining, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(_)) => {
                if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                    msgs.push(msg);
                }
            }
            _ => break,
        }
    }
    msgs
}

// ── 2.1 Send+Poll Cycle ────────────────────────────────────────────────────

/// One-shot send returns a broadcast echo with correct fields.
///
/// Manual test plan 2.1: "room send — JSON echo with correct fields".
#[tokio::test]
async fn send_returns_echo_with_correct_fields() {
    let daemon = TestDaemon::start(&["t-send-echo"]).await;

    let token = daemon_join(&daemon.socket_path, "t-send-echo", "sender").await;
    let echo = daemon_send(
        &daemon.socket_path,
        "t-send-echo",
        &token,
        r#"{"type":"message","content":"hello"}"#,
    )
    .await;

    assert_eq!(echo["type"], "message");
    assert_eq!(echo["user"], "sender");
    assert_eq!(echo["content"], "hello");
    assert!(echo["id"].as_str().is_some(), "echo should have an id");
    assert!(
        echo["ts"].as_str().is_some(),
        "echo should have a timestamp"
    );
    assert_eq!(echo["room"], "t-send-echo");
}

/// Interactive client sees one-shot messages in real time.
///
/// Manual test plan 2.1: "room poll — message appears".
#[tokio::test]
async fn interactive_client_sees_oneshot_messages() {
    let daemon = TestDaemon::start(&["t-poll-see"]).await;

    let (mut r_alice, _w_alice) = daemon_connect(&daemon.socket_path, "t-poll-see", "alice").await;
    drain_messages(&mut r_alice, Duration::from_millis(200)).await;

    let token_bob = daemon_join(&daemon.socket_path, "t-poll-see", "bob").await;
    daemon_send(
        &daemon.socket_path,
        "t-poll-see",
        &token_bob,
        r#"{"type":"message","content":"bob says hi"}"#,
    )
    .await;

    let msgs = drain_messages(&mut r_alice, Duration::from_millis(500)).await;
    let found = msgs
        .iter()
        .any(|m| matches!(m, Message::Message { content, .. } if content == "bob says hi"));
    assert!(found, "alice should see bob's message");
}

/// Multiple one-shot sends are all visible to an interactive watcher.
///
/// Manual test plan 2.1: "send + poll — only new message returned".
#[tokio::test]
async fn multiple_sends_all_visible() {
    let daemon = TestDaemon::start(&["t-multi-send"]).await;

    let (mut r_watcher, _w_watcher) =
        daemon_connect(&daemon.socket_path, "t-multi-send", "watcher").await;
    drain_messages(&mut r_watcher, Duration::from_millis(200)).await;

    let token = daemon_join(&daemon.socket_path, "t-multi-send", "sender").await;
    for i in 1..=3 {
        daemon_send(
            &daemon.socket_path,
            "t-multi-send",
            &token,
            &format!(r#"{{"type":"message","content":"msg-{i}"}}"#),
        )
        .await;
    }

    let msgs = drain_messages(&mut r_watcher, Duration::from_millis(500)).await;
    let contents: Vec<String> = msgs
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(contents.contains(&"msg-1".to_owned()));
    assert!(contents.contains(&"msg-2".to_owned()));
    assert!(contents.contains(&"msg-3".to_owned()));
}

// ── 2.2 Watch Mode ──────────────────────────────────────────────────────────

/// Interactive session blocks until a foreign message arrives.
///
/// Manual test plan 2.2: "watch blocks until message arrives" and
/// "message from another user — watch exits with it".
#[tokio::test]
async fn interactive_receives_delayed_foreign_message() {
    let daemon = TestDaemon::start(&["t-watch"]).await;

    let (mut r_alice, _w_alice) = daemon_connect(&daemon.socket_path, "t-watch", "alice").await;
    drain_messages(&mut r_alice, Duration::from_millis(200)).await;

    let token_bob = daemon_join(&daemon.socket_path, "t-watch", "bob").await;

    // Spawn bob's send after a delay — alice should be "blocking" until this arrives.
    let socket = daemon.socket_path.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        daemon_send(
            &socket,
            "t-watch",
            &token_bob,
            r#"{"type":"message","content":"delayed hello"}"#,
        )
        .await;
    });

    // Alice waits for the delayed message.
    let mut found = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            break;
        }
        let mut line = String::new();
        match timeout(remaining, r_alice.read_line(&mut line)).await {
            Ok(Ok(n)) if n > 0 => {
                if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                    if matches!(&msg, Message::Message { content, .. } if content == "delayed hello")
                    {
                        found = true;
                        break;
                    }
                }
            }
            _ => break,
        }
    }
    assert!(found, "alice should receive bob's delayed message");
}

// ── 2.5 Multi-Room Isolation ────────────────────────────────────────────────

/// Messages sent to different rooms are isolated — each room only sees its own.
///
/// Manual test plan 2.5: multi-room poll verifies per-room message delivery.
#[tokio::test]
async fn multi_room_messages_are_isolated() {
    let daemon = TestDaemon::start(&["t-mr-a", "t-mr-b"]).await;

    let (mut r_a, _w_a) = daemon_connect(&daemon.socket_path, "t-mr-a", "listener-a").await;
    let (mut r_b, _w_b) = daemon_connect(&daemon.socket_path, "t-mr-b", "listener-b").await;
    drain_messages(&mut r_a, Duration::from_millis(200)).await;
    drain_messages(&mut r_b, Duration::from_millis(200)).await;

    let token_a = daemon_join(&daemon.socket_path, "t-mr-a", "sender-a").await;
    daemon_send(
        &daemon.socket_path,
        "t-mr-a",
        &token_a,
        r#"{"type":"message","content":"for room-a"}"#,
    )
    .await;

    let token_b = daemon_join(&daemon.socket_path, "t-mr-b", "sender-b").await;
    daemon_send(
        &daemon.socket_path,
        "t-mr-b",
        &token_b,
        r#"{"type":"message","content":"for room-b"}"#,
    )
    .await;

    let msgs_a = drain_messages(&mut r_a, Duration::from_millis(500)).await;
    let content_a: Vec<String> = msgs_a
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(content_a.contains(&"for room-a".to_owned()));
    assert!(!content_a.contains(&"for room-b".to_owned()));

    let msgs_b = drain_messages(&mut r_b, Duration::from_millis(500)).await;
    let content_b: Vec<String> = msgs_b
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(content_b.contains(&"for room-b".to_owned()));
    assert!(!content_b.contains(&"for room-a".to_owned()));
}
