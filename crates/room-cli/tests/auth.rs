/// Token authentication, admin command, and subscription persistence tests.
///
/// Covers: join_session token issuance, token reuse, token invalidation,
/// admin kick/reauth/clear, admin exit, permission enforcement,
/// and subscription persistence across join and restart (#438).
mod common;

use std::time::Duration;

use common::{TestBroker, TestClient};
use room_cli::{
    history,
    message::{self, Message},
    oneshot,
};
use room_protocol::RoomConfig;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

// ── Token auth tests ──────────────────────────────────────────────────────────

/// `join_session` returns a non-empty token and the correct username on success.
#[tokio::test]
async fn join_session_returns_token() {
    let broker = TestBroker::start("t_join_token").await;
    let (username, token) = room_cli::oneshot::join_session(&broker.socket_path, "agent1")
        .await
        .expect("join_session failed");
    assert_eq!(username, "agent1");
    assert!(!token.is_empty(), "token must be non-empty");
}

/// A second `join_session` for the same username is rejected with a clear error.
#[tokio::test]
async fn join_session_rejects_duplicate_username() {
    let broker = TestBroker::start("t_join_dup").await;
    room_cli::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .expect("first join failed");
    let err = room_cli::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .expect_err("second join should have failed");
    assert!(
        err.to_string().contains("already in use"),
        "error should mention 'already in use': {err}"
    );
}

/// Two distinct usernames can both register in the same room.
#[tokio::test]
async fn join_session_allows_different_usernames() {
    let broker = TestBroker::start("t_join_two").await;
    let (_, tok1) = room_cli::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .expect("alice join failed");
    let (_, tok2) = room_cli::oneshot::join_session(&broker.socket_path, "bob")
        .await
        .expect("bob join failed");
    assert_ne!(tok1, tok2, "each user must receive a distinct token");
}

/// `send_message_with_token` delivers a message and returns the broadcast echo.
#[tokio::test]
async fn send_with_token_delivers_message() {
    let broker = TestBroker::start("t_send_token").await;

    let mut watcher = TestClient::connect(&broker.socket_path, "watcher").await;
    watcher
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "watcher"))
        .await;

    let (_, token) = room_cli::oneshot::join_session(&broker.socket_path, "agent")
        .await
        .expect("join_session failed");

    let wire = serde_json::json!({"type": "message", "content": "hello from token"}).to_string();
    let msg = room_cli::oneshot::send_message_with_token(&broker.socket_path, &token, &wire)
        .await
        .expect("send_message_with_token failed");

    assert!(
        matches!(&msg, Message::Message { user, content, .. }
            if user == "agent" && content == "hello from token"),
        "unexpected message: {msg:?}"
    );

    let received = watcher
        .recv_until(|m| matches!(m, Message::Message { user, .. } if user == "agent"))
        .await;
    assert!(
        matches!(&received, Message::Message { content, .. } if content == "hello from token"),
        "watcher got unexpected message: {received:?}"
    );
}

/// An invalid (unknown) token returns a clear error from the broker.
#[tokio::test]
async fn send_with_invalid_token_returns_error() {
    let broker = TestBroker::start("t_invalid_token").await;
    let wire = serde_json::json!({"type": "message", "content": "hi"}).to_string();
    let err =
        room_cli::oneshot::send_message_with_token(&broker.socket_path, "not-a-real-token", &wire)
            .await
            .expect_err("should have failed with invalid token");
    assert!(
        err.to_string().contains("invalid token"),
        "error should mention 'invalid token': {err}"
    );
}

/// After a `join_session` the token can be used from two sequential sends.
#[tokio::test]
async fn token_is_reusable_across_sends() {
    let broker = TestBroker::start("t_token_reuse").await;
    let (_, token) = room_cli::oneshot::join_session(&broker.socket_path, "agent")
        .await
        .expect("join_session failed");

    for i in 0..2u8 {
        let wire =
            serde_json::json!({"type": "message", "content": format!("msg {i}")}).to_string();
        room_cli::oneshot::send_message_with_token(&broker.socket_path, &token, &wire)
            .await
            .unwrap_or_else(|e| panic!("send {i} failed: {e}"));
    }
}

// ── Admin command tests ───────────────────────────────────────────────────────

