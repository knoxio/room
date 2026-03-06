#!/usr/bin/env bash
# ralph-room.sh — Run a Claude agent headlessly with auto-restart on context death.
#
# Implements the "ralph loop" pattern (outer-loop variant): spawns fresh `claude -p`
# instances in a loop, feeding room context and progress files on each restart.
# Context exhaustion is not task death — progress persists in files.
#
# Design references:
#   - snarktank/ralph: PRD-based autonomous loop
#   - anthropics/claude-code ralph-wiggum plugin: stop-hook in-session loop
#   - frankbria/ralph-claude-code: rate limiting + exit detection
#
# This script takes the outer-loop approach (vs the stop-hook approach) because:
#   1. Each iteration gets a clean context window — no accumulated drift
#   2. Progress files are the single source of truth across restarts
#   3. Room coordination (poll/send) happens at the wrapper level, not inside claude
#   4. Crash recovery is trivial — the loop just restarts
#
# Usage:
#   bash scripts/ralph-room.sh <room-id> <username> [options]
#   bash scripts/ralph-room.sh myroom saphire --model opus --issue 42
#   bash scripts/ralph-room.sh myroom saphire --tmux
#
# Options:
#   --model <model>       Claude model (default: opus)
#   --issue <number>      GitHub issue — enables progress file persistence
#   --tmux                Run in a detached tmux session (ralph-<username>)
#   --max-iter <n>        Max iterations before stopping (default: 50, 0 = unlimited)
#   --cooldown <secs>     Seconds between iterations (default: 5)
#   --prompt <file>       Custom system prompt file (replaces built-in prompt)
#   --add-dir <dir>       Additional dir for claude --add-dir (repeatable)
#   --dry-run             Print the prompt that would be sent, then exit
#   -h, --help            Show this help
#
# Dependencies: claude, room, jq
# Optional: tmux (for --tmux mode)
#
# Testability:
#   Source this script with RALPH_ROOM_SOURCED=1 to load functions without running main.
#   Example: RALPH_ROOM_SOURCED=1 source scripts/ralph-room.sh
#   Then call individual functions: build_prompt, detect_context_exhaustion, etc.
set -euo pipefail

# --- defaults ---
MODEL="opus"
ISSUE=""
USE_TMUX=false
MAX_ITER=50
COOLDOWN=5
CUSTOM_PROMPT=""
DRY_RUN=false
ADD_DIRS=()
RUNNING=true
ITER=0
TOKEN=""

# --- usage ---
usage() {
    cat <<'USAGE'
ralph-room.sh — Run a Claude agent headlessly with auto-restart on context death.

Usage:
  bash scripts/ralph-room.sh <room-id> <username> [options]

Examples:
  bash scripts/ralph-room.sh myroom saphire --model opus --issue 42
  bash scripts/ralph-room.sh myroom saphire --tmux
  bash scripts/ralph-room.sh myroom saphire --dry-run --issue 42

Options:
  --model <model>       Claude model (default: opus)
  --issue <number>      GitHub issue — enables progress file persistence
  --tmux                Run in a detached tmux session (ralph-<username>)
  --max-iter <n>        Max iterations before stopping (default: 50, 0 = unlimited)
  --cooldown <secs>     Seconds between iterations (default: 5)
  --prompt <file>       Custom system prompt file (replaces built-in prompt)
  --add-dir <dir>       Additional dir for claude --add-dir (repeatable)
  --dry-run             Print the prompt that would be sent, then exit
  -h, --help            Show this help

Progress files:
  /tmp/room-progress-<issue>.md     Written on context exhaustion, read on restart
  /tmp/ralph-room-<username>.log    Timestamped log of all wrapper activity

Dependencies: claude, room, jq
Optional: tmux (for --tmux mode)
USAGE
}

# --- path helpers (pure functions, testable) ---

# Returns the path to the progress file for an issue or username.
progress_file_path() {
    local issue="${1:-}"
    local username="${2:-}"
    echo "/tmp/room-progress-${issue:-${username}}.md"
}

# Returns the path to the ralph log file.
log_file_path() {
    local username="$1"
    echo "/tmp/ralph-room-${username}.log"
}

# Returns the path to the room token file.
token_file_path() {
    local room_id="$1"
    local username="$2"
    echo "/tmp/room-${room_id}-${username}.token"
}

# --- logging ---
log() {
    local ts
    ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '[%s] [ralph] %s\n' "$ts" "$*" | tee -a "${LOG_FILE:-/dev/null}"
}

