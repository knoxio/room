/// Scripted multi-agent coordination tests.
///
/// These tests simulate real multi-agent workflows using the one-shot
/// CLI commands (join/send/poll/pull/who) against a live broker.
mod common;

use std::time::Duration;

use common::{daemon_connect, daemon_join, daemon_send, TestBroker, TestClient, TestDaemon};
use room_cli::message::Message;
use room_protocol::RoomConfig;
use tokio::io::AsyncBufReadExt;
use tokio::time::timeout;

#[tokio::test]
async fn scripted_three_agent_join_send_poll() {
    let broker = TestBroker::start("t_3agent").await;

    // Phase 1: all three agents join and get tokens
    let (_, tok_a) = room_cli::oneshot::join_session(&broker.socket_path, "agent-a")
        .await
        .unwrap();
    let (_, tok_b) = room_cli::oneshot::join_session(&broker.socket_path, "agent-b")
        .await
        .unwrap();
    let (_, tok_c) = room_cli::oneshot::join_session(&broker.socket_path, "agent-c")
        .await
        .unwrap();

    // Phase 2: scripted message exchange — strict A→B→C→A→B ordering
    let msgs = [
        (&tok_a, "starting task #42"),
        (&tok_b, "ack, reading target file"),
        (&tok_c, "standing by"),
        (&tok_a, "first draft done, running tests"),
        (&tok_b, "review ready"),
    ];
    for (token, content) in &msgs {
        let wire = serde_json::json!({"type": "message", "content": content}).to_string();
        room_cli::oneshot::send_message_with_token(&broker.socket_path, token, &wire)
            .await
            .unwrap();
    }

    // Phase 3: small delay so broker flushes to chat file
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Phase 4: each agent polls and sees all 5 messages in order
    let dir = tempfile::tempdir().unwrap();
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let cursor = dir.path().join(format!("{agent}.cursor"));
        let polled = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor, None, None, None)
            .await
            .unwrap();

        let contents: Vec<&str> = polled
            .iter()
            .filter_map(|m| match m {
                Message::Message { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(
            contents,
            vec![
                "starting task #42",
                "ack, reading target file",
                "standing by",
                "first draft done, running tests",
                "review ready",
            ],
            "{agent} should see all 5 messages in order"
        );
    }
}

/// Agent sets status, other agents see it via /who response.
#[tokio::test]
async fn scripted_status_visibility_across_agents() {
    let broker = TestBroker::start("t_status_vis").await;

    // host connects interactively to observe
    let mut host = TestClient::connect(&broker.socket_path, "host").await;
    host.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "host"))
        .await;

    // agent-a joins, sets status
    let (_, tok_a) = room_cli::oneshot::join_session(&broker.socket_path, "agent-a")
        .await
        .unwrap();

    let status_wire = serde_json::json!({
        "type": "command",
        "cmd": "set_status",
        "params": ["coding #42"]
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_a, &status_wire)
        .await
        .unwrap();

    // host should see the status system message
    let status_msg = host
        .recv_until(
            |m| matches!(m, Message::System { content, .. } if content.contains("set status")),
        )
        .await;
    assert!(
        matches!(&status_msg, Message::System { content, .. } if content.contains("coding #42")),
        "status message should contain 'coding #42': {status_msg:?}"
    );
}