/// `\kick <username>` invalidates the target's token; subsequent sends fail.
#[tokio::test]
async fn admin_kick_invalidates_token() {
    let broker = TestBroker::start("t_kick").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    // Drain admin's own join
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    let (_, victim_token) = room_cli::oneshot::join_session(&broker.socket_path, "victim")
        .await
        .expect("join failed");

    // Admin kicks victim
    admin
        .send_json(r#"{"type":"command","cmd":"kick","params":["victim"]}"#)
        .await;

    // Kick produces a system broadcast
    let sys = admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;
    assert!(
        matches!(&sys, Message::System { content, .. } if content.contains("kicked victim")),
        "expected kick system message, got: {sys:?}"
    );

    // Victim's token is now invalid
    let wire = serde_json::json!({"type": "message", "content": "sneaky"}).to_string();
    let err = room_cli::oneshot::send_message_with_token(&broker.socket_path, &victim_token, &wire)
        .await
        .expect_err("send with kicked token should fail");
    assert!(
        err.to_string().contains("invalid token"),
        "expected invalid token error, got: {err}"
    );
}

/// Kicking two users must not unblock the first when the second is kicked.
#[tokio::test]
async fn admin_kick_two_users_both_blocked() {
    let broker = TestBroker::start("t_kick2").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    let (_, tok_a) = room_cli::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .unwrap();
    let (_, tok_b) = room_cli::oneshot::join_session(&broker.socket_path, "bob")
        .await
        .unwrap();

    admin
        .send_json(r#"{"type":"command","cmd":"kick","params":["alice"]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;

    admin
        .send_json(r#"{"type":"command","cmd":"kick","params":["bob"]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;

    let wire = serde_json::json!({"type": "message", "content": "x"}).to_string();
    for (user, tok) in [("alice", &tok_a), ("bob", &tok_b)] {
        let result =
            room_cli::oneshot::send_message_with_token(&broker.socket_path, tok, &wire).await;
        assert!(
            result.is_err(),
            "{user} should still be blocked after two kicks"
        );
    }
}

/// `/reauth <username>` removes the token so the user can rejoin.
#[tokio::test]
async fn admin_reauth_allows_rejoin() {
    let broker = TestBroker::start("t_reauth").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    // Join as "alice" to register the username
    let (_, _alice_token) = room_cli::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .expect("alice join failed");

    // Alice is now registered; a second join would fail
    let err = room_cli::oneshot::join_session(&broker.socket_path, "alice")
        .await
        .expect_err("duplicate alice join should fail");
    assert!(err.to_string().contains("username_taken") || err.to_string().contains("alice"));

    // Admin reauthenticates alice
    admin
        .send_json(r#"{"type":"command","cmd":"reauth","params":["alice"]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;

    // Now alice can rejoin
    let result = room_cli::oneshot::join_session(&broker.socket_path, "alice").await;
    assert!(
        result.is_ok(),
        "alice should be able to rejoin after reauth"
    );
}

/// `/clear-tokens` removes all tokens; no user can send until they rejoin.
#[tokio::test]
async fn admin_clear_tokens_blocks_all_sends() {
    let broker = TestBroker::start("t_clear_tokens").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    let (_, tok1) = room_cli::oneshot::join_session(&broker.socket_path, "u1")
        .await
        .unwrap();
    let (_, tok2) = room_cli::oneshot::join_session(&broker.socket_path, "u2")
        .await
        .unwrap();

    admin
        .send_json(r#"{"type":"command","cmd":"clear-tokens","params":[]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { content, .. } if content.contains("cleared all tokens")))
        .await;

    for (user, tok) in [("u1", &tok1), ("u2", &tok2)] {
        let wire = serde_json::json!({"type": "message", "content": "hi"}).to_string();
        let result =
            room_cli::oneshot::send_message_with_token(&broker.socket_path, tok, &wire).await;
        assert!(
            result.is_err(),
            "send from {user} should fail after clear-tokens"
        );
    }
}

/// `/clear` truncates the history file and broadcasts a system message.
#[tokio::test]
async fn admin_clear_history() {
    let broker = TestBroker::start("t_clear_history").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    // Write something to history first
    let wire = serde_json::json!({"type": "message", "content": "keep me"}).to_string();
    room_cli::oneshot::send_message(&broker.socket_path, "admin", &wire)
        .await
        .unwrap();

    // Verify history has content
    let before = history::load(&broker.chat_path).await.unwrap();
    assert!(
        !before.is_empty(),
        "history should have at least join + message"
    );

    admin
        .send_json(r#"{"type":"command","cmd":"clear","params":[]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { content, .. } if content.contains("cleared chat history")))
        .await;

    // The /clear system message itself is written after truncation, so history
    // should contain only that one entry now.
    let after = history::load(&broker.chat_path).await.unwrap();
    assert_eq!(
        after.len(),
        1,
        "history should have only the clear notice: {after:?}"
    );
    assert!(
        matches!(&after[0], Message::System { content, .. } if content.contains("cleared chat history"))
    );
}

/// `/exit` broadcasts a shutdown notice and the broker stops accepting connections.
#[tokio::test]
async fn admin_exit_shuts_down_broker() {
    let broker = TestBroker::start("t_exit").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    admin
        .send_json(r#"{"type":"command","cmd":"exit","params":[]}"#)
        .await;
    admin
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("shutting down")),
        )
        .await;

    // Give the broker a moment to stop listening
    tokio::time::sleep(Duration::from_millis(100)).await;

    // New connections should fail
    let result = UnixStream::connect(&broker.socket_path).await;
    assert!(
        result.is_err(),
        "broker should have stopped accepting connections after /exit"
    );
}

/// After `/exit`, connected clients receive EOF (Ok(0)) on their socket read,
/// which the TUI detects to exit cleanly without requiring Ctrl-C.
#[tokio::test]
async fn exit_causes_broker_to_close_client_connections() {
    let broker = TestBroker::start("t_exit_eof").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    // A second client to verify all connections are closed, not just admin's.
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    admin
        .send_json(r#"{"type":"command","cmd":"exit","params":[]}"#)
        .await;

    // Both clients should see the shutdown message.
    admin
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("shutting down")),
        )
        .await;
    alice
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("shutting down")),
        )
        .await;

    // After the shutdown message the broker closes the sockets. Both clients
    // should get EOF (read returns 0 bytes) within a short window.
    let admin_eof = tokio::time::timeout(Duration::from_millis(500), async {
        let mut buf = String::new();
        admin.reader.read_line(&mut buf).await.unwrap_or(0)
    })
    .await;
    assert!(
        matches!(admin_eof, Ok(0)),
        "admin socket should receive EOF after /exit"
    );

    let alice_eof = tokio::time::timeout(Duration::from_millis(500), async {
        let mut buf = String::new();
        alice.reader.read_line(&mut buf).await.unwrap_or(0)
    })
    .await;
    assert!(
        matches!(alice_eof, Ok(0)),
        "alice socket should receive EOF after /exit"
    );
}

// ── pull_messages tests ────────────────────────────────────────────────────────

/// `pull_messages` returns the last N messages from history.

/// After `/kick`, the kicked user must not appear in subsequent `/who` responses.
#[tokio::test]
async fn kick_removes_user_from_who() {
    let broker = TestBroker::start("t_kick_who").await;
    let mut admin = TestClient::connect(&broker.socket_path, "admin").await;
    admin
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    let mut victim = TestClient::connect(&broker.socket_path, "victim").await;
    victim
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "victim"))
        .await;

    // Admin kicks victim
    admin
        .send_json(r#"{"type":"command","cmd":"kick","params":["victim"]}"#)
        .await;
    admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;

    // Admin queries /who — victim should no longer appear
    admin
        .send_json(r#"{"type":"command","cmd":"who","params":[]}"#)
        .await;
    let sys = admin
        .recv_until(|m| matches!(m, Message::System { .. }))
        .await;
    let Message::System { content, .. } = &sys else {
        panic!("expected system message");
    };
    assert!(
        !content.contains("victim"),
        "kicked user should not appear in /who, got: {content}"
    );
    assert!(
        content.contains("admin"),
        "admin should still appear in /who, got: {content}"
    );
}

/// `/who` via oneshot send returns the user list to the sender without broadcasting.
#[tokio::test]
async fn oneshot_who_returns_user_list() {
    let broker = TestBroker::start("t_oneshot_who").await;
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { .. }))
        .await;

    let (_, tok) = room_cli::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .unwrap();

    let wire = serde_json::json!({"type":"command","cmd":"who","params":[]}).to_string();
    let msg = room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok, &wire)
        .await
        .expect("oneshot /who should succeed");

    let Message::System { content, .. } = msg else {
        panic!("expected system message, got: {msg:?}");
    };
    assert!(
        content.contains("alice"),
        "/who should list alice, got: {content}"
    );

    // The Command should NOT have been broadcast to alice
    let got_cmd = tokio::time::timeout(std::time::Duration::from_millis(200), async {
        alice
            .recv_until(|m| matches!(m, Message::Command { cmd, .. } if cmd == "who"))
            .await
    })
    .await;
    assert!(
        got_cmd.is_err(),
        "/who command should not be broadcast to other clients"
    );
}

