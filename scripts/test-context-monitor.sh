#!/usr/bin/env bash
# test-context-monitor.sh — Tests for context-monitor.sh functions
#
# Run: bash scripts/test-context-monitor.sh
# All tests use mock JSON data — no live claude CLI required.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=context-monitor.sh
source "$SCRIPT_DIR/context-monitor.sh"

PASS=0
FAIL=0
TOTAL=0

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  TOTAL=$((TOTAL + 1))
  if [ "$expected" = "$actual" ]; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    printf 'FAIL: %s — expected "%s", got "%s"\n' "$label" "$expected" "$actual" >&2
  fi
}

assert_exit() {
  local label="$1" expected_exit="$2"
  shift 2
  TOTAL=$((TOTAL + 1))
  local actual_exit=0
  "$@" || actual_exit=$?
  if [ "$expected_exit" -eq "$actual_exit" ]; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    printf 'FAIL: %s — expected exit %d, got %d\n' "$label" "$expected_exit" "$actual_exit" >&2
  fi
}

# ── Mock JSON payloads ──────────────────────────────────────────────

# Format 1: usage at top level
JSON_TOP='{"result":"hello","usage":{"input_tokens":150000,"output_tokens":2000}}'

# Format 2: usage nested under result
JSON_NESTED='{"result":{"content":"hello","usage":{"input_tokens":95000,"output_tokens":1500}}}'

# Format 3: usage under statistics
JSON_STATS='{"result":"hello","statistics":{"input_tokens":180000,"output_tokens":3000}}'

# Format 4: no usage info at all
JSON_NONE='{"result":"hello"}'

# Format 5: usage with cost
JSON_COST='{"result":"hello","usage":{"input_tokens":100000,"output_tokens":2000,"total_cost":0.42}}'

# ── parse_usage tests ──────────────────────────────────────────────

assert_eq "parse_usage: top-level usage" \
  "150000" "$(parse_usage "$JSON_TOP")"

assert_eq "parse_usage: nested usage" \
  "95000" "$(parse_usage "$JSON_NESTED")"

assert_eq "parse_usage: statistics path" \
  "180000" "$(parse_usage "$JSON_STATS")"

assert_eq "parse_usage: no usage returns 0" \
  "0" "$(parse_usage "$JSON_NONE")"

assert_eq "parse_usage: empty string returns 0" \
  "0" "$(parse_usage "")"

# ── parse_output_tokens tests ──────────────────────────────────────

assert_eq "parse_output_tokens: top-level" \
  "2000" "$(parse_output_tokens "$JSON_TOP")"

assert_eq "parse_output_tokens: nested" \
  "1500" "$(parse_output_tokens "$JSON_NESTED")"

assert_eq "parse_output_tokens: none returns 0" \
  "0" "$(parse_output_tokens "$JSON_NONE")"

# ── parse_cost tests ───────────────────────────────────────────────

assert_eq "parse_cost: with cost" \
  "0.42" "$(parse_cost "$JSON_COST")"

assert_eq "parse_cost: without cost" \
  "0" "$(parse_cost "$JSON_TOP")"

# ── get_context_limit tests ────────────────────────────────────────

unset CONTEXT_LIMIT 2>/dev/null || true
assert_eq "get_context_limit: default" \
  "200000" "$(get_context_limit)"

CONTEXT_LIMIT=128000
assert_eq "get_context_limit: custom" \
  "128000" "$(get_context_limit)"
unset CONTEXT_LIMIT

# ── get_threshold_tokens tests ─────────────────────────────────────

unset CONTEXT_LIMIT CONTEXT_THRESHOLD 2>/dev/null || true
assert_eq "get_threshold_tokens: default (200K * 80%)" \
  "160000" "$(get_threshold_tokens)"

CONTEXT_LIMIT=100000
assert_eq "get_threshold_tokens: custom limit (100K * 80%)" \
  "80000" "$(get_threshold_tokens)"

CONTEXT_THRESHOLD=90
assert_eq "get_threshold_tokens: custom threshold (100K * 90%)" \
  "90000" "$(get_threshold_tokens)"
unset CONTEXT_LIMIT CONTEXT_THRESHOLD

# ── should_restart tests ───────────────────────────────────────────

unset CONTEXT_LIMIT CONTEXT_THRESHOLD 2>/dev/null || true

assert_exit "should_restart: under threshold (100K < 160K)" \
  1 should_restart 100000

assert_exit "should_restart: at threshold (160K = 160K)" \
  0 should_restart 160000

assert_exit "should_restart: over threshold (180K > 160K)" \
  0 should_restart 180000