/// DM round-trip: agent-a sends DM to agent-b via token auth.
/// agent-b sees it in poll, agent-c (bystander) does not.
#[tokio::test]
async fn scripted_dm_exchange_with_bystander_isolation() {
    let broker = TestBroker::start("t_dm_script").await;

    // host connects first (needed for DM routing — host also sees DMs)
    let mut host = TestClient::connect(&broker.socket_path, "host").await;
    host.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "host"))
        .await;

    // Three agents join (agent-c is a bystander — token unused)
    let (_, tok_a) = room_cli::oneshot::join_session(&broker.socket_path, "agent-a")
        .await
        .unwrap();
    let (_, tok_b) = room_cli::oneshot::join_session(&broker.socket_path, "agent-b")
        .await
        .unwrap();
    let _ = room_cli::oneshot::join_session(&broker.socket_path, "agent-c")
        .await
        .unwrap();

    // agent-a sends DM to agent-b
    let dm_wire = serde_json::json!({
        "type": "dm",
        "to": "agent-b",
        "content": "secret plan"
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_a, &dm_wire)
        .await
        .unwrap();

    // also send a public message so we have a cursor anchor
    let pub_wire = serde_json::json!({"type": "message", "content": "public msg"}).to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_b, &pub_wire)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let dir = tempfile::tempdir().unwrap();

    // agent-b polls: should see the DM
    let cursor_b = dir.path().join("b.cursor");
    let polled_b =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_b, Some("agent-b"), None, None)
            .await
            .unwrap();
    let b_contents: Vec<&str> = polled_b
        .iter()
        .filter_map(|m| match m {
            Message::DirectMessage { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        b_contents.contains(&"secret plan"),
        "agent-b should see the DM"
    );

    // agent-c polls: should NOT see the DM
    let cursor_c = dir.path().join("c.cursor");
    let polled_c =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_c, Some("agent-c"), None, None)
            .await
            .unwrap();
    let c_dm_contents: Vec<&str> = polled_c
        .iter()
        .filter_map(|m| match m {
            Message::DirectMessage { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !c_dm_contents.contains(&"secret plan"),
        "agent-c should NOT see the DM"
    );
}

/// Token-authenticated agents can send and poll without interactive sessions.
/// Simulates the exact oneshot workflow agents use in production.
#[tokio::test]
async fn scripted_token_auth_send_poll_workflow() {
    let broker = TestBroker::start("t_token_workflow").await;

    // Phase 1: two agents register
    let (_, tok1) = room_cli::oneshot::join_session(&broker.socket_path, "bot-1")
        .await
        .unwrap();
    let (_, tok2) = room_cli::oneshot::join_session(&broker.socket_path, "bot-2")
        .await
        .unwrap();

    // Phase 2: bot-1 sends a message, bot-2 sends a reply
    let wire1 =
        serde_json::json!({"type": "message", "content": "plan: modify src/lib.rs"}).to_string();
    let echo1 = room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok1, &wire1)
        .await
        .unwrap();
    let anchor_id = echo1.id().to_string();

    let wire2 = serde_json::json!({"type": "message", "content": "ack, no overlap"}).to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok2, &wire2)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Phase 3: bot-1 polls since its own message — should see bot-2's reply only
    let dir = tempfile::tempdir().unwrap();
    let cursor = dir.path().join("bot1.cursor");
    let polled =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor, None, None, Some(&anchor_id))
            .await
            .unwrap();

    let contents: Vec<&str> = polled
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();

    assert!(
        contents.contains(&"ack, no overlap"),
        "bot-1 should see bot-2's reply"
    );
    assert!(
        !contents.contains(&"plan: modify src/lib.rs"),
        "bot-1's own message should be excluded (before anchor)"
    );
}

/// Daemon multi-room: two agents in separate rooms, messages are isolated.
/// Simulates the production daemon pattern where each room is independent.
#[tokio::test]
async fn scripted_daemon_multi_room_isolation() {
    let td = TestDaemon::start(&["room-alpha", "room-beta"]).await;

    // agent-a joins room-alpha, agent-b joins room-beta
    let tok_a = daemon_join(&td.socket_path, "room-alpha", "agent-a").await;
    let tok_b = daemon_join(&td.socket_path, "room-beta", "agent-b").await;

    // Each sends a message to their own room
    daemon_send(&td.socket_path, "room-alpha", &tok_a, "alpha msg").await;
    daemon_send(&td.socket_path, "room-beta", &tok_b, "beta msg").await;

    // agent-c joins room-alpha — should see alpha msg but NOT beta msg
    let (mut reader_c, _writer_c) = daemon_connect(&td.socket_path, "room-alpha", "agent-c").await;

    // Drain all messages with a short timeout, check for alpha/beta
    let mut saw_alpha = false;
    let mut saw_beta = false;
    let mut line = String::new();
    loop {
        line.clear();
        match timeout(Duration::from_millis(500), reader_c.read_line(&mut line)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(_)) => {
                if line.contains("alpha msg") {
                    saw_alpha = true;
                }
                if line.contains("beta msg") {
                    saw_beta = true;
                }
            }
            Ok(Err(_)) => break,
        }
    }
    assert!(saw_alpha, "agent-c should see alpha msg in room-alpha");
    assert!(!saw_beta, "beta msg must not leak into room-alpha");
}

