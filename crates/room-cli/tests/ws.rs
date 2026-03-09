/// WebSocket protocol and single-room REST API tests.
///
/// Covers: WS interactive session, oneshot WS send/join, token auth over WS,
/// cross-transport message delivery, REST join/send/poll lifecycle, REST DM.
mod common;

use std::time::Duration;

use common::{ws_connect, ws_recv_json, ws_recv_until, TestBroker, TestClient};
use futures_util::SinkExt;
use room_cli::{history, message::Message};
use tokio_tungstenite::{connect_async, tungstenite::Message as TungsteniteMsg};

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