assert_exit "should_restart: zero tokens" \
  1 should_restart 0

assert_exit "should_restart: empty defaults to 0" \
  1 should_restart ""

# Custom threshold
CONTEXT_LIMIT=100000
CONTEXT_THRESHOLD=50
assert_exit "should_restart: custom 50K threshold, 60K tokens" \
  0 should_restart 60000

assert_exit "should_restart: custom 50K threshold, 40K tokens" \
  1 should_restart 40000
unset CONTEXT_LIMIT CONTEXT_THRESHOLD

# ── context_usage_pct tests ────────────────────────────────────────

unset CONTEXT_LIMIT 2>/dev/null || true
assert_eq "context_usage_pct: 50%" \
  "50" "$(context_usage_pct 100000)"

assert_eq "context_usage_pct: 75%" \
  "75" "$(context_usage_pct 150000)"

assert_eq "context_usage_pct: 100%" \
  "100" "$(context_usage_pct 200000)"

assert_eq "context_usage_pct: 0 tokens" \
  "0" "$(context_usage_pct 0)"

CONTEXT_LIMIT=100000
assert_eq "context_usage_pct: custom limit 60%" \
  "60" "$(context_usage_pct 60000)"
unset CONTEXT_LIMIT

# ── log_usage tests ────────────────────────────────────────────────

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

# Test: creates Context Usage section in new file
progress="$tmpdir/progress.md"
log_usage 150000 "$progress" 2000 3
assert_eq "log_usage: creates file" "0" "$([ -f "$progress" ] && printf '0' || printf '1')"

content=$(cat "$progress")
assert_eq "log_usage: has Context Usage header" \
  "1" "$(printf '%s' "$content" | grep -c '## Context Usage' | tr -d ' ')"

assert_eq "log_usage: has input tokens" \
  "1" "$(printf '%s' "$content" | grep -c 'input=150000' | tr -d ' ')"

assert_eq "log_usage: has output tokens" \
  "1" "$(printf '%s' "$content" | grep -c 'output=2000' | tr -d ' ')"

assert_eq "log_usage: has iteration" \
  "1" "$(printf '%s' "$content" | grep -c 'iter=3' | tr -d ' ')"

# Test: appends to existing section
log_usage 170000 "$progress" 2500 4
line_count=$(grep -c 'input=' "$progress" | tr -d ' ')
assert_eq "log_usage: appends second entry" \
  "2" "$line_count"

# Test: restart annotation
unset CONTEXT_LIMIT CONTEXT_THRESHOLD 2>/dev/null || true
restart_progress="$tmpdir/restart.md"
log_usage 180000 "$restart_progress" 3000 5
assert_eq "log_usage: restart annotation at 180K" \
  "1" "$(grep -c 'RESTART TRIGGERED' "$restart_progress" | tr -d ' ')"

# Test: no file returns error
assert_exit "log_usage: empty path returns 1" \
  1 log_usage 1000 ""

# Test: CONTEXT_LOG_FILE
log_file="$tmpdir/usage.log"
CONTEXT_LOG_FILE="$log_file"
log_progress="$tmpdir/log-test.md"
log_usage 120000 "$log_progress" 1000
assert_eq "log_usage: writes to CONTEXT_LOG_FILE" \
  "0" "$([ -f "$log_file" ] && printf '0' || printf '1')"
assert_eq "log_usage: log file has entry" \
  "1" "$(grep -c 'input=120000' "$log_file" | tr -d ' ')"
unset CONTEXT_LOG_FILE

# ── format_usage_summary tests ─────────────────────────────────────

unset CONTEXT_LIMIT CONTEXT_THRESHOLD 2>/dev/null || true
summary=$(format_usage_summary 100000 2000)
assert_eq "format_usage_summary: contains tokens" \
  "1" "$(printf '%s' "$summary" | grep -c '100000/200000' | tr -d ' ')"

assert_eq "format_usage_summary: contains percentage" \
  "1" "$(printf '%s' "$summary" | grep -c '50%' | tr -d ' ')"

assert_eq "format_usage_summary: no restart marker under threshold" \
  "0" "$(printf '%s' "$summary" | grep -c 'RESTART' | tr -d ' ')"

restart_summary=$(format_usage_summary 180000 3000)
assert_eq "format_usage_summary: restart marker over threshold" \
  "1" "$(printf '%s' "$restart_summary" | grep -c 'RESTART' | tr -d ' ')"

# ── Results ─────────────────────────────────────────────────────────

printf '\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n'
printf 'context-monitor tests: %d passed, %d failed, %d total\n' "$PASS" "$FAIL" "$TOTAL"
printf '━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n'

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
