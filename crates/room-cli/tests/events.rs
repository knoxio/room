/// Integration tests for the Event message variant (#430).
///
/// Verifies that taskboard actions emit typed Event messages alongside System
/// broadcasts, and that events flow correctly through all 3 transports:
/// UDS interactive, WebSocket, and REST poll.
mod common;

use std::time::Duration;

use room_cli::message::Message;
use room_protocol::EventType;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Wrapper around a daemon interactive connection (mirrors broker.rs DaemonClient).
struct DaemonClient {
    reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl DaemonClient {
    async fn connect(socket_path: &std::path::PathBuf, room_id: &str, username: &str) -> Self {
        let (reader, writer) = common::daemon_connect(socket_path, room_id, username).await;
        Self { reader, writer }
    }

    async fn send_json(&mut self, json: &str) {
        self.writer
            .write_all(format!("{json}\n").as_bytes())
            .await
            .unwrap();
    }

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

/// Send a taskboard command as a JSON envelope.
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

/// Drain messages until an Event with the given EventType is received.
async fn recv_event(client: &mut DaemonClient, expected_type: EventType) -> Message {
    client
        .recv_until(
            move |m| matches!(m, Message::Event { event_type, .. } if *event_type == expected_type),
        )
        .await
}

/// Drain messages until a System message from `plugin:taskboard` containing
/// `needle` is received.
async fn recv_taskboard_system(client: &mut DaemonClient, needle: &str) -> Message {
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

// ── UDS tests ────────────────────────────────────────────────────────────────

/// Taskboard post emits a TaskPosted event alongside the system broadcast.
///
/// Note: emit_event runs before PluginResult::Broadcast returns, so the
/// Event arrives before the System message on the wire.
#[tokio::test]
async fn taskboard_post_emits_event_uds() {
    let td = common::TestDaemon::start(&["ev-post"]).await;

    let mut alice = DaemonClient::connect(&td.socket_path, "ev-post", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    send_taskboard_cmd(&mut alice, "post", &["test", "task"]).await;

    // Event arrives first (emitted before Broadcast return)
    let evt = recv_event(&mut alice, EventType::TaskPosted).await;
    assert!(evt.content().unwrap().contains("test task"));
    assert_eq!(evt.user(), "plugin:taskboard");
    assert!(evt.seq().is_some(), "event should have a sequence number");

    // System broadcast arrives after
    let sys = recv_taskboard_system(&mut alice, "tb-001").await;
    assert!(sys.content().unwrap().contains("test task"));
}

/// Full lifecycle emits correct EventType at each stage.
#[tokio::test]
async fn taskboard_lifecycle_emits_all_event_types() {
    let td = common::TestDaemon::start(&["ev-life"]).await;

    let mut alice = DaemonClient::connect(&td.socket_path, "ev-life", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut bob = DaemonClient::connect(&td.socket_path, "ev-life", "bob").await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // post → TaskPosted
    send_taskboard_cmd(&mut alice, "post", &["lifecycle", "task"]).await;
    recv_event(&mut alice, EventType::TaskPosted).await;
    recv_event(&mut bob, EventType::TaskPosted).await;

    // claim → TaskClaimed
    send_taskboard_cmd(&mut bob, "claim", &["tb-001"]).await;
    recv_event(&mut alice, EventType::TaskClaimed).await;
    recv_event(&mut bob, EventType::TaskClaimed).await;

    // plan → TaskPlanned
    send_taskboard_cmd(&mut bob, "plan", &["tb-001", "do", "the", "thing"]).await;
    recv_event(&mut alice, EventType::TaskPlanned).await;
    recv_event(&mut bob, EventType::TaskPlanned).await;

    // approve → TaskApproved
    send_taskboard_cmd(&mut alice, "approve", &["tb-001"]).await;
    recv_event(&mut alice, EventType::TaskApproved).await;
    recv_event(&mut bob, EventType::TaskApproved).await;

    // finish → TaskFinished
    send_taskboard_cmd(&mut bob, "finish", &["tb-001"]).await;
    recv_event(&mut alice, EventType::TaskFinished).await;
    recv_event(&mut bob, EventType::TaskFinished).await;
}

/// Events are broadcast to all connected clients (not just the actor).
#[tokio::test]
async fn events_broadcast_to_all_clients() {
    let td = common::TestDaemon::start(&["ev-bc"]).await;

    let mut alice = DaemonClient::connect(&td.socket_path, "ev-bc", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut bob = DaemonClient::connect(&td.socket_path, "ev-bc", "bob").await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    let mut charlie = DaemonClient::connect(&td.socket_path, "ev-bc", "charlie").await;
    charlie
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "charlie"))
        .await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "charlie"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "charlie"))
        .await;

    send_taskboard_cmd(&mut alice, "post", &["broadcast", "test"]).await;

    // All three clients should receive the TaskPosted event
    let evt_alice = recv_event(&mut alice, EventType::TaskPosted).await;
    let evt_bob = recv_event(&mut bob, EventType::TaskPosted).await;
    let evt_charlie = recv_event(&mut charlie, EventType::TaskPosted).await;

    // All should have the same content
    assert_eq!(evt_alice.content(), evt_bob.content());
    assert_eq!(evt_bob.content(), evt_charlie.content());

    // All should have the same seq
    assert_eq!(evt_alice.seq(), evt_bob.seq());
    assert_eq!(evt_bob.seq(), evt_charlie.seq());
}

/// Events are persisted to the chat file and appear in history.
#[tokio::test]
async fn events_persisted_to_chat_history() {
    let td = common::TestDaemon::start(&["ev-hist"]).await;

    let mut alice = DaemonClient::connect(&td.socket_path, "ev-hist", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    send_taskboard_cmd(&mut alice, "post", &["persist", "check"]).await;
    recv_event(&mut alice, EventType::TaskPosted).await;

    // Load the chat file and verify the Event is in history
    let chat_path = td._dir.path().join("ev-hist.chat");
    let messages = room_cli::history::load(&chat_path).await.unwrap();

    let events: Vec<&Message> = messages
        .iter()
        .filter(|m| matches!(m, Message::Event { .. }))
        .collect();

    assert!(
        !events.is_empty(),
        "at least one Event should be persisted to chat history"
    );

    let event = events[0];
    assert_eq!(event.user(), "plugin:taskboard");
    assert!(event.content().unwrap().contains("persist check"));

    if let Message::Event { event_type, .. } = event {
        assert_eq!(*event_type, EventType::TaskPosted);
    } else {
        panic!("expected Event variant");
    }
}

/// Event sequence numbers are monotonically increasing and interleave
/// correctly with system messages.
///
/// Since emit_event fires before PluginResult::Broadcast, the Event has a
/// LOWER seq than the corresponding System message.
#[tokio::test]
async fn event_seq_numbers_are_monotonic() {
    let td = common::TestDaemon::start(&["ev-seq"]).await;

    let mut alice = DaemonClient::connect(&td.socket_path, "ev-seq", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    send_taskboard_cmd(&mut alice, "post", &["seq", "test"]).await;

    // Event arrives first (lower seq), then system message (higher seq)
    let evt = recv_event(&mut alice, EventType::TaskPosted).await;
    let sys = recv_taskboard_system(&mut alice, "tb-001").await;

    let evt_seq = evt.seq().expect("event should have seq");
    let sys_seq = sys.seq().expect("system msg should have seq");

    assert!(
        evt_seq < sys_seq,
        "event seq ({evt_seq}) should be < system seq ({sys_seq}) since event is emitted first"
    );
}

/// Assign action emits TaskAssigned (not TaskClaimed).
#[tokio::test]
async fn assign_emits_task_assigned_event() {
    let td = common::TestDaemon::start(&["ev-assign"]).await;

    let mut alice = DaemonClient::connect(&td.socket_path, "ev-assign", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut bob = DaemonClient::connect(&td.socket_path, "ev-assign", "bob").await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // Post a task, then assign to bob
    send_taskboard_cmd(&mut alice, "post", &["assign", "test"]).await;
    recv_event(&mut alice, EventType::TaskPosted).await;
    recv_event(&mut bob, EventType::TaskPosted).await;

    send_taskboard_cmd(&mut alice, "assign", &["tb-001", "bob"]).await;

    // Should emit TaskAssigned, not TaskClaimed
    let evt = recv_event(&mut alice, EventType::TaskAssigned).await;
    assert!(evt.content().unwrap().contains("bob"));
    recv_event(&mut bob, EventType::TaskAssigned).await;
}

/// Cancel action emits TaskCancelled event.
#[tokio::test]
async fn cancel_emits_task_cancelled_event() {
    let td = common::TestDaemon::start(&["ev-cancel"]).await;

    let mut alice = DaemonClient::connect(&td.socket_path, "ev-cancel", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    send_taskboard_cmd(&mut alice, "post", &["cancel", "test"]).await;
    recv_event(&mut alice, EventType::TaskPosted).await;

    send_taskboard_cmd(&mut alice, "cancel", &["tb-001", "changed", "mind"]).await;
    let evt = recv_event(&mut alice, EventType::TaskCancelled).await;
    assert!(evt.content().unwrap().contains("cancelled"));
}

// ── WebSocket transport tests ────────────────────────────────────────────────

/// Events flow through WebSocket transport to connected clients.
#[tokio::test]
async fn events_visible_over_websocket() {
    let (td, port) = common::TestDaemon::start_with_ws_configs(vec![("ev-ws", None)]).await;

    // UDS client triggers the event
    let mut alice = DaemonClient::connect(&td.socket_path, "ev-ws", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // WS client observes it
    let (_ws_tx, mut ws_rx) = common::ws_connect(port, "ev-ws", "bob").await;
    // Drain until bob's join is received
    common::ws_recv_until(
        &mut ws_rx,
        |m| matches!(m, Message::Join { user, .. } if user == "bob"),
    )
    .await;
    // Alice also sees bob's join
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // Alice posts a task via UDS
    send_taskboard_cmd(&mut alice, "post", &["ws", "event", "test"]).await;

    // WS client should receive the TaskPosted event
    let ws_evt = common::ws_recv_until(
        &mut ws_rx,
        |m| matches!(m, Message::Event { event_type, .. } if *event_type == EventType::TaskPosted),
    )
    .await;

    assert_eq!(ws_evt.user(), "plugin:taskboard");
    assert!(ws_evt.content().unwrap().contains("ws event test"));
}

// ── REST poll transport tests ────────────────────────────────────────────────

/// Events appear in REST poll results.
#[tokio::test]
async fn events_visible_in_rest_poll() {
    let (td, port) = common::TestDaemon::start_with_ws_configs(vec![("ev-rest", None)]).await;

    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();

    // UDS client creates an event first
    let mut alice = DaemonClient::connect(&td.socket_path, "ev-rest", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    send_taskboard_cmd(&mut alice, "post", &["rest", "poll", "test"]).await;
    recv_event(&mut alice, EventType::TaskPosted).await;

    // Join via REST to get a token (REST join is stateless — no Join broadcast)
    let token = common::rest_join(&http, &base, "ev-rest", "poller").await;

    // Poll via REST — should include the event in history
    let resp = http
        .get(format!("{base}/api/ev-rest/poll"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"]
        .as_array()
        .expect("messages should be array");

    let events: Vec<&serde_json::Value> = messages
        .iter()
        .filter(|m| m["type"].as_str() == Some("event"))
        .collect();

    assert!(
        !events.is_empty(),
        "REST poll should include at least one event"
    );

    let event = &events[0];
    assert_eq!(event["event_type"].as_str(), Some("task_posted"));
    assert!(event["content"]
        .as_str()
        .unwrap()
        .contains("rest poll test"));
}

/// Event JSON wire format has correct structure when received via REST.
#[tokio::test]
async fn event_wire_format_is_correct() {
    let (td, port) = common::TestDaemon::start_with_ws_configs(vec![("ev-wire", None)]).await;

    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();

    // Create an event first via UDS
    let mut alice = DaemonClient::connect(&td.socket_path, "ev-wire", "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    send_taskboard_cmd(&mut alice, "post", &["wire", "format"]).await;
    recv_event(&mut alice, EventType::TaskPosted).await;

    // Join via REST (stateless — no Join broadcast) and poll
    let token = common::rest_join(&http, &base, "ev-wire", "observer").await;

    let resp = http
        .get(format!("{base}/api/ev-wire/poll"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();

    let event = messages
        .iter()
        .find(|m| m["type"].as_str() == Some("event"))
        .expect("should find an event in poll results");

    // Verify required fields
    assert!(event["id"].is_string(), "event must have id");
    assert_eq!(event["room"].as_str(), Some("ev-wire"));
    assert_eq!(event["user"].as_str(), Some("plugin:taskboard"));
    assert!(event["ts"].is_string(), "event must have timestamp");
    assert!(event["seq"].is_number(), "event must have seq");
    assert_eq!(event["event_type"].as_str(), Some("task_posted"));
    assert!(event["content"].is_string(), "event must have content");
    assert_eq!(event["type"].as_str(), Some("event"));
}
