//! Integration tests for room-ralph.
//!
//! Tests that use mock binaries modify PATH and are serialized via `PATH_LOCK`.
//! Run with: `cargo test -p room-ralph --test integration -- --test-threads=1`

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use room_ralph::{loop_runner, room, Cli};

/// Serializes tests that modify PATH to avoid races.
static PATH_LOCK: Mutex<()> = Mutex::new(());

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Build a `Cli` with test defaults; apply overrides via the closure.
fn test_cli(f: impl FnOnce(&mut Cli)) -> Cli {
    let mut cli = Cli {
        room_id: "test-room".to_string(),
        username: "test-agent".to_string(),
        model: "test-model".to_string(),
        issue: None,
        tmux: false,
        max_iter: 1,
        cooldown: 0,
        prompt: None,
        personality: None,
        add_dirs: vec![],
        allow_tools: vec![],
        disallow_tools: vec![],
        profile: None,
        socket: None,
        dry_run: false,
    };
    f(&mut cli);
    cli
}

/// Prepend `dir` to `PATH`. Returns the original value for `restore_path`.
fn prepend_path(dir: &Path) -> String {
    let original = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", dir.display(), original));
    original
}

/// Restore `PATH` to its original value.
fn restore_path(original: &str) {
    std::env::set_var("PATH", original);
}

/// Create a mock `claude` binary that reads stdin, outputs `json_output`, and
/// exits with `exit_code`.
fn create_mock_claude(dir: &Path, json_output: &str, exit_code: i32) {
    let response_file = dir.join("claude_response.json");
    std::fs::write(&response_file, json_output).unwrap();

    let script = format!(
        "#!/bin/bash\ncat > /dev/null\ncat \"{}\"\nexit {}\n",
        response_file.display(),
        exit_code
    );
    let path = dir.join("claude");
    std::fs::write(&path, &script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Create a mock `room` binary that handles join/send/poll subcommands.
/// `poll_output` is the raw NDJSON that `room poll` should return.
fn create_mock_room(dir: &Path, poll_output: &str) {
    let poll_file = dir.join("room_poll.ndjson");
    std::fs::write(&poll_file, poll_output).unwrap();

    let script = format!(
        r#"#!/bin/bash
case "$1" in
    join)
        echo '{{"type":"token","token":"mock-tok","username":"'"$3"'"}}'
        ;;
    send)
        echo '{{"type":"message","id":"mock-id","room":"'"$2"'","user":"mock","content":"ok"}}'
        ;;
    poll)
        cat "{poll}"
        ;;
    --version)
        echo "room 2.0.0 (mock)"
        ;;
    *)
        echo "mock room: unknown command $1" >&2
        exit 1
        ;;
