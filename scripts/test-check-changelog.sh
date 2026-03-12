#!/usr/bin/env bash
# test-check-changelog.sh — Tests for check-changelog.sh
#
# Run: bash scripts/test-check-changelog.sh
# Creates temporary git repos to exercise the changelog check in isolation.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK_SCRIPT="$SCRIPT_DIR/check-changelog.sh"

PASS=0
FAIL=0
TOTAL=0

assert_exit() {
  local label="$1" expected_exit="$2"
  shift 2
  TOTAL=$((TOTAL + 1))
  local actual_exit=0
  "$@" > /dev/null 2>&1 || actual_exit=$?
  if [ "$expected_exit" -eq "$actual_exit" ]; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    printf 'FAIL: %s — expected exit %d, got %d\n' "$label" "$expected_exit" "$actual_exit" >&2
  fi
}

# ── Setup temp repo ──────────────────────────────────────────────────

TMPDIR_ROOT=$(mktemp -d)
trap 'rm -rf "$TMPDIR_ROOT"' EXIT

setup_repo() {
  local repo_dir="$TMPDIR_ROOT/repo-$TOTAL"
  mkdir -p "$repo_dir"
  cd "$repo_dir"
  git init -q
  git config user.name "test"
  git config user.email "test@test"
  git checkout -q -b master

  # Create initial CHANGELOG.md
  cat > CHANGELOG.md <<'HEREDOC'
# Changelog

## [Unreleased]

## [1.0.0] - 2026-01-01

### Added

- Initial release.
HEREDOC

  # Create per-crate changelog
  mkdir -p crates/room-cli
  cat > crates/room-cli/CHANGELOG.md <<'HEREDOC'
# Changelog

## [Unreleased]

## [1.0.0] - 2026-01-01

### Added

- Initial release.
HEREDOC

  git add -A
  git commit -q -m "initial commit"

  # Create a "remote" ref for the script to diff against
  git branch origin/master master

  # Work on a feature branch
  git checkout -q -b feature
}

# ── Test: no changelog changes → exit 1 ──────────────────────────────

setup_repo
echo "some code" > src.rs
git add src.rs
git commit -q -m "add code without changelog"
assert_exit "no changelog changes exits 1" 1 bash "$CHECK_SCRIPT" origin/master

# ── Test: root CHANGELOG changed → exit 0 ────────────────────────────

setup_repo
sed -i.bak 's/## \[Unreleased\]/## [Unreleased]\n\n### Added\n\n- New feature. (#42)/' CHANGELOG.md
rm -f CHANGELOG.md.bak
git add CHANGELOG.md
git commit -q -m "add changelog entry"
assert_exit "root CHANGELOG changed exits 0" 0 bash "$CHECK_SCRIPT" origin/master

# ── Test: per-crate CHANGELOG changed → exit 0 ──────────────────────

setup_repo
sed -i.bak 's/## \[Unreleased\]/## [Unreleased]\n\n### Fixed\n\n- Bug fix. (#43)/' crates/room-cli/CHANGELOG.md
rm -f crates/room-cli/CHANGELOG.md.bak
git add crates/room-cli/CHANGELOG.md
git commit -q -m "add per-crate changelog entry"
assert_exit "per-crate CHANGELOG changed exits 0" 0 bash "$CHECK_SCRIPT" origin/master

# ── Test: [skip changelog] in commit message → exit 0 ────────────────

setup_repo
echo "some code" > src.rs
git add src.rs
git commit -q -m "docs: update readme [skip changelog]"
assert_exit "[skip changelog] marker exits 0" 0 bash "$CHECK_SCRIPT" origin/master

# ── Test: [skip changelog] case insensitive ──────────────────────────

setup_repo
echo "some code" > src.rs
git add src.rs
git commit -q -m "ci: update workflow [Skip Changelog]"
assert_exit "[Skip Changelog] case insensitive exits 0" 0 bash "$CHECK_SCRIPT" origin/master

# ── Test: multiple commits, one has changelog → exit 0 ───────────────

setup_repo
echo "code1" > a.rs
git add a.rs
git commit -q -m "first commit without changelog"
sed -i.bak 's/## \[Unreleased\]/## [Unreleased]\n\n### Added\n\n- Feature A. (#44)/' CHANGELOG.md
rm -f CHANGELOG.md.bak
git add CHANGELOG.md
git commit -q -m "add changelog in second commit"
assert_exit "changelog in one of multiple commits exits 0" 0 bash "$CHECK_SCRIPT" origin/master

# ── Test: only diff header lines (no real additions) → exit 1 ────────

setup_repo
# Remove a line from CHANGELOG (deletion only, no addition)
sed -i.bak '/Initial release/d' CHANGELOG.md
rm -f CHANGELOG.md.bak
git add CHANGELOG.md
git commit -q -m "remove line from changelog"
assert_exit "deletion-only changelog change exits 1" 1 bash "$CHECK_SCRIPT" origin/master

# ── Results ──────────────────────────────────────────────────────────

echo ""
echo "check-changelog tests: $PASS passed, $FAIL failed, $TOTAL total"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
