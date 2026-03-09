/// REST /api/{room}/query endpoint tests.
///
/// Covers: auth enforcement, filtering by user/content/since,
/// ordering, limit, DM privacy, and public flag validation.
mod common;

use common::{rest_join, rest_send, TestBroker, TestDaemon};
use room_cli::message::Message;
use room_protocol::{dm_room_id, RoomConfig};

#[tokio::test]
async fn rest_query_without_token_returns_401() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_noauth").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://127.0.0.1:{port}/api/ws_query_noauth/query"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "missing_token");
}

#[tokio::test]
async fn rest_query_with_invalid_token_returns_401() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_badauth").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/api/ws_query_badauth/query"
        ))
        .header("Authorization", "Bearer not-a-valid-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_token");
}

#[tokio::test]
async fn rest_query_wrong_room_returns_404() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_404room").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://127.0.0.1:{port}/api/nosuchroom/query"))
        .header("Authorization", "Bearer dummy")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn rest_query_no_params_returns_all_messages() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_all").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_all", "alice").await;
    rest_send(&client, &base, "ws_query_all", &token, "first message").await;
    rest_send(&client, &base, "ws_query_all", &token, "second message").await;

    let resp = client
        .get(format!("{base}/api/ws_query_all/query"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(|m| m["content"] == "first message"),
        "first message should appear"
    );
    assert!(
        messages.iter().any(|m| m["content"] == "second message"),
        "second message should appear"
    );
}

#[tokio::test]
async fn rest_query_user_filter() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_user").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let alice_tok = rest_join(&client, &base, "ws_query_user", "alice_qu").await;
    let bob_tok = rest_join(&client, &base, "ws_query_user", "bob_qu").await;

    rest_send(&client, &base, "ws_query_user", &alice_tok, "from alice").await;
    rest_send(&client, &base, "ws_query_user", &bob_tok, "from bob").await;

    let resp = client
        .get(format!("{base}/api/ws_query_user/query?user=alice_qu"))
        .header("Authorization", format!("Bearer {alice_tok}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert!(
        messages.iter().all(|m| m["user"] == "alice_qu"),
        "only alice's messages should be returned"
    );
    assert!(
        messages.iter().any(|m| m["content"] == "from alice"),
        "alice's message should be present"
    );
    assert!(
        !messages.iter().any(|m| m["content"] == "from bob"),
        "bob's message should not appear"
    );
}

