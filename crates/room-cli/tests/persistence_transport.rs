//! Integration tests for persistence across daemon restarts and cross-transport
//! message visibility (Section 4+5 of the manual test plan #675).

mod common;

use common::{
    daemon_global_join, daemon_send, rest_join, rest_send, ws_connect, ws_recv_until, TestDaemon,
};
use room_cli::message::Message;
use std::time::Duration;

// ── Section 4: Persistence ──────────────────────────────────────────────────

/// After sending messages, the chat history file should contain them and be
/// loadable by a fresh broker instance using the same data directory.
#[tokio::test]
async fn daemon_restart_preserves_history() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("roomd.sock");

    // Phase 1: start daemon, send messages
    {
        let config = room_cli::broker::daemon::DaemonConfig {
            socket_path: socket_path.clone(),
            data_dir: dir.path().to_owned(),
            state_dir: dir.path().to_owned(),
            ws_port: None,
            grace_period_secs: 0, // shut down immediately when connections close
        };
        let state = std::sync::Arc::new(room_cli::broker::daemon::DaemonState::new(config));
        state.create_room("persist-test").await.unwrap();

        let state_run = state.clone();
        let handle = tokio::spawn(async move { state_run.run().await.ok() });

        common::wait_for_socket(&socket_path, Duration::from_secs(1)).await;

        let token = daemon_global_join(&socket_path, "alice").await;
        daemon_send(&socket_path, "persist-test", &token, "message-one").await;
        daemon_send(&socket_path, "persist-test", &token, "message-two").await;

        // Shut down daemon
        state.shutdown_handle();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    // Verify chat file exists and contains messages
    let chat_path = dir.path().join("persist-test.chat");
    assert!(chat_path.exists(), "chat file should exist after shutdown");

    let history = room_cli::history::load(&chat_path).await.unwrap();
    assert!(
        history.iter().any(|m| m.content() == Some("message-one")),
        "history should contain message-one"
    );
    assert!(
        history.iter().any(|m| m.content() == Some("message-two")),
        "history should contain message-two"
    );
}

/// Tokens issued by the daemon should survive a restart — the token file is
/// persisted to disk alongside the chat file.
#[tokio::test]
async fn daemon_restart_preserves_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("roomd.sock");

    let token: String;

    // Phase 1: start daemon, join, get token
    {
        let config = room_cli::broker::daemon::DaemonConfig {
            socket_path: socket_path.clone(),
            data_dir: dir.path().to_owned(),
            state_dir: dir.path().to_owned(),
            ws_port: None,
            grace_period_secs: 0,
        };
        let state = std::sync::Arc::new(room_cli::broker::daemon::DaemonState::new(config));
        state.create_room("token-test").await.unwrap();

        let state_run = state.clone();
        let handle = tokio::spawn(async move { state_run.run().await.ok() });
        common::wait_for_socket(&socket_path, Duration::from_secs(1)).await;

        token = daemon_global_join(&socket_path, "bob").await;
        assert!(!token.is_empty(), "should receive a token");

        state.shutdown_handle();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    // Verify users.json exists
    let users_path = dir.path().join("users.json");
    assert!(
        users_path.exists(),
        "users.json should be persisted to disk"
    );

    // Phase 2: restart daemon with same directories
    {
        // Remove socket so we can rebind
        let _ = std::fs::remove_file(&socket_path);

        let config = room_cli::broker::daemon::DaemonConfig {
            socket_path: socket_path.clone(),
            data_dir: dir.path().to_owned(),
            state_dir: dir.path().to_owned(),
            ws_port: None,
            grace_period_secs: 0,
        };
        let state = std::sync::Arc::new(room_cli::broker::daemon::DaemonState::new(config));
        state.create_room("token-test").await.unwrap();

        let state_run = state.clone();
        let handle = tokio::spawn(async move { state_run.run().await.ok() });
        common::wait_for_socket(&socket_path, Duration::from_secs(1)).await;

        // The old token should still work for sending
        let result = daemon_send(&socket_path, "token-test", &token, "after-restart").await;
        assert_eq!(
            result["type"], "message",
            "token should still be valid after restart: {result}"
        );

        state.shutdown_handle();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }
}

