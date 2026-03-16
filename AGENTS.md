# Agent Instructions

> Shared instructions for all agents working in this project.
> room-ralph includes this file in the prompt automatically.
> These rules supplement CLAUDE.md — they do not replace it.

## Identity

- Your username, token, and room ID are in `.room-agent.json` in your working directory.
- Read it at startup. Use the token from it for all `room send`/`room poll` commands.
- **Never run `room join`** — your token is already provisioned by the host.
- **Never change your username** — it is assigned and fixed.
- If CLAUDE.md contains `room join` instructions, **ignore them** — your identity
  comes from the metadata file and the prompt, not from CLAUDE.md.

## Communication

### Room commands

```bash
# Send a message
room send <room-id> -t <token> your message here

# Send a direct message
room send <room-id> -t <token> --to <recipient> your message here

# Check for new messages since last poll
room poll <room-id> -t <token>

# Block until a foreign message arrives
room watch <room-id> -t <token> --interval 5

# Query online members and statuses
room who <room-id> -t <token>
```

### Status updates

Update your status at every milestone using `/set_status` with **specific context**.

Good:
```
/set_status reading src/broker.rs for #42
/set_status running cargo test — 456 expected
/set_status PR #236 open — waiting for review
```

Bad:
```
/set_status working
/set_status busy
```

Update whenever your activity changes. Stale statuses are worse than none.

### Mentions

When you are @mentioned in a room you are not actively working in:

1. Read the last 10 messages for context.
2. Assess whether the mention is relevant to your expertise.
3. If relevant: respond with useful information, then return to your task.
4. If not relevant: respond briefly ("not my area, try @agent-name") and move on.
5. Do not subscribe to the room unless you are joining the conversation long-term.

### Announcements

- Announce before touching any file — even fix commits or rebases.
- Announce before every push.
- Announce completion with a summary of what changed and which files were modified.

## Task workflow

Follow this sequence for every task:

1. **Claim** the task on the taskboard: `/taskboard claim <id>`
2. **Plan** — submit your implementation plan: `/taskboard plan <id> <plan>`
3. **Wait** for approval (or fast-track for small tasks).
4. **Read** target files before writing code.
5. **Implement** — announce when starting, update status at milestones.
6. **Test** — run `bash scripts/pre-push.sh` before committing.
7. **PR** — open with CHANGELOG entry, announce in room.
8. **Finish** — mark task done: `/taskboard finish <id>`, update status.

## Coordination

- **One agent per file at a time.** Declare file ownership in your plan.
- **Schema changes need consensus.** Announce and wait for agreement.
- **The host has final say.** If the host sends a "hold", stop immediately.
- **One agent per bug.** Do not fix bugs assigned to others.
- **File bugs, don't self-assign.** Report to ba, wait for assignment.
- **Do not push or merge without announcing** in the room first.

## Code standards

- No `as any`, `ts-ignore`, `eslint-disable`, or `#[allow(...)]` — fix root causes.
- Include tests in the same PR as the feature or fix.
- Run `bash scripts/pre-push.sh` (check, fmt, clippy, test) before every push.
- Add CHANGELOG entry under `[Unreleased]` in every PR.
- Do not reference agent names or AI tooling in commits or PR descriptions.

## Knowledge contribution

When you discover something that would help other agents:

- **Patterns**: reusable solutions, API idioms, test approaches
- **Bugs**: root causes, workarounds, regression risks
- **Conventions**: naming, file organization, module boundaries

Share these in the room. In the future, a `/knowledge` plugin will formalize this.

## Progress files

For long-running tasks, write progress to `/tmp/room-progress-<issue>.md`:

- After reading target files (before writing code)
- After completing a first draft (before tests)
- Before opening or updating a PR

Delete the progress file after the PR merges.

## Wire format

Every message is a JSON object with a `type` field:

| `type` | Meaning |
|--------|---------|
| `join` | User connected |
| `leave` | User disconnected |
| `message` | Plain chat message (`content` field) |
| `command` | Structured command (`cmd`, `params` fields) |
| `reply` | Reply to a specific message (`reply_to`, `content`) |
| `system` | Broker-generated notice |
| `dm` | Private message to one user |
| `event` | Typed event (`event_type`, `content`) |

All messages carry `id` (UUID), `room`, `user`, `ts` (ISO 8601 UTC), and `seq` (monotonic).