# --- dependency check ---
check_dependencies() {
    local missing=()
    for cmd in claude room jq; do
        if ! command -v "$cmd" &>/dev/null; then
            missing+=("$cmd")
        fi
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing dependencies: ${missing[*]}" >&2
        return 1
    fi
    return 0
}

# --- join room ---
# Joins the room and sets the TOKEN variable. Falls back to cached token file.
# Args: $1=room_id, $2=username, $3=token_file
# Sets: TOKEN (global)
join_room() {
    local room_id="$1"
    local username="$2"
    local token_file="$3"
    log "joining room $room_id as $username"
    local join_output
    join_output="$(room join "$room_id" "$username" 2>&1)" || {
        log "join failed: $join_output"
        if [[ -f "$token_file" ]]; then
            TOKEN="$(python3 -c "import json; print(json.load(open('$token_file'))['token'])")"
            log "using cached token from $token_file"
            return 0
        fi
        return 1
    }
    TOKEN="$(echo "$join_output" | python3 -c "import sys,json; print(json.load(sys.stdin)['token'])")"
    log "joined, token: ${TOKEN:0:8}..."
}

# --- detect context exhaustion ---
# Checks claude's output and exit code for signs of context window exhaustion.
# Args: $1=exit_code, $2=response_text
# Returns: 0 if context exhausted, 1 otherwise
detect_context_exhaustion() {
    local exit_code="$1"
    local response="$2"
    if [[ "$exit_code" -eq 0 ]]; then
        return 1
    fi
    if echo "$response" | grep -qi \
        -e "context.*limit" \
        -e "context.*window" \
        -e "context.*exhaust" \
        -e "token.*limit" \
        -e "conversation.*too.*long" \
        -e "maximum.*context" \
        -e "context.*length"; then
        return 0
    fi
    return 1
}

# --- detect token expiry ---
# Checks if the response suggests the auth token is invalid.
# Args: $1=response_text
# Returns: 0 if token appears expired, 1 otherwise
detect_token_expiry() {
    local response="$1"
    if echo "$response" | grep -qi \
        -e "invalid.*token" \
        -e "unauthorized" \
        -e "token.*expired" \
        -e "token.*invalid"; then
        return 0
    fi
    return 1
}

# --- write progress file ---
# Writes a structured progress file on context exhaustion.
# Args: $1=progress_file, $2=iteration, $3=issue, $4=response
write_progress_file() {
    local progress_file="$1"
    local iteration="$2"
    local issue="$3"
    local response="$4"
    {
        echo "# Progress — $(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo ""
        echo "## Metadata"
        echo "- Iteration: $iteration"
        echo "- Issue: ${issue:-unassigned}"
        echo "- Reason: context exhaustion"
        echo ""
        echo "## Last output (truncated)"
        echo '```'
        echo "$response" | tail -50
        echo '```'
        echo ""
        echo "## Status"
        echo "Context exhausted. Restarting with fresh context."
    } > "$progress_file"
}

# --- build prompt ---
# Builds the prompt for claude -p from system context, progress file, and room messages.
# Args: $1=room_id, $2=username, $3=token, $4=custom_prompt_file, $5=progress_file, $6=issue
build_prompt() {
    local room_id="$1"
    local username="$2"
    local token="$3"
    local custom_prompt_file="$4"
    local progress_file="$5"
    local issue="$6"
    local prompt=""

    # system context
    if [[ -n "$custom_prompt_file" && -f "$custom_prompt_file" ]]; then
        prompt+="$(cat "$custom_prompt_file")"
    else
        prompt+="You are $username, an autonomous agent in room $room_id."
        prompt+=" You communicate via the room CLI. Your token is $token."
        prompt+=$'\n\n'
        prompt+="Commands available:"
        prompt+=$'\n'
        prompt+="  room send $room_id -t $token '<message>'  -- send a message"
        prompt+=$'\n'
        prompt+="  room poll $room_id -t $token              -- check for new messages"
        prompt+=$'\n'
        prompt+="  room watch $room_id -t $token --interval 2 -- block until a message arrives"
        prompt+=$'\n\n'
        prompt+="Rules:"
        prompt+=$'\n'
        prompt+="- Announce your plan before writing code"
        prompt+=$'\n'
        prompt+="- One concern per PR"
        prompt+=$'\n'
        prompt+="- Run scripts/pre-push.sh before pushing"
        prompt+=$'\n'
        prompt+="- Check room assignments before committing fixes"
        prompt+=$'\n'
        prompt+="- Write progress to $progress_file at each milestone"
        prompt+=$'\n\n'
    fi

    # progress file from previous iteration
    if [[ -f "$progress_file" ]]; then
        prompt+="--- PROGRESS FROM PREVIOUS CONTEXT ---"
        prompt+=$'\n'
        prompt+="$(cat "$progress_file")"
        prompt+=$'\n'
        prompt+="--- END PROGRESS ---"
        prompt+=$'\n\n'
    fi

    # recent room messages
    local messages
    messages="$(room poll "$room_id" -t "$token" 2>/dev/null || true)"
    if [[ -n "$messages" ]]; then
        prompt+="--- RECENT ROOM MESSAGES ---"
        prompt+=$'\n'
        prompt+="$messages"
        prompt+=$'\n'
        prompt+="--- END MESSAGES ---"
        prompt+=$'\n\n'
    fi

    # task context
    if [[ -n "$issue" ]]; then
        prompt+="Your current assignment is GitHub issue #${issue}."
        prompt+=" Work on this issue, coordinate in the room, and update progress."
    else
        prompt+="Poll the room for assignments. Work on whatever is assigned to you."
    fi

    printf '%s' "$prompt"
}