/// The users.json registry persists subscription state — after joining and
/// interacting with a room, the registry file records the user's presence.
#[tokio::test]
async fn daemon_restart_preserves_user_registry() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("roomd.sock");

    // Phase 1: start daemon, join, interact
    {
        let config = room_cli::broker::daemon::DaemonConfig {
            socket_path: socket_path.clone(),
            data_dir: dir.path().to_owned(),
            state_dir: dir.path().to_owned(),
            ws_port: None,
            grace_period_secs: 0,
        };
        let state = std::sync::Arc::new(room_cli::broker::daemon::DaemonState::new(config));
        state.create_room("sub-test").await.unwrap();

        let state_run = state.clone();
        let handle = tokio::spawn(async move { state_run.run().await.ok() });
        common::wait_for_socket(&socket_path, Duration::from_secs(1)).await;

        let token = daemon_global_join(&socket_path, "carol").await;
        daemon_send(&socket_path, "sub-test", &token, "hello").await;

        state.shutdown_handle();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    // Verify users.json contains the registered user
    let users_path = dir.path().join("users.json");
    assert!(users_path.exists(), "users.json should be persisted");
    let content = std::fs::read_to_string(&users_path).unwrap();
    assert!(
        content.contains("carol"),
        "users.json should contain the registered user 'carol'"
    );
}

// ── Section 5: Cross-Transport Visibility ────────────────────────────────────

/// A message sent via REST should be visible to a WebSocket client connected
/// to the same room.
#[tokio::test]
async fn rest_message_visible_to_ws_client() {
    let (daemon, port) = TestDaemon::start_with_ws_configs(vec![("xport", None)]).await;
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();

    // REST client joins first (to keep daemon alive)
    let rest_token = rest_join(&http, &base, "xport", "rest-sender").await;

    // WS client joins
    let (_, mut ws_rx) = ws_connect(port, "xport", "ws-viewer").await;
    // Drain join messages (own join + possibly rest-sender's)
    ws_recv_until(&mut ws_rx, |m| {
        matches!(m, Message::Join { .. }) && m.user() == "ws-viewer"
    })
    .await;

    // REST client sends a message
    rest_send(&http, &base, "xport", &rest_token, "cross-transport hello").await;

    // WS client should receive the REST-sent message
    let msg = ws_recv_until(&mut ws_rx, |m| m.content() == Some("cross-transport hello")).await;
    assert_eq!(msg.user(), "rest-sender");

    daemon.state.shutdown_handle();
}

/// A DM sent via WebSocket should NOT be visible to a non-participant WS client.
#[tokio::test]
async fn ws_dm_not_visible_to_non_participant() {
    let (daemon, port) = TestDaemon::start_with_ws_configs(vec![("dm-test", None)]).await;

    // Three WS clients join
    let (mut tx_alice, mut rx_alice) = ws_connect(port, "dm-test", "alice").await;
    ws_recv_until(&mut rx_alice, |m| matches!(m, Message::Join { .. })).await;

    let (_tx_bob, mut rx_bob) = ws_connect(port, "dm-test", "bob").await;
    ws_recv_until(&mut rx_bob, |m| {
        matches!(m, Message::Join { .. }) && m.user() == "bob"
    })
    .await;

    let (_tx_eve, mut rx_eve) = ws_connect(port, "dm-test", "eve").await;
    ws_recv_until(&mut rx_eve, |m| {
        matches!(m, Message::Join { .. }) && m.user() == "eve"
    })
    .await;

    // Alice sends a DM to Bob
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMsg;
    let dm_json = serde_json::json!({
        "type": "dm",
        "to": "bob",
        "content": "secret for bob only"
    });
    tx_alice
        .send(WsMsg::Text(dm_json.to_string().into()))
        .await
        .unwrap();

    // Bob should receive the DM
    let bob_msg = ws_recv_until(&mut rx_bob, |m| m.content() == Some("secret for bob only")).await;
    assert_eq!(bob_msg.user(), "alice");

    // Eve should NOT receive it — wait briefly then verify no matching message
    let eve_result = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            let v = common::ws_recv_json(&mut rx_eve).await;
            if v.get("content").and_then(|c| c.as_str()) == Some("secret for bob only") {
                return true; // Eve saw the DM — test should fail
            }
        }
    })
    .await;

    assert!(
        eve_result.is_err(),
        "eve must NOT receive alice->bob DM (timeout expected)"
    );

    daemon.state.shutdown_handle();
}
