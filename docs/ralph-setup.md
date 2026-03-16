# Setting up room-ralph

> **Note:** `room-ralph` has been moved to its own repository:
> [knoxio/room-ralph](https://github.com/knoxio/room-ralph).
> This document is kept for historical reference. See the new repository for up-to-date
> documentation.

`room-ralph` is a Rust binary that runs `claude -p` in an autonomous loop, restarting on
context exhaustion. It communicates with a room broker via the `room` CLI (subprocess calls,
not library imports).

## Installation

```bash
cargo install room-ralph          # from crates.io
# or build from source:
cargo build -p room-ralph --release
```

## Quick start

```bash
# Minimal: join a room and start coding
room-ralph myroom agent-1

# With a builtin personality and issue tracking
room-ralph myroom agent-1 --personality coder --issue 42

# With a tool profile (controls allowed/disallowed tools)
room-ralph myroom agent-1 --profile reviewer

# In a tmux session (detached, named ralph-<username>)
room-ralph myroom agent-1 --personality coder --tmux

# Connect to a daemon socket
room-ralph myroom agent-1 --socket /tmp/roomd.sock
```

## CLI reference

| Flag | Env var | Default | Description |
|---|---|---|---|
| `<room_id>` | `RALPH_ROOM` | (required) | Room to join |
| `<username>` | `RALPH_USERNAME` | (required) | Username to register |
| `--model` | `RALPH_MODEL` | `opus` | Claude model to use |
| `--issue` | `RALPH_ISSUE` | — | GitHub issue number; enables progress file persistence |
| `--personality` | — | — | Builtin name or file path (see below) |
| `--profile` | `RALPH_PROFILE` | — | Tool profile: coder, reviewer, coordinator, notion, reader |
| `--prompt` | — | — | Custom system prompt file (replaces built-in prompt entirely) |
| `--allow-tools` | — | — | Comma-separated tools to auto-approve (merges with profile) |
| `--disallow-tools` | `RALPH_DISALLOWED_TOOLS` | — | Comma-separated tools to hard-block (merges with profile) |
| `--allow-all` | `RALPH_ALLOW_ALL` | false | Skip all tool restrictions |
| `--add-dir` | — | — | Additional directories for claude (repeatable) |
| `--socket` | `ROOM_SOCKET` | — | Override broker socket path |
| `--tmux` | — | false | Run in detached tmux session |
| `--max-iter` | — | 50 | Max iterations before stopping (0 = unlimited) |
| `--cooldown` | — | 5 | Seconds between iterations |
| `--list-personalities` | — | — | Print builtin personalities and exit |
| `--dry-run` | — | — | Print the prompt that would be sent, then exit |

## Personalities

Personalities bundle a system prompt fragment, tool profile, and default model. Use
`--personality <name>` to activate one. Builtins:

| Name | Profile | Model | Description |
|---|---|---|---|
| `coder` | Coder | opus | Writes code, runs tests, opens PRs |
| `reviewer` | Reviewer | opus | Reviews PRs, checks code quality, runs clippy |
| `researcher` | Reader | sonnet | Reads code, searches, summarizes findings |
| `coordinator` | Coordinator | opus | Manages tasks, coordinates agents, tracks progress |
| `documenter` | Notion | sonnet | Writes docs, updates external systems, maintains changelogs |

You can also pass a file path: `--personality /path/to/custom-prompt.md`. The file contents
are prepended to the system prompt. Custom file paths do not set profile or model defaults.

Use `--list-personalities` to see available builtins.

## Tool profiles

Profiles define auto-approved and hard-blocked tools for each agent role. Set via
`--profile <name>` or implicitly via `--personality`.

| Profile | Auto-approved | Hard-blocked |
|---|---|---|
| **coder** | Read, Edit, Write, Glob, Grep, WebSearch, Bash(room/git/cargo/gh/pre-push) | (none) |
| **reviewer** | Read, Glob, Grep, Bash(room/gh pr) | Write, Edit |
| **coordinator** | Read, Glob, Grep, Bash(room/gh) | Write, Edit |
| **notion** | Read, Glob, Grep, Bash(room/gh), mcp\_\_notion\_\_\* | Write, Edit, Bash(git push/commit) |
| **reader** | Read, Glob, Grep | Bash, Write, Edit |

Explicit `--allow-tools` and `--disallow-tools` flags **merge** on top of the profile's
base lists. `--allow-all` overrides everything — no restrictions at all.

**Key distinction:**
- `--allow-tools` (mapped to `--allowedTools`) controls **auto-approval** — tools not
  listed still exist but require manual approval (auto-denied in `-p` mode)
- `--disallow-tools` (mapped to `--disallowedTools`) **hard-blocks** tools — they are
  completely removed from the session
- Disallow always wins over allow

## Permissions

Claude Code has its own permission system. Configure via `.claude/settings.json`:

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

For fully autonomous operation (e.g. CI pipelines), use the `--allow-all` flag
(or `RALPH_ALLOW_ALL=true`), which passes auto-approval for all tools to the claude
subprocess. Do not use this for untrusted workloads.

| Use case | Approach |
|---|---|
| Interactive development | Default (claude prompts for each tool) |
| Supervised automation | `.claude/settings.json` + `--profile` |
| Fully autonomous (CI, demo) | `--allow-all` flag |

## System prompt

Agents get instructions from two sources:

1. **CLAUDE.md** — loaded automatically from the working directory. Put project-wide
   conventions, coding standards, and behavioral rules here.
2. **`--prompt` flag** — replaces ralph's built-in system prompt entirely. Use for
   custom agent roles. If you want to layer instructions on top of the defaults, use
   CLAUDE.md or `--personality` instead.

## Memory convention (3 layers)

Ralph agents use a three-layer memory system:

| Layer | Location | Lifespan | Purpose |
|---|---|---|---|
| **Memory files** | `~/.claude/projects/<project>/memory/` | Permanent (across sprints) | Stable patterns, architecture, preferences |
| **Progress files** | `/tmp/room-progress-<issue>.md` | Per-issue (delete on merge) | Cross-session state for active work |
| **Room messages** | Room chat log | Per-broker session | Coordination, announcements, decisions |

### Memory files

Claude manages these automatically via its auto-memory system. The main index
(`MEMORY.md`) is loaded into every conversation context. Topic-specific files
hold detailed notes and are linked from the index.

### Progress files

Ralph reads progress files on startup so a fresh claude instance can resume where the
previous one left off after context exhaustion. Enable with `--issue <number>`.

Progress files follow the template at `scripts/progress-template.md` and track: current
status, completed steps, files modified, and decisions made. Delete after the PR merges.

### Room messages

Agents send and receive coordination messages via `room send` / `room poll`. These are
ephemeral (tied to the broker session) and serve as the real-time coordination layer.

## Status convention

Agents must keep `/set_status` current. The host uses the member status panel (TUI) or
`room who` to see what every agent is doing.

Status text must include **what and where** — a phase word alone is not useful:

| Phase | Good status | Bad status |
|---|---|---|
| Starting | `reading src/broker.rs for #42` | `reading` |
| Drafting | `drafting kick parser in tui/input.rs` | `working` |
| Testing | `running cargo test — 461 expected` | `testing` |
| Fixing | `fixing clippy in oneshot/who.rs` | `fixing` |
| PR open | `PR #236 open — kicked users fix` | `PR open` |
| Blocked | `blocked on #38 — need schema decision` | `blocked` |
| Done | `done — PR #236 merged` | `done` |
| Idle | *(clear with `/set_status`)* | |

```bash
room send <room-id> -t <token> /set_status drafting auth handler
room send <room-id> -t <token> /set_status   # clear
```
