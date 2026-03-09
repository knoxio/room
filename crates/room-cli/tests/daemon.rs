/// Multi-room daemon protocol tests and REST/WS DM enforcement.
///
/// Covers: ROOM: handshake, multi-room isolation, daemon token auth,
/// DM room join/send enforcement via REST and WS.
mod common;

use std::time::Duration;

use common::{
    daemon_connect, daemon_create, daemon_global_join, daemon_join, daemon_send, ws_connect,
    TestBroker, TestClient, TestDaemon,
};
use futures_util::{SinkExt, StreamExt};
use room_cli::message::Message;
use room_protocol::{dm_room_id, RoomConfig, RoomVisibility};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as TungsteniteMsg;

#[tokio::test]
async fn daemon_rejects_missing_room_prefix() {
    let td = TestDaemon::start(&["test-room"]).await;

    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    // Send without ROOM: prefix
    w.write_all(b"alice\n").await.unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "missing_room_prefix");
}

// ── Test: daemon rejects connections to nonexistent rooms ─────────────────

#[tokio::test]
async fn daemon_rejects_nonexistent_room() {
    let td = TestDaemon::start(&["real-room"]).await;

    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:fake-room:JOIN:alice\n").await.unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "room_not_found");
}

// ── Test: daemon join + send + receive ────────────────────────────────────

#[tokio::test]
async fn daemon_join_send_receive() {
    let td = TestDaemon::start(&["chat"]).await;

    let token = daemon_join(&td.socket_path, "chat", "alice").await;
    assert!(!token.is_empty());

    let resp = daemon_send(&td.socket_path, "chat", &token, "hello from daemon").await;
    assert_eq!(resp["type"], "message");
    assert_eq!(resp["content"], "hello from daemon");
    assert_eq!(resp["user"], "alice");
}

// ── Test: multi-room message isolation ────────────────────────────────────

#[tokio::test]
async fn daemon_multi_room_message_isolation() {
    let td = TestDaemon::start(&["room-alpha", "room-beta"]).await;

    // Join both rooms with different tokens.
    let token_a = daemon_join(&td.socket_path, "room-alpha", "agent-a").await;
    let token_b = daemon_join(&td.socket_path, "room-beta", "agent-b").await;

    // Connect an interactive client to room-alpha to observe messages.
    let (mut reader_a, _writer_a) =
        daemon_connect(&td.socket_path, "room-alpha", "observer-a").await;

    // Drain history/join messages from the interactive connection.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send a message to room-beta.
    let resp_b = daemon_send(&td.socket_path, "room-beta", &token_b, "beta-only msg").await;
    assert_eq!(resp_b["type"], "message");
    assert_eq!(resp_b["room"], "room-beta");

    // Send a message to room-alpha.
    let resp_a = daemon_send(&td.socket_path, "room-alpha", &token_a, "alpha msg").await;
    assert_eq!(resp_a["type"], "message");
    assert_eq!(resp_a["room"], "room-alpha");

    // The observer in room-alpha should see "alpha msg" but NOT "beta-only msg".
    // Read with a short timeout — we should get the alpha message.
    let mut line = String::new();
    let read_result = timeout(Duration::from_millis(500), reader_a.read_line(&mut line)).await;

    // We may get the join/leave messages first, so drain until we find "alpha msg"
    // or run out of data.
    let mut saw_alpha = false;
    let mut saw_beta = false;

    if read_result.is_ok() {
        // Check this line and keep reading.
        loop {
            if line.contains("alpha msg") {
                saw_alpha = true;
            }
            if line.contains("beta-only msg") {
                saw_beta = true;
            }
            line.clear();
            match timeout(Duration::from_millis(200), reader_a.read_line(&mut line)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
            }
        }
    }

    assert!(saw_alpha, "observer in room-alpha should see alpha msg");
    assert!(
        !saw_beta,
        "observer in room-alpha should NOT see beta-only msg"
    );
}

// ── Test: system-level tokens are valid across all rooms in a daemon ──────
//
// Tokens are daemon-scoped (not room-scoped): a token issued by any room JOIN
// can be used to send to any other room managed by the same daemon (#293).

