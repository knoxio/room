#!/usr/bin/env bash
# test-ralph-room.sh — Unit tests for ralph-room.sh
#
# Usage: bash scripts/test-ralph-room.sh
#
# Tests the pure functions in ralph-room.sh by sourcing it with RALPH_ROOM_SOURCED=1
# to skip main(). Each test function runs in a subshell for isolation.
set -euo pipefail

PASS=0
FAIL=0
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Source ralph-room.sh without running main
RALPH_ROOM_SOURCED=1 source "$SCRIPT_DIR/ralph-room.sh"

# --- test helpers ---
assert_eq() {
    local label="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        PASS=$((PASS + 1))
        printf '  PASS: %s\n' "$label"
    else
        FAIL=$((FAIL + 1))
        printf '  FAIL: %s\n    expected: %s\n    actual:   %s\n' "$label" "$expected" "$actual"
    fi
}

assert_contains() {
    local label="$1" needle="$2" haystack="$3"
    if echo "$haystack" | grep -qF -- "$needle"; then
        PASS=$((PASS + 1))
        printf '  PASS: %s\n' "$label"
    else
        FAIL=$((FAIL + 1))
        printf '  FAIL: %s (does not contain "%s")\n' "$label" "$needle"
    fi
}

assert_exit() {
    local label="$1" expected_code="$2"
    shift 2
    local actual_code=0
    "$@" >/dev/null 2>&1 || actual_code=$?
    if [[ "$actual_code" -eq "$expected_code" ]]; then
        PASS=$((PASS + 1))
        printf '  PASS: %s\n' "$label"
    else
        FAIL=$((FAIL + 1))
        printf '  FAIL: %s (expected exit %d, got %d)\n' "$label" "$expected_code" "$actual_code"
    fi
}

# --- tests ---

echo "=== Path helpers ==="

assert_eq "progress_file_path with issue" \
    "/tmp/room-progress-42.md" \
    "$(progress_file_path "42" "saphire")"

assert_eq "progress_file_path without issue falls back to username" \
    "/tmp/room-progress-saphire.md" \
    "$(progress_file_path "" "saphire")"

assert_eq "log_file_path" \
    "/tmp/ralph-room-saphire.log" \
    "$(log_file_path "saphire")"

assert_eq "token_file_path" \
    "/tmp/room-myroom-saphire.token" \
    "$(token_file_path "myroom" "saphire")"

assert_eq "token_file_path with dashes in room_id" \
    "/tmp/room-agent-room-2-bot.token" \
    "$(token_file_path "agent-room-2" "bot")"

echo ""
echo "=== Context exhaustion detection ==="

assert_exit "context limit in output triggers detection" \
    0 detect_context_exhaustion 1 "Error: context limit exceeded"

assert_exit "context window in output triggers detection" \
    0 detect_context_exhaustion 1 "the context window is full"

assert_exit "conversation too long triggers detection" \
    0 detect_context_exhaustion 1 "conversation too long to continue"

assert_exit "token limit triggers detection" \
    0 detect_context_exhaustion 1 "maximum token limit reached"

assert_exit "maximum context triggers detection" \
    0 detect_context_exhaustion 1 "exceeded maximum context length"

assert_exit "normal exit code 0 does NOT trigger" \
    1 detect_context_exhaustion 0 "all done"

assert_exit "non-zero exit without context keywords does NOT trigger" \
    1 detect_context_exhaustion 1 "network error: connection refused"

assert_exit "empty response with non-zero exit does NOT trigger" \
    1 detect_context_exhaustion 1 ""

echo ""
echo "=== Token expiry detection ==="

assert_exit "invalid token triggers expiry" \
    0 detect_token_expiry "error: invalid token"

assert_exit "unauthorized triggers expiry" \
    0 detect_token_expiry "HTTP 401 Unauthorized"

assert_exit "token expired triggers expiry" \
    0 detect_token_expiry "your token expired"

assert_exit "normal output does NOT trigger expiry" \
    1 detect_token_expiry "task completed successfully"

assert_exit "empty string does NOT trigger expiry" \
    1 detect_token_expiry ""

echo ""
echo "=== Progress file writing ==="

TEMP_PROGRESS="$(mktemp)"
trap 'rm -f "$TEMP_PROGRESS"' EXIT

write_progress_file "$TEMP_PROGRESS" 3 "42" "line 1
line 2
line 3
final output here"

assert_contains "progress file has iteration" \
    "Iteration: 3" "$(cat "$TEMP_PROGRESS")"

assert_contains "progress file has issue" \
    "Issue: 42" "$(cat "$TEMP_PROGRESS")"

assert_contains "progress file has reason" \
    "context exhaustion" "$(cat "$TEMP_PROGRESS")"

assert_contains "progress file has last output" \
    "final output here" "$(cat "$TEMP_PROGRESS")"

# test with no issue
write_progress_file "$TEMP_PROGRESS" 1 "" "some output"

assert_contains "progress file without issue says unassigned" \
    "Issue: unassigned" "$(cat "$TEMP_PROGRESS")"