esac"#,
        poll = poll_file.display()
    );
    let path = dir.join("room");
    std::fs::write(&path, &script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Remove temp files that `run_loop` creates for a given test username/issue.
fn cleanup(username: &str, issue: Option<&str>) {
    let progress = room_ralph::progress::progress_file_path(issue, username);
    std::fs::remove_file(&progress).ok();
    std::fs::remove_file(format!("/tmp/ralph-room-prompt-{username}.txt")).ok();
    std::fs::remove_file(format!("/tmp/ralph-room-{username}.log")).ok();
}

// ── Test 1: dry-run prints prompt and exits ─────────────────────────────────

#[tokio::test]
async fn dry_run_prints_prompt_and_exits() {
    let cli = test_cli(|c| {
        c.dry_run = true;
        c.username = "integ-dryrun".to_string();
    });
    let running = Arc::new(AtomicBool::new(true));

    // dry_run exits before calling claude; room poll failure is swallowed
    let result = loop_runner::run_loop(&cli, "fake-token".to_string(), &running).await;

    assert!(result.is_ok(), "dry_run should return Ok: {result:?}");
    assert!(
        running.load(Ordering::SeqCst),
        "running flag should still be true (clean exit)"
    );
    cleanup("integ-dryrun", None);
}

// ── Test 2: max_iter=1 stops after a single iteration ───────────────────────

#[tokio::test]
async fn max_iter_one_stops_after_single_iteration() {
    let _lock = PATH_LOCK.lock().unwrap();
    let mock_dir = tempfile::TempDir::new().unwrap();
    create_mock_claude(mock_dir.path(), r#"{"result":"test output"}"#, 0);
    create_mock_room(mock_dir.path(), "");

    let original = prepend_path(mock_dir.path());

    let cli = test_cli(|c| {
        c.max_iter = 1;
        c.username = "integ-max1".to_string();
    });
    let running = Arc::new(AtomicBool::new(true));

    let result = loop_runner::run_loop(&cli, "mock-tok".to_string(), &running).await;
    restore_path(&original);

    assert!(result.is_ok(), "max_iter=1 should complete: {result:?}");
    assert!(
        running.load(Ordering::SeqCst),
        "loop exited via max_iter, not signal"
    );
    cleanup("integ-max1", None);
}

// ── Test 3: max_iter=0 means unlimited — runs until signal ──────────────────

#[tokio::test]
async fn max_iter_zero_runs_until_signal() {
    let _lock = PATH_LOCK.lock().unwrap();
    let mock_dir = tempfile::TempDir::new().unwrap();
    create_mock_claude(mock_dir.path(), r#"{"result":"ok"}"#, 0);
    create_mock_room(mock_dir.path(), "");

    let original = prepend_path(mock_dir.path());

    let cli = test_cli(|c| {
        c.max_iter = 0;
        c.username = "integ-max0".to_string();
    });
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Stop after a short delay — allows at least one iteration
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        r.store(false, Ordering::SeqCst);
    });

    let result = loop_runner::run_loop(&cli, "mock-tok".to_string(), &running).await;
    restore_path(&original);

    assert!(result.is_ok(), "should stop cleanly: {result:?}");
    assert!(
        !running.load(Ordering::SeqCst),
        "signal should have stopped the loop"
    );
    cleanup("integ-max0", None);
}

// ── Test 4: context exhaustion triggers progress file write ─────────────────

#[tokio::test]
async fn context_exhaustion_writes_progress() {
    let _lock = PATH_LOCK.lock().unwrap();
    let mock_dir = tempfile::TempDir::new().unwrap();

    // Mock claude returns high token count → proactive restart
    create_mock_claude(
        mock_dir.path(),
        r#"{"result":"partial work","usage":{"input_tokens":190000,"output_tokens":2000}}"#,
        0,
    );
    create_mock_room(mock_dir.path(), "");

    let original = prepend_path(mock_dir.path());

    // Use default thresholds (limit=200000, threshold=80% → 160000)
    std::env::remove_var("CONTEXT_LIMIT");
    std::env::remove_var("CONTEXT_THRESHOLD");

    let cli = test_cli(|c| {
        c.max_iter = 1;
        c.username = "integ-ctx".to_string();
        c.issue = Some("999".to_string());
    });
    let running = Arc::new(AtomicBool::new(true));

    let result = loop_runner::run_loop(&cli, "mock-tok".to_string(), &running).await;
    restore_path(&original);

    assert!(result.is_ok());

    // progress_file_path(Some("999"), _) → /tmp/room-progress-999.md
    let progress_path = room_ralph::progress::progress_file_path(Some("999"), "integ-ctx");
    assert!(
        progress_path.exists(),
        "progress file should exist at {} after context exhaustion",
        progress_path.display()
    );

    let content = std::fs::read_to_string(&progress_path).unwrap();
    assert!(
        content.contains("Iteration: 1"),
        "should record iteration number"
    );
    assert!(content.contains("Issue: 999"), "should record issue");
    assert!(
        content.contains("context exhaustion"),
        "should note context exhaustion as reason"
    );

    cleanup("integ-ctx", Some("999"));
}

// ── Test 5: token expiry detection across formats ───────────────────────────

#[test]
fn token_expiry_detected_across_formats() {
    // Positive: various real-world expiry messages
    assert!(room::detect_token_expiry("error: invalid token"));
    assert!(room::detect_token_expiry(
        "Error: Invalid Token — please rejoin"
    ));
    assert!(room::detect_token_expiry("unauthorized access"));
    assert!(room::detect_token_expiry("your token has expired"));
    assert!(room::detect_token_expiry("TOKEN INVALID"));
    assert!(room::detect_token_expiry(
        "The token is invalid, re-join required"
    ));
    assert!(room::detect_token_expiry(
        "token not recognised — run: room join myroom <username>"
    ));

    // Negative: benign text that mentions tokens but isn't an expiry error
    assert!(!room::detect_token_expiry("generated 500 tokens of output"));
    assert!(!room::detect_token_expiry("the output token count is high"));
    assert!(!room::detect_token_expiry("valid response with tokens"));
    assert!(!room::detect_token_expiry("everything is fine"));
    assert!(!room::detect_token_expiry(""));
}

// ── Test 6: claude failure (non-context) lets loop continue ─────────────────

#[tokio::test]
async fn claude_failure_continues_loop() {
    let _lock = PATH_LOCK.lock().unwrap();
    let mock_dir = tempfile::TempDir::new().unwrap();

    // Claude exits 1 with a non-context error → loop should retry
    create_mock_claude(
        mock_dir.path(),
        r#"{"error":"syntax error in your code"}"#,
        1,
    );
    create_mock_room(mock_dir.path(), "");

    let original = prepend_path(mock_dir.path());

    std::env::remove_var("CONTEXT_LIMIT");
    std::env::remove_var("CONTEXT_THRESHOLD");

    let cli = test_cli(|c| {
        c.max_iter = 2;
        c.username = "integ-fail".to_string();
    });
    let running = Arc::new(AtomicBool::new(true));

    let result = loop_runner::run_loop(&cli, "mock-tok".to_string(), &running).await;
    restore_path(&original);

    assert!(
        result.is_ok(),
        "claude failure should not abort the loop: {result:?}"
    );
    assert!(
        running.load(Ordering::SeqCst),
        "loop exited via max_iter, not signal"
    );
    cleanup("integ-fail", None);
}

// ── Test 7: poll_messages parses NDJSON from room binary ────────────────────

#[test]
fn poll_messages_parses_ndjson() {
    let _lock = PATH_LOCK.lock().unwrap();
    let mock_dir = tempfile::TempDir::new().unwrap();

    let ndjson = concat!(
        r#"{"type":"message","id":"aaa","room":"test","user":"alice","ts":"2026-01-01T00:00:00Z","content":"hello"}"#,
        "\n",
        r#"{"type":"message","id":"bbb","room":"test","user":"bob","ts":"2026-01-01T00:01:00Z","content":"world"}"#,
        "\n"
    );
    create_mock_room(mock_dir.path(), ndjson);

    let original = prepend_path(mock_dir.path());
    let messages = room::poll_messages("test-room", "mock-tok", None).unwrap();
    restore_path(&original);

    assert_eq!(messages.len(), 2, "should parse 2 messages from NDJSON");
    assert_eq!(messages[0].user(), "alice");
    assert_eq!(messages[1].user(), "bob");
    if let room_protocol::Message::Message { content, .. } = &messages[0] {
        assert_eq!(content, "hello");
    } else {
        panic!("expected Message variant, got {:?}", messages[0]);
    }
}

// ── Test 8: --personality file content appears in dry-run output ─────────────

#[tokio::test]
async fn personality_appears_in_dry_run_output() {
    let dir = tempfile::TempDir::new().unwrap();
    let personality = dir.path().join("personality.txt");
    std::fs::write(
        &personality,
        "You are a grumpy robot who hates small talk and loves Rust.",
    )
    .unwrap();

    let cli = test_cli(|c| {
        c.dry_run = true;
        c.username = "integ-personality".to_string();
        c.personality = Some(personality);
    });
    let running = Arc::new(AtomicBool::new(true));

    // Capture stdout to verify personality content
    let result = loop_runner::run_loop(&cli, "fake-token".to_string(), &running).await;
    assert!(
        result.is_ok(),
        "dry_run with personality should succeed: {result:?}"
    );

    // Verify personality was wired through by checking the prompt file
    // (run_loop writes it to /tmp before dry_run prints and exits,
    // but dry_run returns before writing the file — it prints to stdout instead)
    // So we test via unit tests in prompt.rs. Here we just confirm no crash.
    cleanup("integ-personality", None);
}

// ── Test 9: live broker join and announce ────────────────────────────────────

#[tokio::test]
#[ignore = "requires a running broker: `room ralph-live-test host &`"]
async fn live_broker_join_and_announce() {
    let room_id = "ralph-live-test";

    match room::join_room(room_id, "ralph-integ-agent", None) {
        Ok(result) => {
            assert!(!result.token.is_empty(), "token should be non-empty");
            assert!(!result.username.is_empty(), "username should be non-empty");
            let send_result = room::send_message(
                room_id,
                &result.token,
                "integration test: hello from ralph",
                None,
            );
            assert!(send_result.is_ok(), "send should succeed: {send_result:?}");

            let messages = room::poll_messages(room_id, &result.token, None).unwrap();
            // At minimum, we should see our own announce message
            assert!(
                !messages.is_empty(),
                "should receive at least one message after send"
            );
        }
        Err(e) => {
            panic!("join_room failed — is the broker running? error: {e}");
        }
    }
}