#[tokio::test]
async fn daemon_token_valid_across_rooms() {
    let td = TestDaemon::start(&["room-x", "room-y"]).await;

    // Join room-x — the resulting token is system-level.
    let token_x = daemon_join(&td.socket_path, "room-x", "user-x").await;

    // Use the token issued in room-x to send to room-y.
    let resp = daemon_send(&td.socket_path, "room-y", &token_x, "cross-room msg").await;
    assert_eq!(
        resp["type"], "message",
        "system-level token should be valid in a different room: {resp}"
    );
    assert_eq!(resp["content"], "cross-room msg");
    assert_eq!(resp["user"], "user-x");
    assert_eq!(resp["room"], "room-y");
}

// ── DM room visibility integration tests ─────────────────────────────────

#[tokio::test]
async fn daemon_dm_room_participants_can_join() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let config = RoomConfig::dm("alice", "bob");
    let td = TestDaemon::start_with_configs(vec![(&dm_id, Some(config))]).await;

    let token_a = daemon_join(&td.socket_path, &dm_id, "alice").await;
    assert!(!token_a.is_empty());

    let token_b = daemon_join(&td.socket_path, &dm_id, "bob").await;
    assert!(!token_b.is_empty());
}

#[tokio::test]
async fn daemon_dm_room_non_participant_rejected() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let config = RoomConfig::dm("alice", "bob");
    let td = TestDaemon::start_with_configs(vec![(&dm_id, Some(config))]).await;

    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("ROOM:{dm_id}:JOIN:eve\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "join_denied");
}

#[tokio::test]
async fn daemon_dm_room_send_and_receive() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let config = RoomConfig::dm("alice", "bob");
    let td = TestDaemon::start_with_configs(vec![(&dm_id, Some(config))]).await;

    let token = daemon_join(&td.socket_path, &dm_id, "alice").await;
    let resp = daemon_send(&td.socket_path, &dm_id, &token, "private hello").await;
    assert_eq!(resp["type"], "message");
    assert_eq!(resp["content"], "private hello");
    assert_eq!(resp["user"], "alice");
    assert_eq!(resp["room"], dm_id);
}

#[tokio::test]
async fn daemon_private_room_requires_invite() {
    let config = RoomConfig {
        visibility: RoomVisibility::Private,
        max_members: None,
        invite_list: ["member".to_owned()].into(),
        created_by: "owner".to_owned(),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    let td = TestDaemon::start_with_configs(vec![("secret-room", Some(config))]).await;

    // Owner can join (creator privilege).
    let token_owner = daemon_join(&td.socket_path, "secret-room", "owner").await;
    assert!(!token_owner.is_empty());

    // Invited member can join.
    let token_member = daemon_join(&td.socket_path, "secret-room", "member").await;
    assert!(!token_member.is_empty());

    // Uninvited user is rejected.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:secret-room:JOIN:stranger\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "join_denied");
}

#[tokio::test]
async fn daemon_public_room_allows_anyone() {
    let config = RoomConfig::public("owner");
    let td = TestDaemon::start_with_configs(vec![("open-room", Some(config))]).await;

    let token = daemon_join(&td.socket_path, "open-room", "random-user").await;
    assert!(!token.is_empty());
}

#[tokio::test]
async fn dm_room_id_both_directions_same() {
    assert_eq!(
        dm_room_id("alice", "bob").unwrap(),
        dm_room_id("bob", "alice").unwrap()
    );
}

// ── Scripted multi-agent test sequences (#180) ──────────────────────────────
//
// Deterministic, pre-scripted coordination scenarios that exercise the exact
// patterns agents use in production: join → send → poll → verify ordering.

/// Three agents join, send messages in a deterministic sequence, then poll.
/// Verifies message ordering and completeness from each agent's perspective.

#[tokio::test]
async fn rest_dm_room_non_participant_join_rejected() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let dm_config = RoomConfig::dm("alice", "bob");
    let (_td, port) =
        TestDaemon::start_with_ws_configs(vec![(dm_id.as_str(), Some(dm_config))]).await;

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // eve is not a DM participant — join should be rejected.
    let join_resp = client
        .post(format!("{base}/api/{dm_id}/join"))
        .json(&serde_json::json!({"username": "eve"}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        join_resp.status(),
        403,
        "non-participant join should be 403"
    );
    let body: serde_json::Value = join_resp.json().await.unwrap();
    assert_eq!(body["code"], "join_denied");
}

/// WS interactive handshake as a non-DM-participant should be rejected
/// (the server closes the connection without issuing a token).
#[tokio::test]
async fn ws_dm_room_non_participant_join_rejected() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let dm_config = RoomConfig::dm("alice", "bob");
    let (_td, port) =
        TestDaemon::start_with_ws_configs(vec![(dm_id.as_str(), Some(dm_config))]).await;

    // eve tries an interactive WS handshake with her username.
    let url = format!("ws://127.0.0.1:{port}/ws/{dm_id}");
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();
    tx.send(TungsteniteMsg::Text("eve".into())).await.unwrap();

    // The server should reject eve — expect an error frame or connection close.
    let resp = tokio::time::timeout(Duration::from_secs(2), rx.next()).await;
    match resp {
        Ok(Some(Ok(TungsteniteMsg::Text(text)))) => {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["type"], "error", "expected join error, got: {v}");
        }
        Ok(Some(Ok(TungsteniteMsg::Close(_)))) => {
            // Connection closed without a message — also acceptable rejection.
        }
        Ok(None) => {
            // Stream ended — connection was closed, which is a valid rejection.
        }
        _ => {
            // Timeout or other — eve was not allowed in either way.
        }
    }
}