echo ""
echo "=== Response extraction ==="

TEMP_OUTPUT="$(mktemp)"

# json with result field
printf '{"result":"hello world"}' > "$TEMP_OUTPUT"
assert_eq "extract result from json" \
    "hello world" \
    "$(extract_response "$TEMP_OUTPUT")"

# json with content field
printf '{"content":"content here"}' > "$TEMP_OUTPUT"
assert_eq "extract content from json" \
    "content here" \
    "$(extract_response "$TEMP_OUTPUT")"

# json with error field
printf '{"error":"something broke"}' > "$TEMP_OUTPUT"
assert_eq "extract error from json" \
    "something broke" \
    "$(extract_response "$TEMP_OUTPUT")"

# plain text (not json)
printf 'plain text output' > "$TEMP_OUTPUT"
assert_eq "extract plain text fallback" \
    "plain text output" \
    "$(extract_response "$TEMP_OUTPUT")"

# empty file
printf '' > "$TEMP_OUTPUT"
assert_eq "extract from empty file" \
    "no output" \
    "$(extract_response "$TEMP_OUTPUT")"

# nonexistent file
assert_eq "extract from missing file" \
    "no output" \
    "$(extract_response "/tmp/nonexistent-ralph-test-file")"

rm -f "$TEMP_OUTPUT"

echo ""
echo "=== Build prompt ==="

# create a mock progress file
TEMP_PROGRESS2="$(mktemp)"
echo "# Previous progress" > "$TEMP_PROGRESS2"
echo "Step 1 done" >> "$TEMP_PROGRESS2"

# test with custom prompt file
TEMP_PROMPT="$(mktemp)"
echo "Custom system prompt for agent." > "$TEMP_PROMPT"

# Note: build_prompt calls `room poll` which requires a running broker.
# We test the parts we can without a broker.

# Test with custom prompt (no room poll)
# These globals are used by build_prompt via the sourced ralph-room.sh
# shellcheck disable=SC2034
ROOM_ID="test-room"
# shellcheck disable=SC2034
USERNAME="test-user"
# shellcheck disable=SC2034
TOKEN="test-token"
prompt_output="$(build_prompt "test-room" "test-user" "test-token" "$TEMP_PROMPT" "$TEMP_PROGRESS2" "99")"

assert_contains "custom prompt is included" \
    "Custom system prompt" "$prompt_output"

assert_contains "progress file is included" \
    "Previous progress" "$prompt_output"

assert_contains "issue number is included" \
    "issue #99" "$prompt_output"

# Test default prompt (no custom file)
prompt_output="$(build_prompt "test-room" "test-user" "test-token" "" "$TEMP_PROGRESS2" "")"

assert_contains "default prompt has username" \
    "test-user" "$prompt_output"

assert_contains "default prompt has room id" \
    "test-room" "$prompt_output"

assert_contains "default prompt has token" \
    "test-token" "$prompt_output"

assert_contains "default prompt has room send command" \
    "room send" "$prompt_output"

assert_contains "default prompt has poll command" \
    "room poll" "$prompt_output"

assert_contains "no issue falls back to poll message" \
    "Poll the room" "$prompt_output"

# Test without progress file
prompt_no_progress="$(build_prompt "test-room" "test-user" "test-token" "" "/tmp/nonexistent-file" "10")"

# should NOT contain progress section
if echo "$prompt_no_progress" | grep -q "PROGRESS FROM PREVIOUS"; then
    FAIL=$((FAIL + 1))
    printf '  FAIL: prompt without progress file should not have progress section\n'
else
    PASS=$((PASS + 1))
    printf '  PASS: prompt without progress file omits progress section\n'
fi

rm -f "$TEMP_PROGRESS2" "$TEMP_PROMPT"

echo ""
echo "=== Dependency check ==="

# check_dependencies should succeed since we need claude, room, jq to run tests
if check_dependencies 2>/dev/null; then
    PASS=$((PASS + 1))
    printf '  PASS: dependency check passes with all deps available\n'
else
    # this is expected to fail if deps are missing in test env
    printf '  SKIP: dependency check (missing deps in test env)\n'
fi

echo ""
echo "=== Usage output ==="

usage_output="$(usage)"

assert_contains "usage has script name" \
    "ralph-room.sh" "$usage_output"

assert_contains "usage has --model" \
    "--model" "$usage_output"

assert_contains "usage has --issue" \
    "--issue" "$usage_output"

assert_contains "usage has --tmux" \
    "--tmux" "$usage_output"

assert_contains "usage has --dry-run" \
    "--dry-run" "$usage_output"

assert_contains "usage has progress file docs" \
    "room-progress" "$usage_output"

echo ""
echo "=== Parse args ==="