/// A non-host user sending an admin command receives a private "permission denied"
/// system message. The command is not executed and no broadcast is sent.
#[tokio::test]
async fn non_host_admin_cmd_is_rejected() {
    let broker = TestBroker::start("t_admin_auth_reject").await;

    // alice is host (first to join)
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // bob joins — not the host
    let mut bob = TestClient::connect(&broker.socket_path, "bob").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;
    bob.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "bob"))
        .await;

    // bot joins via token (oneshot JOIN), then attempts /kick alice
    let (_, tok) = room_cli::oneshot::join_session(&broker.socket_path, "bot")
        .await
        .unwrap();
    let msg = room_cli::oneshot::send_message_with_token(
        &broker.socket_path,
        &tok,
        r#"{"type":"command","cmd":"kick","params":["alice"]}"#,
    )
    .await
    .expect("oneshot send should succeed even when permission is denied");

    let Message::System { content, .. } = msg else {
        panic!("expected system message, got: {msg:?}");
    };
    assert!(
        content.contains("permission denied"),
        "non-host admin cmd should return permission denied, got: {content}"
    );

    // alice must NOT have received a kick broadcast
    let got_kick = tokio::time::timeout(Duration::from_millis(200), async {
        alice
            .recv_until(
                |m| matches!(m, Message::System { content, .. } if content.contains("kicked")),
            )
            .await
    })
    .await;
    assert!(
        got_kick.is_err(),
        "kick must not execute; alice should not see a kick system message"
    );
}

