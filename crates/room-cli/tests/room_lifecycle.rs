/// Room lifecycle tests: UDS CREATE:/DESTROY: protocol and REST POST /api/rooms.
///
/// Covers: dynamic room creation, duplicate/invalid ID errors, private/DM room
/// configuration, room destruction, and REST room creation endpoint.
mod common;

use common::{daemon_create, daemon_destroy, daemon_join, daemon_send, rest_join, TestDaemon};
use room_protocol::RoomConfig;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[tokio::test]
async fn create_room_via_uds_then_join_and_send() {
    // Start a daemon with no pre-created rooms.
    let td = TestDaemon::start(&[]).await;

    // Create a room dynamically.
    let resp = daemon_create(
        &td.socket_path,
        "dynamic-room",
        r#"{"visibility":"public"}"#,
    )
    .await;
    assert_eq!(resp["type"], "room_created");
    assert_eq!(resp["room"], "dynamic-room");

    // Join the newly created room.
    let token = daemon_join(&td.socket_path, "dynamic-room", "alice").await;
    assert!(!token.is_empty());

    // Send a message to it.
    let msg = daemon_send(&td.socket_path, "dynamic-room", &token, "hello dynamic").await;
    assert_eq!(msg["type"], "message");
    assert_eq!(msg["content"], "hello dynamic");
    assert_eq!(msg["user"], "alice");
}

#[tokio::test]
async fn create_room_duplicate_returns_error() {
    let td = TestDaemon::start(&["existing-room"]).await;

    // Try to create a room that already exists.
    let resp = daemon_create(
        &td.socket_path,
        "existing-room",
        r#"{"visibility":"public"}"#,
    )
    .await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "room_exists");
}

