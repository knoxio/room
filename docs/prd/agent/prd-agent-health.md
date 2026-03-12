# PRD: Agent Health and Heartbeat System

## Status

Draft — 2026-03-12

## Problem

Today there is no way to know if an agent is working correctly, stuck, or
misbehaving. A ralph process can be alive (PID exists) but completely non-functional:
claude might be hung on a rate limit, context might be exhausted without restart,
or the agent might be in an infinite loop. The host has no visibility into agent
health without SSH-ing into the machine and reading log files.

Additionally, agents are expected to follow protocols (announce plans, update status,
follow the task workflow) but there is no enforcement. A misbehaving agent can silently
stall without anyone noticing until work is blocked.

## Goals

1. **Status heartbeat**: Enforce regular `/set_status` updates from agents. Flag
   agents that go silent.
2. **Stale detection**: Automatically detect agents that are alive but not making
   progress.
3. **Plan adhesion**: Track whether agents follow their personality's workflow
   (announce plan, wait for approval, implement, test, PR, announce completion).
4. **Lifecycle enforcement**: Different health rules for persistent vs ephemeral
   agents.
5. **Actionable alerts**: Surface health issues in the room so the host can intervene.

## Non-goals

- Automatic remediation (auto-restart stuck agents). The host decides what to do.
- Per-tool-call monitoring (too granular; health is about behavioral patterns).
- Cost monitoring (Hive handles billing/metering).

## Design

### Health Model

Each spawned agent has a health state tracked by the `/agent` plugin:

```rust
pub struct AgentHealth {
    /// Last time the agent sent any message to the room
    pub last_message_at: Option<Instant>,

    /// Last time the agent called /set_status
    pub last_status_at: Option<Instant>,

    /// Last status text
    pub last_status_text: Option<String>,

    /// Current health verdict
    pub health: HealthStatus,

    /// Personality-defined thresholds
    pub config: HealthConfig,
}

pub enum HealthStatus {
    /// Agent is active — recent status updates within threshold
    Healthy,

    /// Agent has not updated status within status_interval_max
    /// but has sent messages recently
    Warning,

    /// Agent has not sent any message within stale_threshold
    Stale,

    /// Agent process has exited
    Exited(Option<i32>),
}
```

### Status Timestamp Monitoring

The plugin monitors two timestamps for each agent:

1. **Last message**: Any message (chat, command, system) from the agent's username.
   Updated by intercepting broadcast messages in the broker's fanout path.

2. **Last status**: Specifically `/set_status` commands. Updated when the broker
   processes a status update from the agent.

These are compared against personality-defined thresholds:

| Threshold | Default | Description |
|---|---|---|
| `status_interval_max` | 5 minutes | Max time between `/set_status` updates |
| `stale_threshold` | 10 minutes | Max time since any message before flagged stale |

### Health Check Loop

The plugin runs a periodic health check (every 60 seconds by default):

```
for each spawned agent:
    1. Check PID alive (kill(pid, 0))
       → if dead: set Exited, broadcast alert, remove from active map
    2. Check last_status_at against status_interval_max
       → if exceeded: set Warning, broadcast warning (once, not repeated)
    3. Check last_message_at against stale_threshold
       → if exceeded: set Stale, broadcast alert
```

Alerts are broadcast as system messages:

```
⚠ agent dev-anna has not updated status in 7m (threshold: 5m)
🔴 agent dev-anna is stale — no messages in 12m (threshold: 10m)
💀 agent scout-c7 exited with code 1
```

### Stale Detection

An agent is **stale** when it has not sent any message to the room within the
`stale_threshold`. This catches:

- Claude hung on a rate limit or API error
- Context exhausted but ralph failed to restart
- Network issues between ralph and the broker
- Ralph stuck in an infinite retry loop

Stale detection is distinct from status monitoring:
- **Warning** (no status update): Agent is probably working but not following the
  status protocol. May need a nudge.
- **Stale** (no messages at all): Agent is likely non-functional. Needs intervention.

### Plan Adhesion Tracking

Persistent agents with a `workflow.on_task` definition are expected to follow the
defined steps. The plugin tracks adherence by monitoring status updates against the
workflow:

```
Workflow: read → announce plan → wait → implement → test → PR → announce
Status:   "reading src/broker.rs"  → matches "read" step
Status:   "drafting fix in auth.rs" → matches "implement" step
Status:   "running cargo test"      → matches "test" step
```

This is **advisory, not enforcing**. The plugin compares status text against workflow
step keywords and logs which steps have been observed. If an agent skips steps (e.g.
jumps from "reading" to "opening PR" without testing), the plugin flags it:

