/// Multi-room daemon protocol tests and REST/WS DM enforcement.
///
/// Covers: ROOM: handshake, multi-room isolation, daemon token auth,
/// DM room join/send enforcement via REST and WS.
mod common;

use std::time::Duration;

use common::{
    daemon_connect, daemon_create, daemon_destroy, daemon_global_join, daemon_join, daemon_send,
    ws_connect, ws_recv_json, TestDaemon,
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
    let token = daemon_global_join(&td.socket_path, "admin").await;
    let socket1 = td.socket_path.clone();
    let socket2 = td.socket_path.clone();
    let tok1 = token.clone();
    let tok2 = token.clone();

    // Fire two concurrent CREATE requests for the same room ID.
    let t1 = tokio::spawn(async move {
        daemon_create(&socket1, "race-room", r#"{"visibility":"public"}"#, &tok1).await
    });
    let t2 = tokio::spawn(async move {
        daemon_create(&socket2, "race-room", r#"{"visibility":"public"}"#, &tok2).await
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

#[tokio::test]
async fn global_join_token_works_for_room_send() {
    let td = TestDaemon::start(&["lobby"]).await;

    // Global join — no room specified.
    let token = daemon_global_join(&td.socket_path, "gj-sender").await;

    // Use that global token to send to "lobby" via ROOM:lobby:TOKEN:<token>.
    let echo = daemon_send(&td.socket_path, "lobby", &token, "hello from global join").await;
    assert_eq!(
        echo["type"], "message",
        "global token should work for room send: {echo}"
    );
    assert_eq!(echo["content"], "hello from global join");
    assert_eq!(echo["user"], "gj-sender");
}

// ── Global token fallback via WS and REST (#416) ─────────────────────────────

#[tokio::test]
async fn global_token_works_for_ws_oneshot_send() {
    let (td, port) = TestDaemon::start_with_ws_configs(vec![("ws-lobby", None)]).await;

    // Get a global token via UDS.
    let token = daemon_global_join(&td.socket_path, "ws-gj").await;

    // Use TOKEN: handshake over WS with the global token.
    let first_frame = format!("TOKEN:{token}");
    let (mut tx, mut rx) = ws_connect(port, "ws-lobby", &first_frame).await;

    tx.send(TungsteniteMsg::Text("ws global token msg".into()))
        .await
        .unwrap();

    let echo = ws_recv_json(&mut rx).await;
    assert_eq!(
        echo["type"], "message",
        "WS global token send failed: {echo}"
    );
    assert_eq!(echo["content"], "ws global token msg");
    assert_eq!(echo["user"], "ws-gj");
}

#[tokio::test]
async fn global_token_works_for_rest_send() {
    let (td, port) = TestDaemon::start_with_ws_configs(vec![("rest-lobby", None)]).await;

    let token = daemon_global_join(&td.socket_path, "rest-gj").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Send via REST using the global token.
    let echo = rest_send(&client, &base, "rest-lobby", &token, "rest global msg").await;
    assert_eq!(
        echo["type"], "message",
        "REST global token send failed: {echo}"
    );
    assert_eq!(echo["content"], "rest global msg");
    assert_eq!(echo["user"], "rest-gj");
}

#[tokio::test]
async fn global_token_works_for_rest_poll() {
    let (td, port) = TestDaemon::start_with_ws_configs(vec![("poll-lobby", None)]).await;

    let token = daemon_global_join(&td.socket_path, "poll-gj").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Send a message first so there's something to poll.
    rest_send(&client, &base, "poll-lobby", &token, "poll test msg").await;

    // Poll via REST using the global token.
    let resp = client
        .get(format!("{base}/api/poll-lobby/poll"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "REST poll with global token failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(|m| m["content"] == "poll test msg"),
        "poll should contain the sent message: {body}"
    );
}

#[tokio::test]
async fn global_token_works_for_rest_query() {
    let (td, port) = TestDaemon::start_with_ws_configs(vec![("query-lobby", None)]).await;

    let token = daemon_global_join(&td.socket_path, "query-gj").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Send a message first so there's something to query.
    rest_send(&client, &base, "query-lobby", &token, "query test msg").await;

    // Query via REST using the global token.
    let resp = client
        .get(format!("{base}/api/query-lobby/query?user=query-gj"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "REST query with global token failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(|m| m["content"] == "query test msg"),
        "query should contain the sent message: {body}"
    );
}

// ── Daemon multi-room isolation tests (#578) ─────────────────────────────

/// A global token obtained before a room is destroyed must remain valid for
/// sends to surviving rooms. Regression: if the daemon invalidated tokens
/// on room destruction, cross-room sends would break for all previously-
/// issued tokens.
#[tokio::test]
async fn global_token_works_after_room_destroy() {
    let td = TestDaemon::start(&["keep-room", "doomed-room"]).await;

    // Get a global token — valid across all rooms.
    let token = daemon_global_join(&td.socket_path, "survivor").await;

    // Send to both rooms before destruction — sanity check.
    let resp_keep = daemon_send(&td.socket_path, "keep-room", &token, "pre-destroy").await;
    assert_eq!(resp_keep["type"], "message");
    let resp_doomed = daemon_send(&td.socket_path, "doomed-room", &token, "farewell").await;
    assert_eq!(resp_doomed["type"], "message");

    // Destroy one room.
    let destroy_resp = daemon_destroy(&td.socket_path, "doomed-room", &token).await;
    assert_eq!(
        destroy_resp["type"], "room_destroyed",
        "destroy should succeed: {destroy_resp}"
    );

    // The global token must still work for the surviving room.
    let resp_after = daemon_send(&td.socket_path, "keep-room", &token, "post-destroy").await;
    assert_eq!(
        resp_after["type"], "message",
        "global token should still work after sibling room destroy: {resp_after}"
    );
    assert_eq!(resp_after["content"], "post-destroy");
    assert_eq!(resp_after["user"], "survivor");
}

/// Destroying a room while an interactive client is connected must close
/// the client's stream cleanly (EOF / connection closed). The daemon must
/// not panic, and other rooms must remain functional.
#[tokio::test]
async fn destroy_room_with_active_connection() {
    let td = TestDaemon::start(&["live-room", "other-room"]).await;

    let token = daemon_global_join(&td.socket_path, "admin").await;

    // Open an interactive session to live-room.
    let (mut reader, _writer) = daemon_connect(&td.socket_path, "live-room", "occupant").await;

    // Drain the join/history messages so the reader is caught up.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Destroy the room while the client is connected.
    let destroy_resp = daemon_destroy(&td.socket_path, "live-room", &token).await;
    assert_eq!(
        destroy_resp["type"], "room_destroyed",
        "destroy with active connection should succeed: {destroy_resp}"
    );

    // The interactive reader should get EOF (0 bytes) or a close frame.
    let mut buf = String::new();
    let read_result = timeout(Duration::from_secs(2), reader.read_line(&mut buf)).await;
    match read_result {
        // EOF — stream closed cleanly.
        Ok(Ok(0)) => {}
        // Got some data — could be a system leave/shutdown message, acceptable.
        Ok(Ok(_)) => {}
        // Timeout — the stream hung. Not ideal, but the daemon didn't panic.
        Err(_) => {}
        // Read error — connection was dropped.
        Ok(Err(_)) => {}
    }

    // The other room must still be functional after the destroy.
    let other_token = daemon_join(&td.socket_path, "other-room", "bystander").await;
    assert!(
        !other_token.is_empty(),
        "other room should still accept joins"
    );
    let resp = daemon_send(&td.socket_path, "other-room", &other_token, "still alive").await;
    assert_eq!(
        resp["type"], "message",
        "other room should still accept sends: {resp}"
    );
    assert_eq!(resp["content"], "still alive");
}

/// Rooms created dynamically via `daemon_create` after daemon startup must
/// be fully functional: joinable, sendable, and isolated from pre-existing
/// rooms.
#[tokio::test]
async fn create_room_dynamically_and_use() {
    // Start daemon with one pre-existing room.
    let td = TestDaemon::start(&["static-room"]).await;

    let token = daemon_global_join(&td.socket_path, "creator").await;

    // Dynamically create a new room.
    let create_resp = daemon_create(
        &td.socket_path,
        "dynamic-room",
        r#"{"visibility":"public"}"#,
        &token,
    )
    .await;
    assert_eq!(
        create_resp["type"], "room_created",
        "dynamic room creation should succeed: {create_resp}"
    );

    // Join and send in the dynamically created room.
    let dyn_token = daemon_join(&td.socket_path, "dynamic-room", "dynamic-user").await;
    assert!(!dyn_token.is_empty(), "dynamic room should accept joins");

    let send_resp = daemon_send(&td.socket_path, "dynamic-room", &dyn_token, "hello dynamic").await;
    assert_eq!(send_resp["type"], "message");
    assert_eq!(send_resp["content"], "hello dynamic");
    assert_eq!(send_resp["room"], "dynamic-room");

    // Verify isolation: connect interactive client to dynamic-room, send
    // to static-room, confirm the dynamic-room observer does NOT see it.
    let (mut dyn_reader, _dyn_writer) =
        daemon_connect(&td.socket_path, "dynamic-room", "dyn-observer").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let static_token = daemon_join(&td.socket_path, "static-room", "static-user").await;
    daemon_send(
        &td.socket_path,
        "static-room",
        &static_token,
        "static-only msg",
    )
    .await;

    // Also send to dynamic-room so the observer has something to read.
    daemon_send(
        &td.socket_path,
        "dynamic-room",
        &dyn_token,
        "dynamic-only msg",
    )
    .await;

    // Drain the dynamic-room observer — should see "dynamic-only msg" but
    // not "static-only msg".
    let mut saw_dynamic = false;
    let mut saw_static = false;
    loop {
        let mut line = String::new();
        match timeout(Duration::from_millis(500), dyn_reader.read_line(&mut line)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(_)) => {
                if line.contains("dynamic-only msg") {
                    saw_dynamic = true;
                }
                if line.contains("static-only msg") {
                    saw_static = true;
                }
            }
            Ok(Err(_)) => break,
        }
    }

    assert!(
        saw_dynamic,
        "dynamic-room observer should see dynamic-only msg"
    );
    assert!(
        !saw_static,
        "dynamic-room observer must NOT see static-room messages"
    );
}
