#!/usr/bin/env bash
# pre-push.sh — Run before every push to ensure CI will pass.
# Install as a git hook:  ln -sf ../../scripts/pre-push.sh .git/hooks/pre-push
# Or run manually:        bash scripts/pre-push.sh
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
# 4. cargo test — crate-scoped when possible, full suite as fallback.

step "cargo check"
cargo check 2>&1 || fail "cargo check"
pass "cargo check"

step "cargo fmt --check"
cargo fmt -- --check 2>&1 || fail "cargo fmt (run 'cargo fmt' to fix)"
pass "cargo fmt"

step "cargo clippy -- -D warnings"
cargo clippy -- -D warnings 2>&1 || fail "cargo clippy"
pass "cargo clippy"

# Detect changed crates for scoped testing.
# Falls back to full suite on master, cross-crate changes, or root Cargo.toml changes.
CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "unknown")

run_full_suite() {
  step "cargo test (full suite)"
  cargo test 2>&1 || fail "cargo test"
  pass "cargo test (full suite)"
}

if [ "$CURRENT_BRANCH" = "master" ] || [ "$CURRENT_BRANCH" = "main" ]; then
  info "on $CURRENT_BRANCH — running full test suite"
  run_full_suite
else
  # Find merge base with master to detect changed files.
  MERGE_BASE=$(git merge-base origin/master HEAD 2>/dev/null || echo "")
  if [ -z "$MERGE_BASE" ]; then
    info "cannot determine merge base — running full test suite"
    run_full_suite
  else
    # Check for root Cargo.toml changes (workspace-level).
    ROOT_CHANGED=$(git diff --name-only "$MERGE_BASE"..HEAD -- Cargo.toml | wc -l)

    # Extract unique crate names from changed files under crates/.
    CHANGED_CRATES=$(git diff --name-only "$MERGE_BASE"..HEAD -- 'crates/' \
      | sed -n 's|^crates/\([^/]*\)/.*|\1|p' \
      | sort -u)

    CRATE_COUNT=$(echo "$CHANGED_CRATES" | grep -c . || true)

    if [ "$ROOT_CHANGED" -gt 0 ]; then
      info "root Cargo.toml changed — running full test suite"
      run_full_suite
    elif [ "$CRATE_COUNT" -eq 0 ]; then
      info "no crate changes detected — skipping tests"
    elif [ "$CRATE_COUNT" -gt 3 ]; then
      info "$CRATE_COUNT crates changed — running full test suite"
      run_full_suite
    else
      for crate in $CHANGED_CRATES; do
        step "cargo test -p $crate"
        cargo test -p "$crate" 2>&1 || fail "cargo test -p $crate"
        pass "cargo test -p $crate"
      done
    fi
  fi
fi

printf '\n%s%s[pre-push] All checks passed.%s\n' "$GREEN" "$BOLD" "$RESET"
