/// End-to-end smoke tests for the `--ws-port` flag.
///
/// These tests spawn the real `room` binary as a child process (broker mode)
/// and connect via WebSocket and REST from outside the process. This validates
/// the full CLI → broker → transport path that users will exercise.
///
/// Tests are serialized via `SMOKE_LOCK` because spawning 5 broker processes
/// simultaneously causes disk I/O contention on encrypted volumes, preventing
/// any of them from starting within a reasonable timeout.
mod common;

use std::{
    process::Stdio,
    sync::{LazyLock, Mutex},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use room_cli::paths;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message as TungsteniteMsg};

/// Serialize smoke test execution to prevent disk I/O contention when multiple
/// broker processes start simultaneously on encrypted volumes.
static SMOKE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Find the compiled `room` binary. Works with `cargo test`.
fn room_binary() -> std::path::PathBuf {
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    path.push("room");
    assert!(path.exists(), "room binary not found at {}", path.display());
    path
}

/// Spawn a `room` broker process with a unique room ID and WS port.
/// Returns (child, ws_port).
/// The caller must kill the child when done.
async fn spawn_broker(room_id: &str) -> (tokio::process::Child, u16) {
    let port = common::free_port();
    let bin = room_binary();

    let chat_file = format!("/tmp/ws_smoke_{room_id}.chat");
    let token_file = format!("/tmp/ws_smoke_{room_id}.tokens");
    let socket_path = format!("/tmp/room-{room_id}.sock");
    // Also clean up the durable broker token map (moved to ~/.room/state/ in #285).
    let durable_token_map = paths::broker_tokens_path(&paths::room_state_dir(), room_id);
    common::cleanup_stale_files(&[&chat_file, &token_file, &socket_path]);
    let _ = std::fs::remove_file(&durable_token_map);

    let mut child = tokio::process::Command::new(&bin)
        .args([
            room_id,
            "smoke_host",
            "--agent",
            "--ws-port",
            &port.to_string(),
            "-f",
            &chat_file,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn room binary at {}: {e}", bin.display()));

    // Wait for TCP readiness with early crash detection.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!("broker exited with {status} before WS server was ready on port {port}");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "WS server did not start on port {port} within 10 seconds"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    (child, port)
}

// ── REST smoke tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_rest_health() {
    let _guard = SMOKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut child, port) = spawn_broker("smoke_health").await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/api/health"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["room"], "smoke_health");

    child.kill().await.ok();
}