// ── Duplicate username tests (P0 gap) ───────────────────────────────────────

/// After a second join attempt is rejected, the first session's token still works.
#[tokio::test]
async fn duplicate_join_preserves_original_token() {
    let broker = TestBroker::start("t_dup_preserves").await;
    let (_, token) = room_cli::oneshot::join_session(&broker.socket_path, "agent")
        .await
        .expect("first join failed");

    // Second join is rejected
    let err = room_cli::oneshot::join_session(&broker.socket_path, "agent")
        .await
        .expect_err("duplicate join should fail");
    assert!(
        err.to_string().contains("already in use"),
        "error should mention 'already in use': {err}"
    );

    // Original token remains valid — can still send
    let wire = serde_json::json!({"type": "message", "content": "still alive"}).to_string();
    let msg = room_cli::oneshot::send_message_with_token(&broker.socket_path, &token, &wire)
        .await
        .expect("send with original token should succeed after rejected duplicate");
    assert!(
        matches!(&msg, Message::Message { content, .. } if content == "still alive"),
        "unexpected message: {msg:?}"
    );
}

/// Interactive clients allow duplicate usernames (no token-based uniqueness
/// check). The second "alice" connects successfully and both coexist.
///
/// NOTE: This documents current behavior — interactive sessions do not go
/// through `issue_token()`, so the username_taken guard does not apply.
/// Only one-shot `JOIN:` (token-issuing) sessions enforce uniqueness.
#[tokio::test]
async fn duplicate_interactive_username_coexists() {
    let broker = TestBroker::start("t_dup_interactive").await;

    let mut alice1 = TestClient::connect(&broker.socket_path, "alice").await;
    alice1
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // Second interactive client with the same username succeeds
    let mut alice2 = TestClient::connect(&broker.socket_path, "alice").await;
    alice2
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // Both can send messages
    alice1.send_text("from alice1").await;
    alice2.send_text("from alice2").await;

    // alice1 receives alice2's message (and vice versa)
    let msg = alice1
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content == "from alice2"))
        .await;
    assert!(
        matches!(&msg, Message::Message { user, content, .. }
            if user == "alice" && content == "from alice2"),
        "alice1 should receive alice2's message: {msg:?}"
    );
}