# --- build claude command ---
# Returns the claude command array as newline-separated strings (for eval).
# Args: $1=model, remaining=add_dirs
build_claude_cmd() {
    local model="$1"
    shift
    echo "claude"
    echo "-p"
    echo "--model"
    echo "$model"
    echo "--output-format"
    echo "json"
    for dir in "$@"; do
        echo "--add-dir"
        echo "$dir"
    done
}

# --- extract response ---
# Extracts the text response from claude's JSON output.
# Args: $1=output_file
extract_response() {
    local output_file="$1"
    if [[ -f "$output_file" && -s "$output_file" ]]; then
        jq -r '.result // .content // .error // "no output"' "$output_file" 2>/dev/null || cat "$output_file"
    else
        echo "no output"
    fi
}

# --- parse args ---
parse_args() {
    for arg in "$@"; do
        case "$arg" in
            -h|--help) usage; exit 0 ;;
        esac
    done

    if [[ $# -lt 2 ]]; then
        usage
        exit 1
    fi

    ROOM_ID="$1"
    USERNAME="$2"
    shift 2

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --model)    MODEL="$2"; shift 2 ;;
            --issue)    ISSUE="$2"; shift 2 ;;
            --tmux)     USE_TMUX=true; shift ;;
            --max-iter) MAX_ITER="$2"; shift 2 ;;
            --cooldown) COOLDOWN="$2"; shift 2 ;;
            --prompt)   CUSTOM_PROMPT="$2"; shift 2 ;;
            --add-dir)  ADD_DIRS+=("$2"); shift 2 ;;
            --dry-run)  DRY_RUN=true; shift ;;
            -h|--help)  usage; exit 0 ;;
            *)          echo "unknown option: $1" >&2; exit 1 ;;
        esac
    done
}

# --- tmux launcher ---
# If --tmux is set, re-execs the script inside a tmux session and exits.
# Args: $0=script_path, all original args minus --tmux
launch_tmux() {
    local script_path="$1"
    local session_name="ralph-${USERNAME}"

    if tmux has-session -t "$session_name" 2>/dev/null; then
        log "tmux session $session_name already exists — attaching"
        exec tmux attach-session -t "$session_name"
    fi

    local self_args=("$ROOM_ID" "$USERNAME" --model "$MODEL" --max-iter "$MAX_ITER" --cooldown "$COOLDOWN")
    [[ -n "$ISSUE" ]] && self_args+=(--issue "$ISSUE")
    [[ -n "$CUSTOM_PROMPT" ]] && self_args+=(--prompt "$CUSTOM_PROMPT")
    for d in "${ADD_DIRS[@]+"${ADD_DIRS[@]}"}"; do
        self_args+=(--add-dir "$d")
    done

    tmux new-session -d -s "$session_name" "bash $(realpath "$script_path") ${self_args[*]}"
    log "started tmux session: $session_name"
    log "attach with: tmux attach -t $session_name"
    log "logs: tail -f $LOG_FILE"
    exit 0
}

# --- graceful shutdown ---
cleanup() {
    RUNNING=false
    log "shutting down (caught signal)"
    if [[ -n "${TOKEN:-}" && -n "${ROOM_ID:-}" ]]; then
        room send "$ROOM_ID" -t "$TOKEN" "shutting down (signal received)" 2>/dev/null || true
    fi
}

