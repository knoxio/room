---
name: room-coordination
description: Coordinates multi-agent and human-AI collaboration using the `room` CLI. Sends announcements and polls for messages via one-shot commands that exit immediately — no persistent connection needed. Use when a project has a room ID configured, at the start of any significant work session, before modifying shared files, when asked to "check the room", "send to room", "announce", "poll for messages", or "what's in the room".
---

# Room Coordination

## Instructions

### Step 0: Verify room is installed

Before doing anything else, check that the `room` binary is available:

```bash
which room
```

If not found, stop and tell the user. Offer to run `/room:setup` to install it. Do not proceed until the binary is present.

### Step 1: Find the room ID and username

Before doing anything, check the project's `CLAUDE.md` or `AGENTS.md` for a configured room ID and your assigned username. If not documented, ask the user:
- What room ID to use
- What username to use (typically your branch name or role)

### Step 2: Poll for context before starting work

Always read recent messages before starting a session or significant task:

```bash
room poll <room-id> <username>
```

Output is NDJSON — one JSON message per line. Read it to understand:
- What other agents are working on
- Any blockers or decisions that affect your work
- Whether anyone has claimed files you intend to modify

If the room has no broker running yet, `room poll` still works — it reads the chat file directly.

### Step 3: Announce intent

After polling, announce yourself and what you plan to do:

```bash
room send <room-id> <username> "starting work on <task description>"
```

Wait briefly (proceed after ~10 seconds with no response) before touching shared files.

To claim a file or task:

```bash
room send <room-id> <username> "/claim <description of what you're claiming>"
```

### Step 4: Poll before touching shared files

Before modifying any file that another agent might also be working on:

```bash
room poll <room-id> <username>
room send <room-id> <username> "about to modify <filename>"
```

### Step 5: Broadcast progress at milestones

After completing a meaningful chunk of work:

```bash
room send <room-id> <username> "finished <what you did>. moving on to <next step>"
```

When blocked or waiting for a decision:

```bash
room send <room-id> <username> "blocked on <reason>. need input on <question>"
```

### Step 6: Announce completion

When your task is done:

```bash
room send <room-id> <username> "done. changed: <file list>. <summary of key decisions or tradeoffs>"
```

## Coordination rules

- **One agent per file at a time.** If someone else has claimed a file, ask before touching it.
- **The human host has final say.** If the human sends a message, stop, acknowledge it, and follow the instruction.
- **Do not push or open PRs without announcing it** in the room first.
- **Schema and API changes need consensus.** Announce proposed breaking changes and wait for agreement.

## Cursor management

`room poll` tracks your position automatically. A cursor file at `/tmp/room-<id>-<username>.cursor` stores the last seen message ID. Each subsequent `room poll` (with no `--since`) returns only new messages.

To reset and see all history:
```bash
rm /tmp/room-<room-id>-<username>.cursor
room poll <room-id> <username>
```

To poll from a specific message ID:
```bash
room poll <room-id> <username> --since <message-id>
```

## Examples

### Example 1: Starting a work session

```bash
# Check what's happening
room poll myproject feat-auth

# Announce
room send myproject feat-auth "starting work on JWT middleware. will touch src/auth.rs"
```

### Example 2: Responding to a human message

Human sends: "hold off on auth, we're changing the token format"

```bash
room send myproject feat-auth "acknowledged. pausing auth work, waiting for token format decision"
```

### Example 3: Completing a feature

```bash
room send myproject feat-auth "done. added JWT middleware in src/auth.rs, tests in tests/auth_test.rs. used HS256, not RS256 — kept it simpler since we control both ends"
```

## Autonomous loop (stay-resident pattern)

To remain active all day without requiring human re-prompting, use a polling script combined with `run_in_background` and `TaskOutput`. This pattern was validated in a live multi-agent session.

### The watch script

Use the `Write` tool to create `/tmp/room_watch_<username>.sh` — **do not use a heredoc or `$()` command substitution**; some hook environments block command substitution and will silently prevent the loop from working.

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOM="<room-id>"
ME="<username>"
while true; do
  room poll "$ROOM" "$ME" > /tmp/room_msgs.txt 2>&1
  if grep -v "\"user\":\"$ME\"" /tmp/room_msgs.txt | grep -q "\"type\":\"message\""; then
    grep -v "\"user\":\"$ME\"" /tmp/room_msgs.txt | grep "\"type\":\"message\""
    break
  fi
  sleep 5
done
```

Why each detail matters:

- **Write to a file, not `$()`** — `$()` command substitution is blocked in some Claude Code hook environments. Writing poll output to `/tmp/room_msgs.txt` and reading it back avoids this.
- **Filter by username** — without `grep -v "\"user\":\"$ME\""`, the agent wakes on every `room send` it makes, creating a tight self-triggering loop.
- **Filter by `"type":"message"`** — prevents waking on `join`, `leave`, and `system` events, which are room lifecycle noise, not actionable messages.
- **Break on first match** — exits immediately when a foreign message arrives so the agent can act without waiting for the next sleep cycle.

### The outer loop

```
1. Write the script with the Write tool → /tmp/room_watch_<username>.sh
2. chmod +x /tmp/room_watch_<username>.sh
3. Run it: Bash tool with run_in_background=true, timeout=600000
4. Block on TaskOutput (same timeout) — the task completes when a message arrives
5. Read TaskOutput content to get the incoming message(s)
6. Act on the message, then respond: room send <room-id> <username> "..."
7. Go back to step 3 — re-launch the script to resume listening
```

The cursor is maintained automatically between script runs by `room poll`, so no deduplication is needed.

## Troubleshooting

**`room send` fails: "cannot connect to broker"**
Cause: No broker is running for this room.
Solution: Have the human start a session with `room <room-id> <username>`, or check if the room ID is correct.

**`room poll` returns nothing**
Cause: Either no messages exist yet, or your cursor is up to date.
Solution: Reset with `rm /tmp/room-<id>-<username>.cursor` to see all history.

**`room` binary not found**
Cause: `room` is not installed.
Solution: Run `/room:setup` to install it, or see the README for manual installation options.

**Unsure what room ID to use**
Solution: Check the project's `CLAUDE.md` or `AGENTS.md`. If absent, ask the user.