/// In daemon mode, a duplicate username join across the SAME room is rejected
/// and the original token remains valid.
#[tokio::test]
async fn daemon_duplicate_join_same_room_rejected() {
    let td = common::TestDaemon::start(&["dup-room"]).await;

    let token = common::daemon_join(&td.socket_path, "dup-room", "agent").await;

    // Second join for same username in same room should fail
    let stream = tokio::net::UnixStream::connect(&td.socket_path)
        .await
        .unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:dup-room:JOIN:agent\n").await.unwrap();

    let mut reader = tokio::io::BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(
        v["type"], "error",
        "duplicate daemon join should return error, got: {v}"
    );
    assert_eq!(v["code"], "username_taken");

    // Original token still works
    let resp = common::daemon_send(
        &td.socket_path,
        "dup-room",
        &token,
        r#"{"type":"message","content":"still here"}"#,
    )
    .await;
    assert_eq!(resp["type"], "message");
    assert_eq!(resp["content"], "still here");
}

/// In daemon mode, a username taken in one room blocks joining ANY room
/// (tokens are daemon-scoped, not room-scoped).
#[tokio::test]
async fn daemon_duplicate_username_cross_room_rejected() {
    let td = common::TestDaemon::start(&["room-a", "room-b"]).await;

    // Join room-a as "agent"
    common::daemon_join(&td.socket_path, "room-a", "agent").await;

    // Try joining room-b with the same username — should be rejected
    // because daemon tokens are system-wide
    let stream = tokio::net::UnixStream::connect(&td.socket_path)
        .await
        .unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:room-b:JOIN:agent\n").await.unwrap();

    let mut reader = tokio::io::BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(
        v["type"], "error",
        "cross-room duplicate should return error, got: {v}"
    );
    assert_eq!(v["code"], "username_taken");
}

// ── Subscription persistence on join ─────────────────────────────────────────

/// Load a subscription map from disk (mirrors broker's load_subscription_map,
/// which is pub(crate) and inaccessible from integration tests).
fn load_sub_map(
    path: &std::path::Path,
) -> std::collections::HashMap<String, room_protocol::SubscriptionTier> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return std::collections::HashMap::new(),
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// After a one-shot JOIN, the subscription file is written to disk with the
/// user set to Full. This is the core fix for #438.
#[tokio::test]
async fn oneshot_join_persists_subscription_to_disk() {
    let broker = TestBroker::start("t_sub_persist").await;
    room_cli::oneshot::join_session(&broker.socket_path, "agent")
        .await
        .expect("join failed");

    // Give the broker a moment to persist (fire-and-forget write).
    tokio::time::sleep(Duration::from_millis(50)).await;

    let sub_path = broker.chat_path.with_extension("subscriptions");
    assert!(
        sub_path.exists(),
        "subscription file should exist after join"
    );

    let map = load_sub_map(&sub_path);
    assert_eq!(
        map.get("agent"),
        Some(&room_protocol::SubscriptionTier::Full),
        "join should persist Full subscription; got: {map:?}"
    );
}

/// After a one-shot JOIN via the daemon protocol, the subscription is persisted.
#[tokio::test]
async fn daemon_join_persists_subscription_to_disk() {
    let td = common::TestDaemon::start(&["sub-persist"]).await;
    common::daemon_join(&td.socket_path, "sub-persist", "bot").await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    let sub_path = td._dir.path().join("sub-persist.subscriptions");
    assert!(
        sub_path.exists(),
        "subscription file should exist after daemon join"
    );

    let map = load_sub_map(&sub_path);
    assert_eq!(
        map.get("bot"),
        Some(&room_protocol::SubscriptionTier::Full),
        "daemon join should persist Full subscription; got: {map:?}"
    );
}

