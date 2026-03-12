/// Room lifecycle tests: UDS CREATE:/DESTROY: protocol and REST POST /api/rooms.
///
/// Covers: dynamic room creation, duplicate/invalid ID errors, private/DM room
/// configuration, room destruction (including with connected clients), and REST
/// room creation endpoint.
mod common;

use std::time::Duration;

use common::{
    daemon_connect, daemon_create, daemon_destroy, daemon_global_join, daemon_join, daemon_send,
    rest_join, TestDaemon,
};
use room_protocol::RoomConfig;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

#[tokio::test]
async fn create_room_via_uds_then_join_and_send() {
    // Start a daemon with no pre-created rooms.
    let td = TestDaemon::start(&[]).await;

    // Get a global token for authentication.
    let admin_token = daemon_global_join(&td.socket_path, "admin").await;

    // Create a room dynamically.
    let resp = daemon_create(
        &td.socket_path,
        "dynamic-room",
        r#"{"visibility":"public"}"#,
        &admin_token,
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
    let token = daemon_global_join(&td.socket_path, "admin").await;

    // Try to create a room that already exists.
    let resp = daemon_create(
        &td.socket_path,
        "existing-room",
        r#"{"visibility":"public"}"#,
        &token,
    )
    .await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "room_exists");
}

#[tokio::test]
async fn create_room_invalid_id_returns_error() {
    let td = TestDaemon::start(&[]).await;
    let token = daemon_global_join(&td.socket_path, "admin").await;

    // Room ID with path traversal.
    let resp = daemon_create(
        &td.socket_path,
        "../escape",
        r#"{"visibility":"public"}"#,
        &token,
    )
    .await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_room_id");

    // Empty room ID.
    let resp2 = daemon_create(&td.socket_path, "", r#"{"visibility":"public"}"#, &token).await;
    assert_eq!(resp2["type"], "error");
    assert_eq!(resp2["code"], "invalid_room_id");
}

#[tokio::test]
async fn create_dm_room_via_uds() {
    let td = TestDaemon::start(&[]).await;
    let token = daemon_global_join(&td.socket_path, "admin").await;

    // Create a DM room with exactly 2 users.
    let resp = daemon_create(
        &td.socket_path,
        "dm-alice-bob",
        r#"{"visibility":"dm","invite":["alice","bob"]}"#,
        &token,
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
    let token = daemon_global_join(&td.socket_path, "admin").await;

    // DM with only 1 user — should fail.
    let resp = daemon_create(
        &td.socket_path,
        "dm-solo",
        r#"{"visibility":"dm","invite":["alice"]}"#,
        &token,
    )
    .await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_config");

    // DM with 3 users — should fail.
    let resp2 = daemon_create(
        &td.socket_path,
        "dm-three",
        r#"{"visibility":"dm","invite":["alice","bob","carol"]}"#,
        &token,
    )
    .await;
    assert_eq!(resp2["type"], "error");
    assert_eq!(resp2["code"], "invalid_config");
}

#[tokio::test]
async fn create_room_default_config_without_token_rejected() {
    let td = TestDaemon::start(&[]).await;

    // Empty config line (no token) — should be rejected.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"CREATE:default-room\n\n").await.unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "missing_token");
}

#[tokio::test]
async fn create_room_invalid_json_returns_error() {
    let td = TestDaemon::start(&[]).await;

    // Send invalid JSON directly — no token injection possible.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"CREATE:bad-config\n").await.unwrap();
    w.write_all(b"not valid json\n").await.unwrap();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_config");
}

#[tokio::test]
async fn create_room_unknown_visibility_returns_error() {
    let td = TestDaemon::start(&[]).await;
    let token = daemon_global_join(&td.socket_path, "admin").await;

    let resp = daemon_create(
        &td.socket_path,
        "weird-room",
        r#"{"visibility":"secret"}"#,
        &token,
    )
    .await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_config");
}

// ── DESTROY: protocol tests ─────────────────────────────────────────────────

#[tokio::test]
async fn destroy_room_removes_from_daemon() {
    let td = TestDaemon::start(&["doomed-room"]).await;

    // Room exists — join works. Also gives us a token for destroy.
    let token = daemon_join(&td.socket_path, "doomed-room", "alice").await;
    assert!(!token.is_empty());

    // Destroy it.
    let resp = daemon_destroy(&td.socket_path, "doomed-room", &token).await;
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
    let token = daemon_global_join(&td.socket_path, "admin").await;

    let resp = daemon_destroy(&td.socket_path, "ghost-room", &token).await;
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
    let resp = daemon_destroy(&td.socket_path, "chat-room", &token).await;
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
    let token = daemon_global_join(&td.socket_path, "admin").await;

    let resp = daemon_destroy(&td.socket_path, "", &token).await;
    assert_eq!(resp["type"], "error");
    assert_eq!(resp["code"], "invalid_room_id");
}

#[tokio::test]
async fn create_then_destroy_then_recreate() {
    let td = TestDaemon::start(&[]).await;
    let admin_token = daemon_global_join(&td.socket_path, "admin").await;

    // Create a room.
    let create_resp = daemon_create(
        &td.socket_path,
        "ephemeral",
        r#"{"visibility":"public"}"#,
        &admin_token,
    )
    .await;
    assert_eq!(create_resp["type"], "room_created");

    // Use it.
    let token = daemon_join(&td.socket_path, "ephemeral", "alice").await;
    assert!(!token.is_empty());

    // Destroy it.
    let destroy_resp = daemon_destroy(&td.socket_path, "ephemeral", &admin_token).await;
    assert_eq!(destroy_resp["type"], "room_destroyed");

    // Recreate it.
    let recreate_resp = daemon_create(
        &td.socket_path,
        "ephemeral",
        r#"{"visibility":"public"}"#,
        &admin_token,
    )
    .await;
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

// ── Destroy with connected clients (P0 gap) ─────────────────────────────────

/// When a room is destroyed, UDS interactive clients should receive EOF
/// (read returns 0 bytes), and subsequent join/send attempts to the
/// destroyed room should return room_not_found.
#[tokio::test]
async fn destroy_room_disconnects_uds_interactive_client() {
    let td = TestDaemon::start(&["live-room"]).await;

    // Connect an interactive client via UDS.
    let (mut reader, _writer) = daemon_connect(&td.socket_path, "live-room", "observer").await;

    // Drain the join broadcast and any history replay.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Get a token for destroy auth.
    let admin_token = daemon_global_join(&td.socket_path, "admin-469").await;

    // Destroy the room while the client is connected.
    let resp = daemon_destroy(&td.socket_path, "live-room", &admin_token).await;
    assert_eq!(resp["type"], "room_destroyed");

    // The interactive client should receive EOF (0 bytes read) within a
    // reasonable timeout — the shutdown signal triggers stream closure.
    let mut buf = String::new();
    let result = timeout(Duration::from_secs(2), reader.read_line(&mut buf)).await;
    match result {
        Ok(Ok(0)) => { /* EOF — expected */ }
        Ok(Ok(_)) => {
            // Got data — could be a final system message before EOF. Drain and
            // expect EOF on the next read.
            buf.clear();
            let eof = timeout(Duration::from_secs(1), reader.read_line(&mut buf)).await;
            match eof {
                Ok(Ok(0)) => { /* EOF after final message */ }
                Ok(Err(_)) => { /* read error after shutdown — acceptable */ }
                Err(_) => panic!("client did not receive EOF after room destroy"),
                Ok(Ok(n)) => panic!("unexpected data ({n} bytes) after destroy: {buf}"),
            }
        }
        Ok(Err(_)) => { /* read error — connection was severed, acceptable */ }
        Err(_) => panic!("timed out — client never received EOF after room destroy"),
    }

    // Subsequent join to the destroyed room should fail.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:live-room:JOIN:latecomer\n")
        .await
        .unwrap();
    let mut reader2 = BufReader::new(r);
    let mut line = String::new();
    reader2.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "room_not_found");
}

/// When a room is destroyed, a token-authenticated send to that room
/// should fail with room_not_found.
#[tokio::test]
async fn destroy_room_rejects_subsequent_token_send() {
    let td = TestDaemon::start(&["send-room"]).await;

    // Get a token before destroy.
    let token = daemon_join(&td.socket_path, "send-room", "alice").await;

    // Verify send works before destroy.
    let msg = daemon_send(&td.socket_path, "send-room", &token, "before").await;
    assert_eq!(msg["type"], "message");

    // Destroy the room (reuse alice's token for auth).
    let resp = daemon_destroy(&td.socket_path, "send-room", &token).await;
    assert_eq!(resp["type"], "room_destroyed");

    // Subsequent send should fail — room no longer exists.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(format!("ROOM:send-room:TOKEN:{token}\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(
        v["type"], "error",
        "send to destroyed room should return error: {v}"
    );
    assert_eq!(v["code"], "room_not_found");
}

/// Multiple interactive clients connected to the same room should all
/// receive EOF when the room is destroyed.
#[tokio::test]
async fn destroy_room_disconnects_multiple_uds_clients() {
    let td = TestDaemon::start(&["multi-client-room"]).await;

    // Connect three interactive clients.
    let (mut r1, _w1) = daemon_connect(&td.socket_path, "multi-client-room", "client-1").await;
    let (mut r2, _w2) = daemon_connect(&td.socket_path, "multi-client-room", "client-2").await;
    let (mut r3, _w3) = daemon_connect(&td.socket_path, "multi-client-room", "client-3").await;

    // Let join broadcasts settle.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Get a token for destroy auth.
    let admin_token = daemon_global_join(&td.socket_path, "admin-multi").await;

    // Destroy the room.
    let resp = daemon_destroy(&td.socket_path, "multi-client-room", &admin_token).await;
    assert_eq!(resp["type"], "room_destroyed");

    // All three clients should eventually get EOF or a read error.
    // We check by trying to read — should return 0 bytes (EOF) or error
    // within the timeout.
    async fn expect_disconnect(
        reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
        label: &str,
    ) {
        let deadline = Duration::from_secs(2);
        loop {
            let mut buf = String::new();
            match timeout(deadline, reader.read_line(&mut buf)).await {
                Ok(Ok(0)) => return,  // EOF
                Ok(Err(_)) => return, // read error (connection reset)
                Err(_) => panic!("{label} did not disconnect after room destroy"),
                Ok(Ok(_)) => continue, // drain residual messages
            }
        }
    }

    // Run all three checks concurrently.
    tokio::join!(
        expect_disconnect(&mut r1, "client-1"),
        expect_disconnect(&mut r2, "client-2"),
        expect_disconnect(&mut r3, "client-3"),
    );
}

// ── Auth rejection tests (#469) ─────────────────────────────────────────────

#[tokio::test]
async fn create_room_without_token_returns_missing_token() {
    let td = TestDaemon::start(&[]).await;

    // Send CREATE without a token in the config.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"CREATE:noauth-room\n{\"visibility\":\"public\"}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "missing_token");
}

#[tokio::test]
async fn create_room_with_invalid_token_returns_invalid_token() {
    let td = TestDaemon::start(&[]).await;

    // Send CREATE with a bogus token.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(
        b"CREATE:bad-token-room\n{\"visibility\":\"public\",\"token\":\"not-a-real-token\"}\n",
    )
    .await
    .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "invalid_token");
}

#[tokio::test]
async fn destroy_room_without_token_returns_missing_token() {
    let td = TestDaemon::start(&["auth-target"]).await;

    // Send DESTROY with an empty second line (no token).
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"DESTROY:auth-target\n\n").await.unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "missing_token");
}

#[tokio::test]
async fn destroy_room_with_invalid_token_returns_invalid_token() {
    let td = TestDaemon::start(&["auth-target2"]).await;

    // Send DESTROY with a bogus token.
    let stream = UnixStream::connect(&td.socket_path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"DESTROY:auth-target2\nnot-a-real-token\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "invalid_token");
}

#[tokio::test]
async fn create_and_destroy_with_valid_token_succeed() {
    let td = TestDaemon::start(&[]).await;
    let token = daemon_global_join(&td.socket_path, "auth-user").await;

    // Create should succeed with valid token.
    let create_resp = daemon_create(
        &td.socket_path,
        "auth-room",
        r#"{"visibility":"public"}"#,
        &token,
    )
    .await;
    assert_eq!(create_resp["type"], "room_created");

    // Destroy should succeed with valid token.
    let destroy_resp = daemon_destroy(&td.socket_path, "auth-room", &token).await;
    assert_eq!(destroy_resp["type"], "room_destroyed");
}
