#!/usr/bin/env bash
# scripts/check-changelog.sh — CI gate: every PR must update a CHANGELOG.md
#
# Verifies that the PR branch adds at least one line to an [Unreleased]
# section in any CHANGELOG.md file compared to the base branch.
#
# Bypass: include "[skip changelog]" in a commit message on the PR branch.
# Use for docs-only, CI-only, or chore changes that don't need a log entry.
#
# Usage:
#   bash scripts/check-changelog.sh [base-ref]
#   base-ref defaults to origin/master

set -euo pipefail

BASE_REF="${1:-origin/master}"

# ---------------------------------------------------------------------------
# Skip check if any commit in the PR carries [skip changelog]
# ---------------------------------------------------------------------------
if git log "$BASE_REF"..HEAD --format=%s | grep -qi '\[skip changelog\]'; then
    echo "Changelog check skipped via [skip changelog] marker."
    exit 0
fi

# ---------------------------------------------------------------------------
# Look for added lines (^+) in any CHANGELOG.md, excluding diff headers (^+++)
# ---------------------------------------------------------------------------
ADDED_LINES=$(git diff "$BASE_REF"...HEAD -- '*/CHANGELOG.md' 'CHANGELOG.md' \
    | grep '^+[^+]' \
    | grep -v '^+++' || true)

if [ -z "$ADDED_LINES" ]; then
    echo "ERROR: No changelog entries found."
    echo ""
    echo "Every PR must add at least one line to the [Unreleased] section"
    echo "of the relevant crate's CHANGELOG.md."
    echo ""
    echo "Which file to update:"
    echo "  crates/room-cli/CHANGELOG.md       — room-cli changes"
    echo "  crates/room-protocol/CHANGELOG.md   — protocol changes"
    echo "  crates/room-ralph/CHANGELOG.md      — room-ralph changes"
    echo ""
    echo "Format: one bullet point per PR under the appropriate heading"
    echo "(Added, Changed, Fixed, Removed) in [Unreleased]."
    echo ""
    echo "To skip: add [skip changelog] to any commit message on this branch."
    exit 1
fi

echo "Changelog entries found:"
echo "$ADDED_LINES"
