# Setting up room-ralph

`room-ralph` is an autonomous agent wrapper that runs `claude -p` in a loop, restarting
on context exhaustion. It communicates with a room broker via the `room` CLI (subprocess
calls, not library imports).

## Permissions

Claude Code has its own permission system that controls what the agent can do at runtime.
Configure it via `.claude/settings.json` in the project directory:

```json
{
  "permissions": {
    "allow": [
      "Read", "Glob", "Grep", "WebSearch",
      "Bash(git status)", "Bash(git diff)", "Bash(cargo test)"
    ],
    "deny": []
  }
}
```

- **`allow`** — tool patterns the agent can use without prompting. Supports exact names
  (`Read`) and parameterized patterns (`Bash(cargo test)`).
- **`deny`** — tool patterns that are always blocked, even if listed in `allow`.

Ralph also supports `--disallow-tools` (or `RALPH_DISALLOWED_TOOLS` env var) for
hard-blocking specific tools from the session. Unlike `--allow-tools` which controls
auto-approval, `--disallow-tools` removes tools entirely — the agent cannot use them
even if prompted. Supports granular patterns like `Bash(python3:*)`.

```bash
# Block Write and Edit — agent can read but not modify files
room-ralph myroom reviewer --disallow-tools Write,Edit

# Block specific shell commands while keeping Bash available
room-ralph myroom agent1 --disallow-tools "Bash(rm:*),Bash(curl:*)"
```

For fully autonomous operation (e.g. CI pipelines), ralph passes
`--dangerously-skip-permissions` to the claude subprocess. Do not use this for untrusted
workloads — it grants the agent unrestricted access to all tools including file writes,
shell commands, and network requests.

### Choosing a permission level

| Use case | Approach |
|---|---|
| Interactive development | Default (claude prompts for each tool) |
| Supervised automation | `.claude/settings.json` with a curated allow list |
| Fully autonomous (CI, demo) | `--dangerously-skip-permissions` flag |

## Personality and system prompt

Agents get their instructions from two sources, both of which you control:

1. **CLAUDE.md** — Claude automatically loads this file from the working directory at the
   start of every session. Put project-wide conventions, coding standards, and behavioral
   rules here. This is the primary mechanism for shaping agent personality and role.

2. **`--prompt` flag** — Pass a custom prompt file to ralph to override the built-in
   default system prompt. Use this for agent-specific roles (e.g. a QA agent vs. a
   feature-building agent).

```bash
room-ralph --room myroom --username qa-bot --prompt /path/to/qa-prompt.md
```

The `--prompt` file replaces ralph's default instructions entirely. If you want to layer
custom instructions on top of the defaults, put them in CLAUDE.md instead.

## Memory convention (3 layers)

Ralph agents use a three-layer memory system. Each layer has a different lifespan and
purpose:

| Layer | Location | Lifespan | Purpose |
|---|---|---|---|
| **Memory files** | `~/.claude/projects/<project>/memory/` | Permanent (across sprints) | Stable patterns, architecture, preferences |
| **Progress files** | `/tmp/room-progress-<issue>.md` | Per-issue (delete on merge) | Cross-session state for active work |
| **Room messages** | Room chat log | Per-broker session | Coordination, announcements, decisions |

### Memory files

Claude manages these automatically via its auto-memory system. The main index
(`MEMORY.md`) is loaded into every conversation context. Topic-specific files
(e.g. `debugging.md`, `patterns.md`) hold detailed notes and are linked from the index.

You do not need to create these manually — Claude writes and updates them as it works.
To seed initial knowledge (e.g. project conventions), put it in CLAUDE.md instead.

### Progress files

Ralph reads progress files on startup so a fresh claude instance can resume where the
previous one left off after context exhaustion. Progress files follow the template at
`scripts/progress-template.md` and track: current status, completed steps, files
modified, and decisions made.

Delete stale progress files after the corresponding PR merges.

### Room messages

Agents send and receive coordination messages via `room send` / `room poll`. These are
ephemeral (tied to the broker session lifetime) and serve as the real-time coordination
layer between agents and humans.

## Status convention

Agents must keep their `/set_status` current at all times. The host uses the member
status panel (TUI) or `room who` to see what every agent is doing. Stale or missing
statuses make coordination harder.

### Required status updates

Status text must include **what you are doing and the specific context** (file, feature,
PR, issue number). A phase word alone (`working`, `reading`, `testing`) is not useful —
the host needs to know what is happening at a glance.

| Phase | Good status | Bad status |
|---|---|---|
| Starting work | `reading src/broker.rs for #42` | `reading` |
| Drafting code | `drafting kick parser in tui/input.rs` | `working` |
| Running tests | `running cargo test — 461 expected` | `testing` |
| Fixing CI | `fixing clippy in oneshot/who.rs` | `fixing` |
| PR open | `PR #236 open — kicked users fix` | `PR open` |
| Blocked | `blocked on #38 — need schema decision` | `blocked` |
| Done | `done — PR #236 merged` | `done` |
| Idle | *(clear status with `/set_status`)* | |

### How to set status

```bash
# From room send (one-shot)
room send <room-id> -t <token> /set_status drafting auth handler

# Clear status
room send <room-id> -t <token> /set_status
```

Ralph agents should set status automatically at each milestone via `room send`. The
CLAUDE.md coordination protocol enforces this convention — agents that do not update
their status will be flagged during review.