#[tokio::test]
async fn smoke_rest_join_send_poll() {
    let _guard = SMOKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut child, port) = spawn_broker("smoke_rsp").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // JOIN.
    let join_resp = client
        .post(format!("{base}/api/smoke_rsp/join"))
        .json(&serde_json::json!({"username": "smoke_agent"}))
        .send()
        .await
        .unwrap();
    assert_eq!(join_resp.status(), 200);
    let join_body: serde_json::Value = join_resp.json().await.unwrap();
    assert_eq!(join_body["type"], "token");
    let token = join_body["token"].as_str().unwrap().to_owned();

    // SEND.
    let send_resp = client
        .post(format!("{base}/api/smoke_rsp/send"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"content": "smoke test message"}))
        .send()
        .await
        .unwrap();
    assert_eq!(send_resp.status(), 200);
    let send_body: serde_json::Value = send_resp.json().await.unwrap();
    assert_eq!(send_body["type"], "message");
    assert_eq!(send_body["content"], "smoke test message");
    assert_eq!(send_body["user"], "smoke_agent");
    let msg_id = send_body["id"].as_str().unwrap().to_owned();

    // POLL — should return the message.
    let poll_resp = client
        .get(format!("{base}/api/smoke_rsp/poll"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(poll_resp.status(), 200);
    let poll_body: serde_json::Value = poll_resp.json().await.unwrap();
    let messages = poll_body["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m["content"] == "smoke test message"),
        "poll should return the sent message"
    );

    // POLL with since — should return empty.
    let poll2_resp = client
        .get(format!("{base}/api/smoke_rsp/poll?since={msg_id}"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    let poll2_body: serde_json::Value = poll2_resp.json().await.unwrap();
    assert!(
        poll2_body["messages"].as_array().unwrap().is_empty(),
        "poll with since=last should return empty"
    );

    child.kill().await.ok();
}

// ── WebSocket smoke tests ───────────────────────────────────────────────────

#[tokio::test]
async fn smoke_ws_interactive_session() {
    let _guard = SMOKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut child, port) = spawn_broker("smoke_wsi").await;

    let url = format!("ws://127.0.0.1:{port}/ws/smoke_wsi");
    let (ws, _) = connect_async(&url).await.expect("WS connect failed");
    let (mut tx, mut rx) = ws.split();

    // Send username handshake.
    tx.send(TungsteniteMsg::Text("ws_smoker".into()))
        .await
        .unwrap();

    // Should eventually receive join event for ws_smoker (may see smoke_host's join first as history).
    let join = recv_until(&mut rx, |v| v["type"] == "join" && v["user"] == "ws_smoker").await;
    assert_eq!(join["user"], "ws_smoker");

    // Send a message.
    tx.send(TungsteniteMsg::Text("hello from smoke test".into()))
        .await
        .unwrap();

    // Should receive the broadcast back.
    let msg = recv_until_type(&mut rx, "message").await;
    assert_eq!(msg["content"], "hello from smoke test");
    assert_eq!(msg["user"], "ws_smoker");
    assert!(msg["seq"].as_u64().is_some());

    child.kill().await.ok();
}

#[tokio::test]
async fn smoke_ws_oneshot_join_and_token_send() {
    let _guard = SMOKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut child, port) = spawn_broker("smoke_wsos").await;

    // One-shot JOIN via WS.
    let url = format!("ws://127.0.0.1:{port}/ws/smoke_wsos");
    let (ws, _) = connect_async(&url).await.expect("WS connect failed");
    let (mut tx, mut rx) = ws.split();
    tx.send(TungsteniteMsg::Text("JOIN:ws_agent".into()))
        .await
        .unwrap();

    let token_resp = recv_json(&mut rx).await;
    assert_eq!(token_resp["type"], "token");
    assert_eq!(token_resp["username"], "ws_agent");
    let token = token_resp["token"].as_str().unwrap().to_owned();

    // One-shot TOKEN: send via WS.
    let (ws2, _) = connect_async(&url).await.expect("WS connect failed");
    let (mut tx2, mut rx2) = ws2.split();
    tx2.send(TungsteniteMsg::Text(format!("TOKEN:{token}").into()))
        .await
        .unwrap();
    tx2.send(TungsteniteMsg::Text("ws one-shot msg".into()))
        .await
        .unwrap();

    let echo = recv_json(&mut rx2).await;
    assert_eq!(echo["type"], "message");
    assert_eq!(echo["content"], "ws one-shot msg");
    assert_eq!(echo["user"], "ws_agent");

    child.kill().await.ok();
}

#[tokio::test]
async fn smoke_ws_and_rest_cross_path() {
    let _guard = SMOKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut child, port) = spawn_broker("smoke_cross").await;
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();

    // Connect a WS client interactively.
    let url = format!("ws://127.0.0.1:{port}/ws/smoke_cross");
    let (ws, _) = connect_async(&url).await.expect("WS connect failed");
    let (mut _ws_tx, mut ws_rx) = ws.split();

    // Send username handshake.
    _ws_tx
        .send(TungsteniteMsg::Text("ws_watcher".into()))
        .await
        .unwrap();

    // Drain until ws_watcher's own join event.
    recv_until_type(&mut ws_rx, "join").await;

    // Register a REST agent.
    let join_resp = http
        .post(format!("{base}/api/smoke_cross/join"))
        .json(&serde_json::json!({"username": "rest_sender"}))
        .send()
        .await
        .unwrap();
    let join_body: serde_json::Value = join_resp.json().await.unwrap();
    let token = join_body["token"].as_str().unwrap();

    // REST agent sends a message.
    let _send_resp = http
        .post(format!("{base}/api/smoke_cross/send"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"content": "hello from REST"}))
        .send()
        .await
        .unwrap();

    // WS watcher should receive the REST message.
    let msg = recv_until_type(&mut ws_rx, "message").await;
    assert_eq!(msg["content"], "hello from REST");
    assert_eq!(msg["user"], "rest_sender");

    child.kill().await.ok();
}

// ── Helpers ─────────────────────────────────────────────────────────────────

type WsRx = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

/// Read WS frames until the predicate matches a parsed JSON value.
async fn recv_until(rx: &mut WsRx, pred: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    let deadline = Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    loop {
        let remaining = deadline.checked_sub(start.elapsed()).unwrap_or_default();
        if remaining.is_zero() {
            panic!("timed out waiting for matching WS message");
        }
        match timeout(remaining, rx.next()).await {
            Ok(Some(Ok(TungsteniteMsg::Text(text)))) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&*text) {
                    if pred(&v) {
                        return v;
                    }
                }
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(e))) => panic!("WS error: {e}"),
            Ok(None) => panic!("WS stream ended waiting for message"),
            Err(_) => panic!("timed out waiting for matching WS message"),
        }
    }
}

/// Read WS frames until we get a text frame with the specified `type` field.
async fn recv_until_type(rx: &mut WsRx, msg_type: &str) -> serde_json::Value {
    let deadline = Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    loop {
        let remaining = deadline.checked_sub(start.elapsed()).unwrap_or_default();
        if remaining.is_zero() {
            panic!("timed out waiting for message type={msg_type}");
        }
        match timeout(remaining, rx.next()).await {
            Ok(Some(Ok(TungsteniteMsg::Text(text)))) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&*text) {
                    if v["type"] == msg_type {
                        return v;
                    }
                }
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(e))) => panic!("WS error: {e}"),
            Ok(None) => panic!("WS stream ended waiting for type={msg_type}"),
            Err(_) => panic!("timed out waiting for message type={msg_type}"),
        }
    }
}

/// Read the next text frame as JSON.
async fn recv_json(rx: &mut WsRx) -> serde_json::Value {
    match timeout(Duration::from_secs(5), rx.next()).await {
        Ok(Some(Ok(TungsteniteMsg::Text(text)))) => {
            serde_json::from_str(&*text).expect("invalid JSON from WS")
        }
        Ok(Some(Ok(other))) => panic!("unexpected frame: {other:?}"),
        Ok(Some(Err(e))) => panic!("WS error: {e}"),
        Ok(None) => panic!("WS stream ended"),
        Err(_) => panic!("timed out waiting for WS frame"),
    }
}