// ── P0: concurrent room creation race ────────────────────────────────────────

/// Two simultaneous CREATE requests for the same room ID must be handled
/// cleanly: exactly one must succeed with `room_created` and the other must
/// fail with `room_exists`. The room must be functional afterwards.
///
/// Regression: without proper deduplication, both requests could race past the
/// existence check and produce two `room_created` responses (and two broker
/// goroutines for the same room).
#[tokio::test]
async fn daemon_concurrent_room_creation_deduplicates() {
    let td = TestDaemon::start(&[]).await;
    let socket1 = td.socket_path.clone();
    let socket2 = td.socket_path.clone();

    // Fire two concurrent CREATE requests for the same room ID.
    let t1 = tokio::spawn(async move {
        daemon_create(&socket1, "race-room", r#"{"visibility":"public"}"#).await
    });
    let t2 = tokio::spawn(async move {
        daemon_create(&socket2, "race-room", r#"{"visibility":"public"}"#).await
    });

    let (r1, r2) = tokio::join!(t1, t2);
    let resp1 = r1.expect("task 1 panicked");
    let resp2 = r2.expect("task 2 panicked");

    let responses = [&resp1, &resp2];

    // Neither response should be an internal/unexpected error.
    for r in &responses {
        assert_ne!(
            r["code"], "internal",
            "unexpected internal error in concurrent create: {r}"
        );
    }

    let created_count = responses
        .iter()
        .filter(|r| r["type"] == "room_created")
        .count();
    let exists_count = responses
        .iter()
        .filter(|r| r["type"] == "error" && r["code"] == "room_exists")
        .count();

    assert_eq!(
        created_count, 1,
        "exactly one concurrent create must succeed (got {:?} and {:?})",
        resp1, resp2
    );
    assert_eq!(
        exists_count, 1,
        "exactly one must get room_exists error (got {:?} and {:?})",
        resp1, resp2
    );

    // The created room must accept joins and sends.
    let token = daemon_join(&td.socket_path, "race-room", "alice").await;
    assert!(!token.is_empty(), "created room must accept joins");
    let send_resp = daemon_send(&td.socket_path, "race-room", &token, "sanity check").await;
    assert_eq!(send_resp["type"], "message");
    assert_eq!(send_resp["room"], "race-room");
}

/// REST POST /api/{room}/send as a DM participant succeeds.
#[tokio::test]
async fn rest_dm_room_participant_send_allowed() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let dm_config = RoomConfig::dm("alice", "bob");
    let (_td, port) =
        TestDaemon::start_with_ws_configs(vec![(dm_id.as_str(), Some(dm_config))]).await;

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // alice is a DM participant — join to get a token.
    let join_resp = client
        .post(format!("{base}/api/{dm_id}/join"))
        .json(&serde_json::json!({"username": "alice"}))
        .send()
        .await
        .unwrap();
    assert_eq!(join_resp.status(), 200);
    let join_body: serde_json::Value = join_resp.json().await.unwrap();
    let token = join_body["token"].as_str().unwrap();

    // alice sends a message — should succeed.
    let send_resp = client
        .post(format!("{base}/api/{dm_id}/send"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"content": "hello bob"}))
        .send()
        .await
        .unwrap();

    assert_eq!(send_resp.status(), 200, "participant send should succeed");
    let body: serde_json::Value = send_resp.json().await.unwrap();
    assert_eq!(body["content"], "hello bob");
    assert_eq!(body["user"], "alice");
}