#[tokio::test]
async fn rest_query_limit_and_ordering() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_limit").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_limit", "alice_lim").await;
    rest_send(&client, &base, "ws_query_limit", &token, "msg1").await;
    rest_send(&client, &base, "ws_query_limit", &token, "msg2").await;
    rest_send(&client, &base, "ws_query_limit", &token, "msg3").await;

    // n=2 newest-first (default).
    let resp = client
        .get(format!("{base}/api/ws_query_limit/query?n=2"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "should return exactly 2 messages");
    // Newest-first: msg3 then msg2.
    assert_eq!(messages[0]["content"], "msg3", "newest message first");
    assert_eq!(messages[1]["content"], "msg2");

    // asc=true: oldest-first, n=2.
    let resp_asc = client
        .get(format!("{base}/api/ws_query_limit/query?n=2&asc=true"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    let body_asc: serde_json::Value = resp_asc.json().await.unwrap();
    let msgs_asc = body_asc["messages"].as_array().unwrap();
    assert_eq!(msgs_asc.len(), 2);
    assert_eq!(
        msgs_asc[0]["content"], "msg1",
        "oldest message first with asc=true"
    );
}

#[tokio::test]
async fn rest_query_content_filter() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_content").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_content", "alice_cnt").await;
    rest_send(&client, &base, "ws_query_content", &token, "hello world").await;
    rest_send(&client, &base, "ws_query_content", &token, "goodbye world").await;
    rest_send(&client, &base, "ws_query_content", &token, "nothing here").await;

    let resp = client
        .get(format!("{base}/api/ws_query_content/query?content=world"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "two messages contain 'world'");
    assert!(messages.iter().any(|m| m["content"] == "hello world"));
    assert!(messages.iter().any(|m| m["content"] == "goodbye world"));
    assert!(!messages.iter().any(|m| m["content"] == "nothing here"));
}

#[tokio::test]
async fn rest_query_since_filter() {
    let (_tb, port) = TestBroker::start_with_ws("ws_query_since").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_since", "alice_s").await;
    rest_send(&client, &base, "ws_query_since", &token, "msg_a").await;
    rest_send(&client, &base, "ws_query_since", &token, "msg_b").await;
    rest_send(&client, &base, "ws_query_since", &token, "msg_c").await;

    // Get all messages oldest-first to find msg_b's seq.
    let all_resp = client
        .get(format!("{base}/api/ws_query_since/query?asc=true"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    let all_body: serde_json::Value = all_resp.json().await.unwrap();
    let all_msgs = all_body["messages"].as_array().unwrap();
    let msg_b_seq = all_msgs
        .iter()
        .find(|m| m["content"] == "msg_b")
        .and_then(|m| m["seq"].as_u64())
        .expect("msg_b should have a seq");

    // since=msg_b_seq — should only return msg_c (strictly after).
    let resp = client
        .get(format!(
            "{base}/api/ws_query_since/query?since={msg_b_seq}&asc=true"
        ))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert!(
        messages.iter().all(|m| m["content"] != "msg_a"),
        "msg_a should be excluded"
    );
    assert!(
        messages.iter().all(|m| m["content"] != "msg_b"),
        "msg_b itself should be excluded"
    );
    assert!(
        messages.iter().any(|m| m["content"] == "msg_c"),
        "msg_c should be included"
    );
}

#[tokio::test]
async fn rest_query_dm_privacy_enforced() {
    // Non-participant cannot see DM messages via /query.
    let dm_id = dm_room_id("alice", "bob").unwrap();
    let dm_config = RoomConfig::dm("alice", "bob");
    let (td, port) =
        TestDaemon::start_with_ws_configs(vec![(dm_id.as_str(), Some(dm_config))]).await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // alice sends a DM to bob (inject token directly, bypassing join permission check).
    let alice_tok = "tok-alice-query";
    td.state
        .test_inject_token(&dm_id, "alice", alice_tok)
        .await
        .unwrap();
    let send_resp = client
        .post(format!("{base}/api/{dm_id}/send"))
        .header("Authorization", format!("Bearer {alice_tok}"))
        .json(&serde_json::json!({"content": "secret dm", "to": "bob"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        send_resp.status(),
        200,
        "alice should be able to send to dm room"
    );

    // eve has a token but is not a participant.
    let eve_tok = "tok-eve-query";
    td.state
        .test_inject_token(&dm_id, "eve", eve_tok)
        .await
        .unwrap();
    let resp = client
        .get(format!("{base}/api/{dm_id}/query"))
        .header("Authorization", format!("Bearer {eve_tok}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert!(
        !messages.iter().any(|m| m["content"] == "secret dm"),
        "non-participant must not see DM messages via /query"
    );
}

#[tokio::test]
async fn rest_query_public_alone_returns_400() {
    // ?public=true without any other narrowing param should be rejected.
    let (_tb, port) = TestBroker::start_with_ws("ws_query_pub400").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_pub400", "alice_pub").await;

    let resp = client
        .get(format!("{base}/api/ws_query_pub400/query?public=true"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "public_requires_filter");
}

#[tokio::test]
async fn rest_query_public_with_narrowing_param_allowed() {
    // ?public=true with at least one narrowing param should succeed.
    let (_tb, port) = TestBroker::start_with_ws("ws_query_pub_ok").await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let token = rest_join(&client, &base, "ws_query_pub_ok", "alice_pubq").await;
    rest_send(&client, &base, "ws_query_pub_ok", &token, "hello").await;

    let resp = client
        .get(format!("{base}/api/ws_query_pub_ok/query?public=true&n=10"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["messages"].is_array());
}