```
⚠ agent dev-anna may have skipped workflow steps: test (expected before PR)
```

Implementation approach:
- Each workflow step maps to a set of keywords (configurable in the personality).
- Status updates are matched against keywords to track which steps have been seen.
- Missing expected steps trigger a warning, not a block.
- The host can `/agent health <username>` to see the full workflow tracking state.

### Lifecycle-Specific Rules

#### Persistent agents

- Must maintain regular status updates (`status_interval_max`).
- Stale threshold applies continuously.
- Workflow tracking resets on each new task claim.
- Health state persists across context restarts (ralph re-joins with same username).
- Expected to run indefinitely — exiting is an alert condition.

#### Ephemeral agents

- Have an **expected duration** derived from task complexity or personality config.
  Default: 30 minutes for reviewers, 60 minutes for other ephemeral types.
- Stale threshold is the expected duration (not the personality's `stale_threshold`).
- Workflow tracking follows the personality's `on_task` steps.
- Clean exit (code 0) is normal — not an alert.
- Non-zero exit is an alert.
- Exceeding expected duration triggers a warning.

### `/agent health` Command

```
/agent health [username]
```

Without a username, shows a summary of all agents:

```
 username     | health  | last status  | last msg  | workflow
 dev-anna     | healthy | 2m ago       | 30s ago   | implement (4/7)
 reviewer-kai | warning | 8m ago       | 1m ago    | review (2/3)
 scout-c7     | exited  | —            | —         | — (exit 0)
```

With a username, shows detailed health for one agent:

```
Agent: dev-anna (pid 12345)
Personality: coder (persistent)
Health: healthy
Uptime: 34m
Last status: "running cargo test for #42" (2m ago)
Last message: 30s ago

Workflow tracking (task #42):
  ✓ read target files
  ✓ announce plan in room
  ✓ wait for approval
  ✓ implement
  → run tests         ← current
  · open PR
  · announce completion
```

### Progress Persistence

Progress files store cross-session state for active work. Location:
`~/.room/state/progress/<username>-<issue>.md`

This is persistent storage — survives both context exhaustion and host reboot.
The agent workdir (`/tmp/room-agent-<username>/`) is ephemeral and cleared on reboot.

The health system reads progress files to understand agent state after a context
restart. If ralph restarts due to context exhaustion, the new instance reads the
progress file and resumes from the recorded checkpoint.

### Integration with `/agent list`

`/agent list` shows the health column from the health system:

```
 username     | pid   | personality | uptime  | health
 dev-anna     | 12345 | coder       | 34m     | healthy
 reviewer-kai | 12400 | reviewer    | 8m      | warning (no status 8m)
```

### Alert Throttling

To avoid alert spam:
- Each health state transition is announced once (not every check cycle).
- Warning → Stale escalation is announced.
- Stale → back to Healthy (agent resumes activity) is announced as recovery.
- Repeated checks in the same state produce no output.

### Hive Integration

When Hive manages agents:
- Hive implements its own health monitoring using the structured `data` field from
  plugin responses and direct status queries.
- Hive may have more sophisticated health checks (API cost tracking, output quality
  metrics, SLA compliance).
- The room-native health system remains useful for standalone operation and as the
  raw signal source that Hive builds on.
- The `/agent health` command works regardless of who spawned the agent.

## Implementation Plan

### Phase 1: Status monitoring (with /agent plugin Phase 1)

- Add `AgentHealth` struct to `SpawnedAgent` tracking
- Hook into broker fanout to update `last_message_at` for spawned agents
- Hook into status command handler to update `last_status_at`
- Periodic health check loop (tokio interval, 60s)
- System message alerts on Warning and Stale transitions

### Phase 2: `/agent health` command

- Add `health` subcommand to AgentPlugin
- Summary view (all agents) and detail view (single agent)
- Include health column in `/agent list`

### Phase 3: Workflow tracking

- Parse personality workflow definitions into step keywords
- Match status text against workflow steps
- Track observed steps per task
- Flag skipped steps as warnings
- `/agent health <username>` shows workflow progress

### Phase 4: Ephemeral agent lifecycle

- Expected duration tracking for ephemeral agents
- Clean exit handling (no alert on exit code 0)
- Over-time warnings
- Auto-remove from spawn map after clean exit

## Tests

- Unit: health state transitions (Healthy → Warning → Stale → Exited)
- Unit: alert throttling (only one alert per transition)
- Unit: workflow step keyword matching
- Unit: ephemeral vs persistent health rule differences
- Integration: spawn agent, wait for stale threshold, verify alert broadcast
- Integration: spawn agent, send status updates, verify Healthy state maintained