/// A denied join (private room, not on invite list) must NOT create a
/// subscription entry.
#[tokio::test]
async fn denied_join_does_not_persist_subscription() {
    use std::collections::HashSet;

    let td = common::TestDaemon::start_with_configs(vec![(
        "private-sub",
        Some(RoomConfig {
            visibility: room_protocol::RoomVisibility::Private,
            max_members: None,
            invite_list: HashSet::new(),
            created_by: "host".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        }),
    )])
    .await;

    // Attempt to join — should be denied
    let stream = tokio::net::UnixStream::connect(&td.socket_path)
        .await
        .unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"ROOM:private-sub:JOIN:stranger\n")
        .await
        .unwrap();

    let mut reader = tokio::io::BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error", "join should be denied: {v}");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let sub_path = td._dir.path().join("private-sub.subscriptions");
    let map = load_sub_map(&sub_path);
    assert!(
        !map.contains_key("stranger"),
        "denied join must not create subscription entry; got: {map:?}"
    );
}

/// Subscription persists across broker restart: join, stop broker, start a new
/// broker with the same data files, verify subscription is loaded.
#[tokio::test]
async fn subscription_survives_broker_restart() {
    use room_cli::broker::Broker;

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("restart.sock");
    let chat_path = dir.path().join("restart.chat");

    // Phase 1: start broker, join, verify subscription written
    {
        let broker = Broker::new(
            "restart",
            chat_path.clone(),
            chat_path.with_extension("tokens"),
            chat_path.with_extension("subscriptions"),
            socket_path.clone(),
            None,
        );
        tokio::spawn(async move { broker.run().await.ok() });
        common::wait_for_socket(&socket_path, Duration::from_secs(1)).await;

        room_cli::oneshot::join_session(&socket_path, "survivor")
            .await
            .expect("join failed");

        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Phase 1 broker dropped — socket gone. Verify file exists.
    let sub_path = chat_path.with_extension("subscriptions");
    let map_before = load_sub_map(&sub_path);
    assert_eq!(
        map_before.get("survivor"),
        Some(&room_protocol::SubscriptionTier::Full),
        "subscription should be on disk before restart"
    );

    // Clean up stale socket for phase 2.
    let _ = std::fs::remove_file(&socket_path);

    // Phase 2: new broker instance loads the same subscription file
    {
        let broker = Broker::new(
            "restart",
            chat_path.clone(),
            chat_path.with_extension("tokens"),
            chat_path.with_extension("subscriptions"),
            socket_path.clone(),
            None,
        );
        tokio::spawn(async move { broker.run().await.ok() });
        common::wait_for_socket(&socket_path, Duration::from_secs(1)).await;

        // The subscription file should still have the entry
        let map_after = load_sub_map(&sub_path);
        assert_eq!(
            map_after.get("survivor"),
            Some(&room_protocol::SubscriptionTier::Full),
            "subscription should survive broker restart"
        );
    }
}

/// WS join persists the subscription to disk (ws_oneshot_join was missing
/// subscription insert entirely before #438).
#[tokio::test]
async fn ws_join_persists_subscription() {
    let (broker, port) = TestBroker::start_with_ws("t_ws_sub").await;

    let (_tx, mut rx) = common::ws_connect(port, "t_ws_sub", "JOIN:wsbot").await;
    let resp = common::ws_recv_json(&mut rx).await;
    assert_eq!(resp["type"], "token", "WS join should return token: {resp}");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let sub_path = broker.chat_path.with_extension("subscriptions");
    let map = load_sub_map(&sub_path);
    assert_eq!(
        map.get("wsbot"),
        Some(&room_protocol::SubscriptionTier::Full),
        "WS join should persist Full subscription; got: {map:?}"
    );
}

/// The room host (first interactive user) can execute admin commands.
#[tokio::test]
async fn host_can_run_admin_commands() {
    let broker = TestBroker::start("t_admin_auth_host").await;

    // alice is host (first to join)
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    let mut victim = TestClient::connect(&broker.socket_path, "victim").await;
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "victim"))
        .await;
    victim
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "victim"))
        .await;

    // alice (host) kicks via JSON command (matching TUI build_payload output)
    alice
        .send_json(r#"{"type":"command","cmd":"kick","params":["victim"]}"#)
        .await;

    // broadcast: all connected users receive the kick system message
    let sys = alice
        .recv_until(|m| matches!(m, Message::System { content, .. } if content.contains("kicked")))
        .await;
    let Message::System { content, .. } = &sys else {
        panic!("expected system message");
    };
    assert!(
        content.contains("victim"),
        "kick message should name the victim, got: {content}"
    );
}
