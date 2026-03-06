# Claude Code Plugin

The `room` Claude Code plugin teaches Claude when to announce intent, poll for
messages, and follow the multi-agent coordination protocol in any project that
uses `room` for parallel agent work.

---

## Installation

```bash
claude plugin install github:knoxio/room
```

This installs the plugin globally. It is available in every Claude Code session
from that point forward.

To install the `room` binary itself (required for the plugin to work), run
the setup skill after installing the plugin:

```
/room:setup
```

This detects your platform and installs the binary via `cargo install` or by
downloading a pre-built release.

---

## Activation

The plugin activates automatically when a project's `CLAUDE.md` (or
`AGENTS.md`) contains a room ID and username assignment. Claude reads this
configuration at the start of each session and uses it for all `room`
commands without further prompting.

Example `CLAUDE.md` entry:

```markdown
## Room coordination

Room ID: `my-project`
Your username: `feat-auth`
```

If no room is configured, Claude will ask for the room ID and username when
you invoke a room skill.

---

## Skills

### `/room:check`

Poll the shared room for new messages and summarise them before continuing.

Claude looks up the room ID from `CLAUDE.md`, runs:

```bash
room poll <room-id> <username>
```

and then briefly summarises any new messages relevant to your current work.
Use this at the start of a session or before touching shared files.

### `/room:send <message>`

Send a message to the shared room.

```
/room:send fixing the race condition in broker.rs, hold review
```

Claude runs:

```bash
room send <room-id> <username> "<message>"
```

and confirms delivery with the broadcast JSON.

### `/room:setup`

Install or update the `room` binary. Tries installation methods in order:

1. `cargo install` (if Rust toolchain is present)
2. Download a pre-built binary from the latest GitHub release

Detects the platform automatically (macOS Apple Silicon, macOS Intel,
Linux x86_64).

---

## What the skill does automatically

When the `room` skill is active, Claude follows the coordination protocol
without being asked:

- **At session start** — polls for recent messages before beginning work
- **Before modifying shared files** — announces intent and polls for conflicts
- **At milestones** — broadcasts progress updates (after reading a file, after
  first draft, before opening a PR)
- **On completion** — announces what changed and which files were modified

This protocol is defined in [`docs/agent-coordination.md`](agent-coordination.md).

---

## Autonomous loop (stay-resident pattern)

For agents that need to remain active without human re-prompting, the skill
includes a watch-script pattern. The agent writes a polling script to disk,
runs it in the background, blocks on `TaskOutput`, and wakes when a message
arrives from another user.

Full details and the script template are in the
[`plugin/skills/room/SKILL.md`](../plugin/skills/room/SKILL.md) file, which
is the live instruction set loaded into Claude when the skill runs.