# parse_args sets globals — test in subshells to avoid pollution
if (
    # Reset defaults before parsing
    MODEL="opus"; ISSUE=""; USE_TMUX=false; MAX_ITER=50; COOLDOWN=5
    export CUSTOM_PROMPT=""; DRY_RUN=false; ADD_DIRS=()
    parse_args "myroom" "agent1" --model "sonnet" --issue "42" --max-iter 10 --cooldown 2
    [[ "$MODEL" == "sonnet" ]] || exit 1
    [[ "$ISSUE" == "42" ]] || exit 1
    [[ "$MAX_ITER" -eq 10 ]] || exit 1
    [[ "$COOLDOWN" -eq 2 ]] || exit 1
    [[ "$ROOM_ID" == "myroom" ]] || exit 1
    [[ "$USERNAME" == "agent1" ]] || exit 1
); then
    PASS=$((PASS + 1)); printf '  PASS: parse_args with all flags\n'
else
    FAIL=$((FAIL + 1)); printf '  FAIL: parse_args with all flags\n'
fi

if (
    MODEL="opus"; ISSUE=""; USE_TMUX=false; DRY_RUN=false; ADD_DIRS=()
    parse_args "room1" "user1" --tmux --dry-run
    [[ "$USE_TMUX" == "true" ]] || exit 1
    [[ "$DRY_RUN" == "true" ]] || exit 1
); then
    PASS=$((PASS + 1)); printf '  PASS: parse_args boolean flags\n'
else
    FAIL=$((FAIL + 1)); printf '  FAIL: parse_args boolean flags\n'
fi

if (
    MODEL="opus"; ADD_DIRS=()
    parse_args "room1" "user1" --add-dir "/tmp/dir1" --add-dir "/tmp/dir2"
    [[ "${#ADD_DIRS[@]}" -eq 2 ]] || exit 1
    [[ "${ADD_DIRS[0]}" == "/tmp/dir1" ]] || exit 1
    [[ "${ADD_DIRS[1]}" == "/tmp/dir2" ]] || exit 1
); then
    PASS=$((PASS + 1)); printf '  PASS: parse_args repeatable --add-dir\n'
else
    FAIL=$((FAIL + 1)); printf '  FAIL: parse_args repeatable --add-dir\n'
fi

if (
    MODEL="opus"
    parse_args "room1" "user1"
    [[ "$MODEL" == "opus" ]] || exit 1
    [[ "$ROOM_ID" == "room1" ]] || exit 1
); then
    PASS=$((PASS + 1)); printf '  PASS: parse_args defaults\n'
else
    FAIL=$((FAIL + 1)); printf '  FAIL: parse_args defaults\n'
fi

# Unknown option should fail
if (parse_args "room1" "user1" --unknown 2>/dev/null); then
    FAIL=$((FAIL + 1)); printf '  FAIL: parse_args unknown option should fail\n'
else
    PASS=$((PASS + 1)); printf '  PASS: parse_args unknown option rejected\n'
fi

echo ""
echo "=== Progress file truncation ==="

TEMP_TRUNC="$(mktemp)"
# Generate 100-line response (no trailing newline to avoid empty last line)
long_response="$(printf '%s\n' $(seq 1 100) | sed 's/^/line /')"
write_progress_file "$TEMP_TRUNC" 1 "99" "$long_response"

# Last 50 lines should be present (51-100)
if grep -q "line 100" "$TEMP_TRUNC"; then
    PASS=$((PASS + 1)); printf '  PASS: truncation keeps line 100\n'
else
    FAIL=$((FAIL + 1)); printf '  FAIL: truncation missing line 100\n'
fi

if grep -q "line 52" "$TEMP_TRUNC"; then
    PASS=$((PASS + 1)); printf '  PASS: truncation keeps line 52\n'
else
    FAIL=$((FAIL + 1)); printf '  FAIL: truncation missing line 52\n'
fi

# Line 1 should be truncated (not in last 50)
if grep -q "^line 1$" "$TEMP_TRUNC"; then
    FAIL=$((FAIL + 1)); printf '  FAIL: truncation should have removed line 1\n'
else
    PASS=$((PASS + 1)); printf '  PASS: truncation removed line 1\n'
fi

rm -f "$TEMP_TRUNC"

echo ""
echo "=== Context exhaustion edge cases ==="

assert_exit "case insensitive: CONTEXT LIMIT" \
    0 detect_context_exhaustion 1 "CONTEXT LIMIT EXCEEDED"

assert_exit "case insensitive: Context Window" \
    0 detect_context_exhaustion 1 "Context Window is full"

assert_exit "context_length pattern" \
    0 detect_context_exhaustion 1 "exceeds context length limit"

echo ""
echo "=== Token expiry edge cases ==="

assert_exit "case insensitive: INVALID TOKEN" \
    0 detect_token_expiry "Error: INVALID TOKEN"

assert_exit "case insensitive: Token Expired" \
    0 detect_token_expiry "Token Expired, please rejoin"

# --- summary ---
echo ""
echo "=============================="
TOTAL=$((PASS + FAIL))
printf 'Results: %d/%d passed' "$PASS" "$TOTAL"
if [[ "$FAIL" -gt 0 ]]; then
    printf ' (%d FAILED)' "$FAIL"
    echo ""
    exit 1
else
    echo ""
    exit 0
fi