/// Mention-based message filtering: agent-a sends messages mentioning agent-b,
/// agent-b can filter poll results to only @-mentioned messages.
#[tokio::test]
async fn scripted_mention_filter_in_poll() {
    let broker = TestBroker::start("t_mention_filter").await;

    let (_, tok_a) = room_cli::oneshot::join_session(&broker.socket_path, "agent-a")
        .await
        .unwrap();
    let (_, _tok_b) = room_cli::oneshot::join_session(&broker.socket_path, "agent-b")
        .await
        .unwrap();

    // agent-a sends 3 messages: 2 mention agent-b, 1 does not
    for content in &[
        "@agent-b please review PR #42",
        "running tests now",
        "@agent-b tests pass, merging",
    ] {
        let wire = serde_json::json!({"type": "message", "content": content}).to_string();
        room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_a, &wire)
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Poll all messages — should see all 3
    let dir = tempfile::tempdir().unwrap();
    let cursor_all = dir.path().join("all.cursor");
    let all_msgs =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_all, None, None, None)
            .await
            .unwrap();
    let all_contents: Vec<&str> = all_msgs
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(all_contents.len(), 3, "should see all 3 messages");

    // Filter to mentions of agent-b using Message::mentions()
    let cursor_mentions = dir.path().join("mentions.cursor");
    let mention_msgs =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_mentions, None, None, None)
            .await
            .unwrap();
    let mentioned: Vec<&str> = mention_msgs
        .iter()
        .filter(|m| m.mentions().contains(&"agent-b".to_string()))
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        mentioned.len(),
        2,
        "should find 2 messages mentioning agent-b"
    );
    assert!(mentioned.contains(&"@agent-b please review PR #42"));
    assert!(mentioned.contains(&"@agent-b tests pass, merging"));
}

