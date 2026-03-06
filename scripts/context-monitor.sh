#!/usr/bin/env bash
# context-monitor.sh — Token usage monitoring for ralph-room.sh
#
# Source this file in ralph-room.sh to get context monitoring functions.
# Parses --output-format json output from claude -p and detects when
# the session is approaching the model's context window limit.
#
# Usage:
#   source scripts/context-monitor.sh
#
#   output=$(claude -p --output-format json "$prompt")
#   tokens=$(parse_usage "$output")
#   if should_restart "$tokens"; then
#     log_usage "$tokens" "$progress_file"
#     # restart with fresh context
#   fi
#
# Environment variables:
#   CONTEXT_LIMIT       — model context window size (default: 200000)
#   CONTEXT_THRESHOLD   — restart threshold as percentage (default: 80)
#   CONTEXT_LOG_FILE    — optional separate log file for usage history
#
# Dependencies: jq

# Default model context limits (tokens).
# Override with CONTEXT_LIMIT env var for custom models.
readonly DEFAULT_CONTEXT_LIMIT=200000
readonly DEFAULT_CONTEXT_THRESHOLD=80

# ── Public API ──────────────────────────────────────────────────────

# parse_usage <json_output>
#
# Extract input_tokens from claude -p --output-format json output.
# Tries multiple JSON paths to handle format variations:
#   .usage.input_tokens
#   .result.usage.input_tokens
#   .statistics.input_tokens
#
# Prints the token count to stdout. Prints 0 if not found.
parse_usage() {
  local json="$1"
  if [ -z "$json" ]; then
    printf '0'
    return 0
  fi

  local tokens
  tokens=$(printf '%s' "$json" | jq -r '
    if .usage.input_tokens? then .usage.input_tokens
    elif (.result | type) == "object" and .result.usage.input_tokens? then .result.usage.input_tokens
    elif .statistics.input_tokens? then .statistics.input_tokens
    else 0 end
  ' 2>/dev/null)

  if [ -z "$tokens" ] || [ "$tokens" = "null" ]; then
    tokens=0
  fi

  printf '%s' "$tokens"
}

# parse_output_tokens <json_output>
#
# Extract output_tokens from claude -p --output-format json output.
# Same path-probing strategy as parse_usage.
parse_output_tokens() {
  local json="$1"
  if [ -z "$json" ]; then
    printf '0'
    return 0
  fi

  local tokens
  tokens=$(printf '%s' "$json" | jq -r '
    if .usage.output_tokens? then .usage.output_tokens
    elif (.result | type) == "object" and .result.usage.output_tokens? then .result.usage.output_tokens
    elif .statistics.output_tokens? then .statistics.output_tokens
    else 0 end
  ' 2>/dev/null)

  if [ -z "$tokens" ] || [ "$tokens" = "null" ]; then
    tokens=0
  fi

  printf '%s' "$tokens"
}

# parse_cost <json_output>
#
# Extract total cost (USD) if present. Returns "0" if not found.
parse_cost() {
  local json="$1"
  if [ -z "$json" ]; then
    printf '0'
    return 0
  fi

  local cost
  cost=$(printf '%s' "$json" | jq -r '
    if .usage.total_cost? then .usage.total_cost
    elif (.result | type) == "object" and .result.usage.total_cost? then .result.usage.total_cost
    elif .cost_usd? then .cost_usd
    elif .total_cost? then .total_cost
    else 0 end
  ' 2>/dev/null)

  if [ -z "$cost" ] || [ "$cost" = "null" ]; then
    cost=0
  fi

  printf '%s' "$cost"
}

# get_context_limit
#
# Return the effective context window size.
# Uses CONTEXT_LIMIT env var if set, otherwise DEFAULT_CONTEXT_LIMIT.
get_context_limit() {
  printf '%s' "${CONTEXT_LIMIT:-$DEFAULT_CONTEXT_LIMIT}"
}

# get_threshold_tokens
#
# Return the token count at which a restart should be triggered.
# Computed as (limit * threshold_pct / 100).
get_threshold_tokens() {
  local limit="${CONTEXT_LIMIT:-$DEFAULT_CONTEXT_LIMIT}"
  local pct="${CONTEXT_THRESHOLD:-$DEFAULT_CONTEXT_THRESHOLD}"
  printf '%s' $(( limit * pct / 100 ))
}

# should_restart <input_tokens>
#
# Returns 0 (true) if input_tokens >= threshold, 1 (false) otherwise.
# Use in conditionals: if should_restart "$tokens"; then ...
should_restart() {
  local tokens="${1:-0}"
  local threshold
  threshold=$(get_threshold_tokens)

  if [ "$tokens" -ge "$threshold" ] 2>/dev/null; then
    return 0
  fi
  return 1
}

# context_usage_pct <input_tokens>
#
# Print the percentage of context window used (integer).
context_usage_pct() {
  local tokens="${1:-0}"
  local limit
  limit=$(get_context_limit)

  if [ "$limit" -eq 0 ] 2>/dev/null; then
    printf '0'
    return 0
  fi

  printf '%s' $(( tokens * 100 / limit ))
}

# log_usage <input_tokens> <progress_file> [output_tokens] [iteration]
#
# Append a usage entry to the progress file's Context Usage section.
# Creates the section if it doesn't exist.
log_usage() {
  local input_tokens="${1:-0}"
  local progress_file="$2"
  local output_tokens="${3:-0}"
  local iteration="${4:-}"

  if [ -z "$progress_file" ]; then
    return 1
  fi

  local pct
  pct=$(context_usage_pct "$input_tokens")
  local threshold
  threshold=$(get_threshold_tokens)
  local limit
  limit=$(get_context_limit)
  local ts
  ts=$(date -u '+%Y-%m-%dT%H:%M:%SZ')

  local iter_str=""
  if [ -n "$iteration" ]; then
    iter_str=" iter=$iteration"
  fi

  local restart_note=""
  if should_restart "$input_tokens"; then
    restart_note=" **RESTART TRIGGERED**"
  fi

  local entry="- ${ts}:${iter_str} input=${input_tokens}/${limit} (${pct}%) output=${output_tokens} threshold=${threshold}${restart_note}"

  # Append to Context Usage section, creating it if needed
  if [ -f "$progress_file" ] && grep -q '## Context Usage' "$progress_file" 2>/dev/null; then
    printf '%s\n' "$entry" >> "$progress_file"
  else
    printf '\n## Context Usage\n%s\n' "$entry" >> "$progress_file"
  fi

  # Also log to separate file if configured
  if [ -n "${CONTEXT_LOG_FILE:-}" ]; then
    printf '%s\n' "$entry" >> "$CONTEXT_LOG_FILE"
  fi
}

# format_usage_summary <input_tokens> [output_tokens]
#
# Print a human-readable one-line usage summary to stdout.
format_usage_summary() {
  local input_tokens="${1:-0}"
  local output_tokens="${2:-0}"
  local pct
  pct=$(context_usage_pct "$input_tokens")
  local limit
  limit=$(get_context_limit)
  local threshold
  threshold=$(get_threshold_tokens)

  printf 'context: %s/%s (%s%%) threshold: %s' \
    "$input_tokens" "$limit" "$pct" "$threshold"

  if should_restart "$input_tokens"; then
    printf ' [RESTART]'
  fi
  printf '\n'
}
