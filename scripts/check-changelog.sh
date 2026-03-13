#!/usr/bin/env bash
# scripts/check-changelog.sh — CI gate: every PR must update a CHANGELOG.md
#
# Verifies that the PR branch adds at least one line to an [Unreleased]
# section in any CHANGELOG.md file compared to the base branch.
#
# Bypass: add the "skip-changelog" label to the PR on GitHub.
# Use for docs-only, CI-only, or chore changes that don't need a log entry.
#
# In CI, set PR_NUMBER env var so the script can query labels via `gh`.
# Locally, set SKIP_CHANGELOG=1 to bypass.
#
# Usage:
#   bash scripts/check-changelog.sh [base-ref]
#   base-ref defaults to origin/master

set -euo pipefail

BASE_REF="${1:-origin/master}"

# ---------------------------------------------------------------------------
# Skip check if the PR has the "skip-changelog" label (CI) or SKIP_CHANGELOG
# env var is set (local override)
# ---------------------------------------------------------------------------
if [ "${SKIP_CHANGELOG:-}" = "1" ]; then
    echo "Changelog check skipped via SKIP_CHANGELOG env var."
    exit 0
fi

if [ -n "${PR_NUMBER:-}" ]; then
    if gh pr view "$PR_NUMBER" --json labels --jq '.labels[].name' 2>/dev/null \
        | grep -qi '^skip-changelog$'; then
        echo "Changelog check skipped via skip-changelog label on PR #$PR_NUMBER."
        exit 0
    fi
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
    echo "To skip: add the 'skip-changelog' label to the PR on GitHub."
    exit 1
fi

echo "Changelog entries found:"
echo "$ADDED_LINES"
