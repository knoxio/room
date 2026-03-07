# room-ralph

Autonomous agent wrapper for [room](https://github.com/knoxio/room). Runs
`claude -p` in a loop with automatic restart on context exhaustion.

## How it works

room-ralph implements the "ralph loop" pattern:

1. Join a room and announce itself
2. Poll the room for recent messages
3. Build a prompt from room context, progress files, and personality
4. Spawn `claude -p` as a subprocess
5. Monitor token usage — restart with a fresh context when nearing the limit
6. Write a progress file so the next iteration can resume where the previous left off
7. Repeat until `--max-iter` is reached or the process is signalled

Context exhaustion is not task death — progress persists across restarts.

## Prerequisites

Both must be on `PATH`:

- **`room`** — the room CLI (`cargo install room-cli`)
- **`claude`** — Claude Code CLI

## Installation

```bash
cargo install room-ralph
```

Or build from source:

```bash
cargo build -p room-ralph --release
```

## Usage

```bash
room-ralph <room-id> <username> [OPTIONS]
```

### Examples

```bash
# Basic — join "myroom" as "agent1", use defaults
room-ralph myroom agent1

# Work on a specific issue with a personality file
room-ralph myroom agent1 --issue 42 --personality persona.txt

# Run in a detached tmux session
room-ralph myroom agent1 --tmux

# Restrict which tools claude can use
room-ralph myroom agent1 --allow-tools Read,Grep,Glob,Bash

# Preview the prompt without running claude
room-ralph myroom agent1 --dry-run

# Use a custom model with extra directories
room-ralph myroom agent1 --model sonnet --add-dir ../shared-lib --add-dir ../docs
```

## Options

| Flag | Default | Description |
|---|---|---|
| `<room-id>` | required | Room to join |
| `<username>` | required | Username to register with |
| `--model <model>` | `opus` | Claude model to use |
| `--issue <number>` | — | GitHub issue number; enables progress file persistence |
| `--tmux` | off | Run in a detached tmux session (`ralph-<username>`) |
| `--max-iter <n>` | `50` | Max iterations before stopping (0 = unlimited) |
| `--cooldown <secs>` | `5` | Seconds between iterations |
| `--prompt <file>` | — | Custom system prompt file (replaces the built-in prompt) |
| `--personality <file>` | — | Personality file; contents prepended before all prompt content |
| `--add-dir <path>` | — | Additional directories for `claude --add-dir` (repeatable) |
| `--allow-tools <tools>` | — | Comma-separated tool allow list (passed as `--allowedTools` to claude) |
| `--dry-run` | off | Print the prompt that would be sent, then exit |
| `-v` / `-V` / `--version` | — | Print version |

## Environment variables

All positional arguments and key options can be set via environment variables.
CLI flags take precedence over environment variables.

| Variable | Equivalent flag | Description |
|---|---|---|
| `RALPH_ROOM` | `<room-id>` | Room to join |
| `RALPH_USERNAME` | `<username>` | Username to register with |
| `RALPH_MODEL` | `--model` | Claude model to use (default: `opus`) |
| `RALPH_ISSUE` | `--issue` | GitHub issue number |
| `RALPH_ALLOWED_TOOLS` | `--allow-tools` | Comma-separated tool allow list (see below) |
| `CONTEXT_LIMIT` | — | Total context window size (default: `200000`) |
| `CONTEXT_THRESHOLD` | — | Percentage at which to restart (default: `80`) |

### Tool precedence

Allowed tools are resolved in this order:

1. `--allow-tools` CLI flag (highest)
2. `RALPH_ALLOWED_TOOLS` environment variable
3. Safe defaults: `Read`, `Glob`, `Grep`, `WebSearch`, `Bash(room *)`,
   `Bash(git status)`, `Bash(git log)`, `Bash(git diff)`

Set `RALPH_ALLOWED_TOOLS=none` (or `--allow-tools none`) to disable tool
restrictions entirely and let claude use all available tools.

## Context exhaustion

room-ralph monitors token usage after each claude invocation. When input tokens
exceed 80% of the context limit (default 200k), it:

1. Writes a progress file to `/tmp/room-progress-<issue>.md`
2. Announces the restart in the room
3. Spawns a fresh claude instance with the progress file included in the prompt

The threshold and limit can be tuned via `CONTEXT_LIMIT` and `CONTEXT_THRESHOLD`
environment variables (see [Environment variables](#environment-variables) above).

## Progress files

When `--issue` is set, room-ralph writes progress files at
`/tmp/room-progress-<issue>.md`. These files track iteration count, completed
steps, files modified, and decisions made — allowing a fresh context to resume
work seamlessly.

Delete progress files after the corresponding PR merges.

## Logging

Logs are written to both stderr and `/tmp/ralph-room-<username>.log`.

## Further reading

- [ralph-setup.md](../../docs/ralph-setup.md) — permissions, personality, and memory convention
- [CLAUDE.md](../../CLAUDE.md) — agent coordination protocol and project conventions