#[tokio::test]
async fn create_room_invalid_id_returns_error() {
    let td = TestDaemon::start(&[]).await;

    // Room ID with path traversal.
    let resp = daemon_create(&td.socket_path, "../escape", r#"{"visibility":"public"}"#).await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_room_id");

    // Empty room ID.
    let resp2 = daemon_create(&td.socket_path, "", r#"{"visibility":"public"}"#).await;
    assert_eq!(resp2["type"], "error");
    assert_eq!(resp2["code"], "invalid_room_id");
}

#[tokio::test]
async fn create_dm_room_via_uds() {
    let td = TestDaemon::start(&[]).await;

    // Create a DM room with exactly 2 users.
    let resp = daemon_create(
        &td.socket_path,
        "dm-alice-bob",
        r#"{"visibility":"dm","invite":["alice","bob"]}"#,
    )
    .await;
    assert_eq!(resp["type"], "room_created");
    assert_eq!(resp["room"], "dm-alice-bob");

    // alice can join.
    let token = daemon_join(&td.socket_path, "dm-alice-bob", "alice").await;
    assert!(!token.is_empty());

    // eve cannot join (not in invite list).
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:dm-alice-bob:JOIN:eve\n").await.unwrap();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error", "eve should be rejected: {v}");
    assert_eq!(v["code"], "join_denied");
}

#[tokio::test]
async fn create_dm_room_wrong_invite_count_returns_error() {
    let td = TestDaemon::start(&[]).await;

    // DM with only 1 user — should fail.
    let resp = daemon_create(
        &td.socket_path,
        "dm-solo",
        r#"{"visibility":"dm","invite":["alice"]}"#,
    )
    .await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_config");

    // DM with 3 users — should fail.
    let resp2 = daemon_create(
        &td.socket_path,
        "dm-three",
        r#"{"visibility":"dm","invite":["alice","bob","carol"]}"#,
    )
    .await;
    assert_eq!(resp2["type"], "error");
    assert_eq!(resp2["code"], "invalid_config");
}

#[tokio::test]
async fn create_room_default_config() {
    let td = TestDaemon::start(&[]).await;

    // Empty config line — should default to public.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"CREATE:default-room\n\n").await.unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "room_created");
    assert_eq!(v["room"], "default-room");

    // Should be joinable (public).
    let token = daemon_join(&td.socket_path, "default-room", "user1").await;
    assert!(!token.is_empty());
}

#[tokio::test]
async fn create_room_invalid_json_returns_error() {
    let td = TestDaemon::start(&[]).await;

    let resp = daemon_create(&td.socket_path, "bad-config", "not valid json").await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_config");
}

#[tokio::test]
async fn create_room_unknown_visibility_returns_error() {
    let td = TestDaemon::start(&[]).await;

    let resp = daemon_create(&td.socket_path, "weird-room", r#"{"visibility":"secret"}"#).await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_config");
}

// ── DESTROY: protocol tests ─────────────────────────────────────────────────

#[tokio::test]
async fn destroy_room_removes_from_daemon() {
    let td = TestDaemon::start(&["doomed-room"]).await;

    // Room exists — join works.
    let token = daemon_join(&td.socket_path, "doomed-room", "alice").await;
    assert!(!token.is_empty());

    // Destroy it.
    let resp = daemon_destroy(&td.socket_path, "doomed-room").await;
    assert_eq!(resp["type"], "room_destroyed");
    assert_eq!(resp["room"], "doomed-room");

    // Room is gone — join should fail with room_not_found.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:doomed-room:JOIN:bob\n").await.unwrap();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "room_not_found");
}

#[tokio::test]
async fn destroy_nonexistent_room_returns_error() {
    let td = TestDaemon::start(&[]).await;

    let resp = daemon_destroy(&td.socket_path, "ghost-room").await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "room_not_found");
}

#[tokio::test]
async fn destroy_room_preserves_chat_file() {
    let td = TestDaemon::start(&["chat-room"]).await;

    // Join and send a message so the chat file has content.
    let token = daemon_join(&td.socket_path, "chat-room", "alice").await;
    daemon_send(&td.socket_path, "chat-room", &token, "hello before destroy").await;

    // Verify chat file exists (data_dir is the TempDir root in tests).
    let chat_path = td._dir.path().join("chat-room.chat");
    assert!(chat_path.exists(), "chat file should exist before destroy");

    // Destroy the room.
    let resp = daemon_destroy(&td.socket_path, "chat-room").await;
    assert_eq!(resp["type"], "room_destroyed");

    // Chat file should still exist.
    assert!(
        chat_path.exists(),
        "chat file should be preserved after destroy"
    );
    let content = std::fs::read_to_string(&chat_path).unwrap();
    assert!(
        content.contains("hello before destroy"),
        "chat file should still contain messages"
    );
}

#[tokio::test]
async fn destroy_empty_room_id_returns_error() {
    let td = TestDaemon::start(&[]).await;

    let resp = daemon_destroy(&td.socket_path, "").await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_room_id");
}

#[tokio::test]
async fn create_then_destroy_then_recreate() {
    let td = TestDaemon::start(&[]).await;

    // Create a room.
    let create_resp =
        daemon_create(&td.socket_path, "ephemeral", r#"{"visibility":"public"}"#).await;
    assert_eq!(create_resp["type"], "room_created");

    // Use it.
    let token = daemon_join(&td.socket_path, "ephemeral", "alice").await;
    assert!(!token.is_empty());

    // Destroy it.
    let destroy_resp = daemon_destroy(&td.socket_path, "ephemeral").await;
    assert_eq!(destroy_resp["type"], "room_destroyed");

    // Recreate it.
    let recreate_resp =
        daemon_create(&td.socket_path, "ephemeral", r#"{"visibility":"public"}"#).await;
    assert_eq!(recreate_resp["type"], "room_created");

    // Can join again (token is system-level, should still work).
    let token2 = daemon_join(&td.socket_path, "ephemeral", "bob").await;
    assert!(!token2.is_empty());
}

// ── REST POST /api/rooms tests ───────────────────────────────────────────────

#[tokio::test]
async fn rest_create_room_without_token_returns_401() {
    let (_td, port) = TestDaemon::start_with_ws_configs(vec![]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/api/rooms"))
        .json(&serde_json::json!({"room_id": "newroom"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "missing_token");
}

#[tokio::test]
async fn rest_create_room_with_invalid_token_returns_401() {
    let (_td, port) = TestDaemon::start_with_ws_configs(vec![]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/api/rooms"))
        .header("Authorization", "Bearer not-a-real-token")
        .json(&serde_json::json!({"room_id": "newroom"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_token");
}

#[tokio::test]
async fn rest_create_room_returns_201() {
    let (td, port) = TestDaemon::start_with_ws_configs(vec![("seed-room", None)]).await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Get a valid token by joining an existing room.
    let token = rest_join(&client, &base, "seed-room", "alice_cr").await;

    let resp = client
        .post(format!("{base}/api/rooms"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"room_id": "brand-new-room"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "room_created");
    assert_eq!(body["room"], "brand-new-room");

    // Room should now be accessible — join it.
    let token2 = rest_join(&client, &base, "brand-new-room", "bob_cr").await;
    assert!(!token2.is_empty());
    drop(td);
}

#[tokio::test]
async fn rest_create_room_duplicate_returns_409() {
    let (td, port) = TestDaemon::start_with_ws_configs(vec![("existing-room-409", None)]).await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "existing-room-409", "alice_dup").await;

    let resp = client
        .post(format!("{base}/api/rooms"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"room_id": "existing-room-409"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "room_exists");
    drop(td);
}

#[tokio::test]
async fn rest_create_room_invalid_id_returns_400() {
    let (td, port) = TestDaemon::start_with_ws_configs(vec![("seed400", None)]).await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "seed400", "alice_inv").await;

    let resp = client
        .post(format!("{base}/api/rooms"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"room_id": "../escape"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_room_id");
    drop(td);
}

#[tokio::test]
async fn rest_create_room_with_private_visibility() {
    let (td, port) = TestDaemon::start_with_ws_configs(vec![("seed-priv", None)]).await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // alice_priv_creator gets a token from the seed room for auth.
    let token = rest_join(&client, &base, "seed-priv", "alice_priv_creator").await;

    let resp = client
        .post(format!("{base}/api/rooms"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"room_id": "private-room", "visibility": "private", "invite": ["bob_priv_invited"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "room_created");

    // bob_priv_invited (in invite list) can join the private room.
    let tok2 = rest_join(&client, &base, "private-room", "bob_priv_invited").await;
    assert!(!tok2.is_empty());
    drop(td);
}

#[tokio::test]
async fn rest_create_dm_room_returns_201() {
    let (td, port) = TestDaemon::start_with_ws_configs(vec![("seed-dm", None)]).await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Use a separate creator user to get the auth token from the seed room.
    let token = rest_join(&client, &base, "seed-dm", "dm_creator").await;

    let resp = client
        .post(format!("{base}/api/rooms"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"room_id": "dm-alice-bob-rest", "visibility": "dm", "invite": ["alice_dm_u", "bob_dm_u"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "room_created");

    // bob_dm_u (in invite list) can join the DM room.
    let tok2 = rest_join(&client, &base, "dm-alice-bob-rest", "bob_dm_u").await;
    assert!(!tok2.is_empty());
    drop(td);
}

#[tokio::test]
async fn rest_create_dm_room_wrong_invite_count_returns_400() {
    let (td, port) = TestDaemon::start_with_ws_configs(vec![("seed-dm2", None)]).await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "seed-dm2", "alice_dm2").await;

    let resp = client
        .post(format!("{base}/api/rooms"))
        .header("Authorization", format!("Bearer {token}"))
        .json(
            &serde_json::json!({"room_id": "dm-bad", "visibility": "dm", "invite": ["alice_dm2"]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_config");
    drop(td);
}