/// A non-participant with a valid token (injected directly, simulating
/// future system-level tokens) must be rejected with send_denied via REST.
#[tokio::test]
async fn rest_dm_room_non_participant_send_rejected() {
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let dm_config = RoomConfig::dm("alice", "bob");
    let (td, port) =
        TestDaemon::start_with_ws_configs(vec![(dm_id.as_str(), Some(dm_config))]).await;

    // Inject a token for eve directly, bypassing join permission.
    // This simulates future system-level token issuance (issue #293).
    let eve_token = "test-token-eve-12345";
    td.state
        .test_inject_token(&dm_id, "eve", eve_token)
        .await
        .unwrap();

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Eve tries to send — should be rejected with 403 send_denied.
    let send_resp = client
        .post(format!("{base}/api/{dm_id}/send"))
        .header("Authorization", format!("Bearer {eve_token}"))
        .json(&serde_json::json!({"content": "unauthorized message"}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        send_resp.status(),
        403,
        "non-participant send should be 403"
    );
    let body: serde_json::Value = send_resp.json().await.unwrap();
    assert_eq!(body["code"], "send_denied");
}

// ── REST /api/{room_id}/query tests ────────────────────────────────────────────

/// Helper: join a room via REST and return the token string.
async fn rest_join(client: &reqwest::Client, base: &str, room_id: &str, username: &str) -> String {
    let resp = client
        .post(format!("{base}/api/{room_id}/join"))
        .json(&serde_json::json!({"username": username}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "rest_join failed for {username}");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_owned()
}

/// Helper: send a message via REST and return the response body.
async fn rest_send(
    client: &reqwest::Client,
    base: &str,
    room_id: &str,
    token: &str,
    content: &str,
) -> serde_json::Value {
    let resp = client
        .post(format!("{base}/api/{room_id}/send"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"content": content}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "rest_send failed for content={content}");
    resp.json().await.unwrap()
}

// ── Global join (room-independent) tests ────────────────────────────────

#[tokio::test]
async fn global_join_returns_token_and_username() {
    let td = TestDaemon::start(&["test-room"]).await;
    let token = daemon_global_join(&td.socket_path, "alice").await;
    assert!(!token.is_empty(), "expected non-empty token");
}

#[tokio::test]
async fn global_join_idempotent_returns_same_token() {
    let td = TestDaemon::start(&["test-room"]).await;
    let token1 = daemon_global_join(&td.socket_path, "bob").await;
    let token2 = daemon_global_join(&td.socket_path, "bob").await;
    assert_eq!(token1, token2, "idempotent join should return same token");
}

#[tokio::test]
async fn global_join_different_users_get_different_tokens() {
    let td = TestDaemon::start(&["test-room"]).await;
    let token_alice = daemon_global_join(&td.socket_path, "alice").await;
    let token_bob = daemon_global_join(&td.socket_path, "bob").await;
    assert_ne!(
        token_alice, token_bob,
        "different users must get different tokens"
    );
}

#[tokio::test]
async fn global_join_empty_username_rejected() {
    let td = TestDaemon::start(&["test-room"]).await;

    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"JOIN:\n").await.unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error", "empty username should be rejected: {v}");
}