/// Full coordination lifecycle: join → announce → status → work → poll → PR.
/// This mirrors the exact sequence from CLAUDE.md's "Expected behaviour" section.
#[tokio::test]
async fn scripted_full_coordination_lifecycle() {
    let broker = TestBroker::start("t_lifecycle").await;

    // BA (host) connects interactively
    let mut ba = TestClient::connect(&broker.socket_path, "ba").await;
    ba.recv_until(|m| matches!(m, Message::Join { user, .. } if user == "ba"))
        .await;

    // Two worker agents register
    let (_, tok_r2d2) = room_cli::oneshot::join_session(&broker.socket_path, "r2d2")
        .await
        .unwrap();
    let (_, tok_bb) = room_cli::oneshot::join_session(&broker.socket_path, "bb")
        .await
        .unwrap();

    // Step 1: r2d2 announces plan
    let wire = serde_json::json!({
        "type": "message",
        "content": "plan: implement #42. files: src/lib.rs, src/main.rs"
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_r2d2, &wire)
        .await
        .unwrap();

    // BA sees the announcement
    let plan_msg = ba
        .recv_until(|m| matches!(m, Message::Message { content, .. } if content.contains("plan:")))
        .await;
    assert_eq!(plan_msg.user(), "r2d2");

    // Step 2: BA approves
    ba.send_text("go ahead").await;

    // Step 3: r2d2 sets status
    let status_wire = serde_json::json!({
        "type": "command",
        "cmd": "set_status",
        "params": ["coding #42"]
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_r2d2, &status_wire)
        .await
        .unwrap();

    // Step 4: r2d2 sends milestone update
    let milestone = serde_json::json!({
        "type": "message",
        "content": "first draft done, running tests"
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_r2d2, &milestone)
        .await
        .unwrap();

    // Step 5: bb sends a message too (concurrent work)
    let bb_msg = serde_json::json!({
        "type": "message",
        "content": "PR #43 ready for review @ba"
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_bb, &bb_msg)
        .await
        .unwrap();

    // Step 6: r2d2 announces PR
    let pr_wire = serde_json::json!({
        "type": "message",
        "content": "opening PR for #42. modified: src/lib.rs, src/main.rs"
    })
    .to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok_r2d2, &pr_wire)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify: poll the full history — messages are in order with correct senders
    let dir = tempfile::tempdir().unwrap();
    let cursor = dir.path().join("verify.cursor");
    let history = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor, None, None, None)
        .await
        .unwrap();

    let chat_msgs: Vec<(&str, &str)> = history
        .iter()
        .filter_map(|m| match m {
            Message::Message { user, content, .. } => Some((user.as_str(), content.as_str())),
            _ => None,
        })
        .collect();

    // Verify ordering: r2d2 plan → ba go → r2d2 milestone → bb PR → r2d2 PR
    assert_eq!(chat_msgs[0].0, "r2d2");
    assert!(chat_msgs[0].1.contains("plan:"));
    assert_eq!(chat_msgs[1].0, "ba");
    assert!(chat_msgs[1].1.contains("go ahead"));
    assert_eq!(chat_msgs[2].0, "r2d2");
    assert!(chat_msgs[2].1.contains("first draft"));
    assert_eq!(chat_msgs[3].0, "bb");
    assert!(chat_msgs[3].1.contains("PR #43"));
    assert_eq!(chat_msgs[4].0, "r2d2");
    assert!(chat_msgs[4].1.contains("opening PR"));
}

/// Cursor isolation: two agents polling the same room maintain independent cursors.
/// Agent-a polls first (advances cursor), agent-b polls and sees everything.
#[tokio::test]
async fn scripted_cursor_isolation_between_agents() {
    let broker = TestBroker::start("t_cursor_iso").await;

    let (_, tok) = room_cli::oneshot::join_session(&broker.socket_path, "sender")
        .await
        .unwrap();

    // Send 3 messages
    for i in 1..=3 {
        let wire =
            serde_json::json!({"type": "message", "content": format!("msg-{i}")}).to_string();
        room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok, &wire)
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    let dir = tempfile::tempdir().unwrap();

    // agent-a polls — sees all 3, advances its cursor
    let cursor_a = dir.path().join("a.cursor");
    let polled_a = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_a, None, None, None)
        .await
        .unwrap();
    let a_contents: Vec<&str> = polled_a
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(a_contents, vec!["msg-1", "msg-2", "msg-3"]);

    // agent-a polls again — nothing new
    let polled_a2 =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_a, None, None, None)
            .await
            .unwrap();
    let a2_msgs: Vec<&str> = polled_a2
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert!(a2_msgs.is_empty(), "agent-a second poll should be empty");

    // agent-b polls with independent cursor — sees all 3
    let cursor_b = dir.path().join("b.cursor");
    let polled_b = room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_b, None, None, None)
        .await
        .unwrap();
    let b_contents: Vec<&str> = polled_b
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(b_contents, vec!["msg-1", "msg-2", "msg-3"]);

    // New message arrives — only agents who haven't seen it get it
    let wire4 = serde_json::json!({"type": "message", "content": "msg-4"}).to_string();
    room_cli::oneshot::send_message_with_token(&broker.socket_path, &tok, &wire4)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let polled_a3 =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_a, None, None, None)
            .await
            .unwrap();
    let a3_contents: Vec<&str> = polled_a3
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(a3_contents, vec!["msg-4"], "agent-a should see only msg-4");

    let polled_b2 =
        room_cli::oneshot::poll_messages(&broker.chat_path, &cursor_b, None, None, None)
            .await
            .unwrap();
    let b2_contents: Vec<&str> = polled_b2
        .iter()
        .filter_map(|m| match m {
            Message::Message { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(b2_contents, vec!["msg-4"], "agent-b should see only msg-4");
}
