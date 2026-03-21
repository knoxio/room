#!/usr/bin/env bash
# pre-push.sh — Run before every push to ensure CI will pass.
# Install as a git hook:  ln -sf ../../scripts/pre-push.sh .git/hooks/pre-push
# Or run manually:        bash scripts/pre-push.sh
#
# Only runs fast checks locally (check, fmt, clippy, unit tests).
# Full integration tests run in CI.
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
RESET='\033[0m'

step() {
  printf "${BOLD}[pre-push]${RESET} %s\n" "$1"
}

fail() {
  printf "${RED}[pre-push] FAILED:${RESET} %s\n" "$1" >&2
  exit 1
}

pass() {
  printf "${GREEN}[pre-push] PASSED:${RESET} %s\n" "$1"
}

info() {
  printf "${YELLOW}[pre-push]${RESET} %s\n" "$1"
}

# Order matters — each step can invalidate the previous one.
# 1. cargo check catches syntax/type errors and conflict markers first.
# 2. cargo fmt reformats; if it changes anything, you have uncommitted diffs.
# 3. cargo clippy catches lint issues.
# 4. Unit tests only (--lib) — integration tests run in CI.

step "cargo check"
cargo check 2>&1 || fail "cargo check"
pass "cargo check"

step "cargo fmt --check"
cargo fmt -- --check 2>&1 || fail "cargo fmt (run 'cargo fmt' to fix)"
pass "cargo fmt"

step "cargo clippy -- -D warnings"
cargo clippy -- -D warnings 2>&1 || fail "cargo clippy"
pass "cargo clippy"

step "cargo test --lib (unit tests only)"
cargo test --lib 2>&1 || fail "cargo test --lib"
pass "cargo test --lib"

printf '\n%s%s[pre-push] All checks passed.%s\n' "$GREEN" "$BOLD" "$RESET"