# --- main loop ---
run_loop() {
    local room_id="$1"
    local username="$2"

    ITER=0
    while $RUNNING; do
        ITER=$((ITER + 1))

        if [[ "$MAX_ITER" -gt 0 && "$ITER" -gt "$MAX_ITER" ]]; then
            log "max iterations ($MAX_ITER) reached, stopping"
            room send "$room_id" -t "$TOKEN" "max iterations reached ($MAX_ITER), shutting down" 2>/dev/null || true
            break
        fi

        log "--- iteration $ITER ---"

        # build prompt
        local prompt
        prompt="$(build_prompt "$room_id" "$username" "$TOKEN" "$CUSTOM_PROMPT" "$PROGRESS_FILE" "$ISSUE")"
        if $DRY_RUN; then
            echo "=== DRY RUN: prompt ==="
            echo "$prompt"
            return 0
        fi

        # write prompt to temp file (avoids shell metacharacter issues)
        local prompt_file="/tmp/ralph-room-prompt-${username}.txt"
        printf '%s' "$prompt" > "$prompt_file"

        # build claude command
        local claude_cmd=(claude -p --model "$MODEL" --output-format json)
        for d in "${ADD_DIRS[@]+"${ADD_DIRS[@]}"}"; do
            claude_cmd+=(--add-dir "$d")
        done

        # run claude
        log "running claude -p (model=$MODEL, iteration=$ITER)"
        local output_file="/tmp/ralph-room-output-${username}.json"
        local exit_code=0
        cat "$prompt_file" | "${claude_cmd[@]}" > "$output_file" 2>>"$LOG_FILE" || exit_code=$?

        log "claude exited with code $exit_code"

        # extract response
        local response
        response="$(extract_response "$output_file")"

        # detect context exhaustion
        if detect_context_exhaustion "$exit_code" "$response"; then
            log "context exhaustion detected, writing progress file"
            write_progress_file "$PROGRESS_FILE" "$ITER" "$ISSUE" "$response"
            room send "$room_id" -t "$TOKEN" "context exhausted at iteration $ITER, restarting with fresh context" 2>/dev/null || true
        elif [[ "$exit_code" -ne 0 ]]; then
            log "claude failed (exit $exit_code), will retry after cooldown"
            room send "$room_id" -t "$TOKEN" "claude exited with error (code $exit_code), retrying in ${COOLDOWN}s" 2>/dev/null || true
        fi

        # re-join if token expired
        if detect_token_expiry "$response"; then
            log "token appears invalid, re-joining"
            join_room "$room_id" "$username" "$TOKEN_FILE" || { log "re-join failed, exiting"; return 1; }
        fi

        # cooldown
        if $RUNNING; then
            log "cooldown ${COOLDOWN}s"
            sleep "$COOLDOWN"
        fi
    done

    log "ralph-room stopped after $ITER iterations"
    room send "$room_id" -t "$TOKEN" "offline (ralph-room stopped after $ITER iterations)" 2>/dev/null || true
}

# --- main ---
main() {
    parse_args "$@"

    # resolve paths
    PROGRESS_FILE="$(progress_file_path "$ISSUE" "$USERNAME")"
    LOG_FILE="$(log_file_path "$USERNAME")"
    TOKEN_FILE="$(token_file_path "$ROOM_ID" "$USERNAME")"

    # dependency check
    check_dependencies || exit 1

    # tmux mode
    if $USE_TMUX; then
        if ! command -v tmux &>/dev/null; then
            echo "error: tmux not found (required for --tmux)" >&2
            exit 1
        fi
        launch_tmux "$0"
    fi

    # signal handling
    trap cleanup SIGTERM SIGINT SIGHUP

    log "ralph-room starting: room=$ROOM_ID user=$USERNAME model=$MODEL issue=${ISSUE:-none} max_iter=$MAX_ITER"

    # join room
    join_room "$ROOM_ID" "$USERNAME" "$TOKEN_FILE" || { log "failed to join room, exiting"; exit 1; }
    room send "$ROOM_ID" -t "$TOKEN" "online (ralph-room, model=$MODEL, iter limit=$MAX_ITER)" 2>/dev/null || true

    # run the loop
    run_loop "$ROOM_ID" "$USERNAME"
}

# Guard: skip main() when sourced for testing
if [[ -z "${RALPH_ROOM_SOURCED:-}" ]]; then
    main "$@"
fi
