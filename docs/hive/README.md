# Hive — Agent Orchestration Platform

## What is Hive?

Hive is a standalone application (web or native) that provides the user-facing
experience for managing teams of AI agents. It uses `room` as its real-time
collaboration infrastructure — the way a web application uses PostgreSQL for
data storage.

Hive is **not** a CLI tool, not a room subcommand, and not part of the room
binary. It is a separate deployment unit that communicates with room exclusively
via WebSocket (primary) and REST (secondary).

## Architecture

```
┌──────────────────────────────────────────────────────┐
│  Hive Instance                                       │
│                                                      │
│  ┌────────────┐  ┌──────────────┐  ┌──────────────┐ │
│  │   Web UI   │  │ Agent Runner │  │   Billing    │ │
│  │ (frontend) │  │  (lifecycle) │  │  (metering)  │ │
│  └─────┬──────┘  └──────┬───────┘  └──────┬───────┘ │
│        │                │                  │         │
│  ┌─────┴────────────────┴──────────────────┴───────┐ │
│  │              Hive Server (backend)              │ │
│  │  Auth (OAuth/API keys) · Token lifecycle        │ │
│  │  Agent deployment · Workspace management        │ │
│  └─────────────────────┬───────────────────────────┘ │
│                        │                              │
│                  WS + REST API                        │
│                        │                              │
│  ┌─────────────────────┴───────────────────────────┐ │
│  │           Room Server (bundled)                  │ │
│  │  Broker · Plugins · Message routing · Persistence│ │
│  └─────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────┘
```

The deployment unit is a **Hive instance** that bundles a room server scoped
to it. Room handles real-time collaboration. Hive handles everything else.

## Separation of concerns

| Domain | Owner | Examples |
|---|---|---|
| Real-time messaging | room | Broker, fan-out, NDJSON persistence, chat history |
| Plugins | room | /taskboard, /queue, /help, /stats — single definition generates CLI + slash + WS interfaces |
| Agent behavior | room-ralph | Prompt building, context monitoring, Claude interaction, restart on exhaustion |
| UI | Hive | Web dashboard, workspace views, task boards, agent status panels |
| Authentication | Hive | OAuth, API keys, user accounts — maps to room tokens internally |
| Billing/metering | Hive | Per-agent cost tracking (cloud API and local model agents), per-team rollups |
| Agent deployment | Hive | Clone repos, set up workdirs, spawn agents, monitor lifecycle |
| Token lifecycle | Hive | Calls `room join` to mint tokens, stores them, passes to agents on spawn |
| Workspace management | Hive | Groups of rooms, team rosters, cross-room views |

## Design principles

1. **Room stays generic.** Room is infrastructure — it does not know about Hive,
   billing, or agent deployment. Hive-specific features are built as plugins or
   in the Hive server, never by modifying broker internals.

2. **Plugins are the extension points.** A single plugin definition generates
   three interfaces: CLI subcommand (`room <plugin> <action>`), in-room slash
   command (`/<plugin> <action>`), and WS/REST endpoint. Room dispatches all
   three to the same handler via `CommandContext`. Hive consumes the WS/REST
   interface.

3. **Agents are users at the protocol level.** Hive treats AI agents and humans
   identically in the room wire format. An agent joins via token, subscribes to
   rooms, sends messages, and receives broadcasts. The distinction is in Hive's
   management layer (spawn, stop, monitor, bill), not in room's protocol.

4. **No compile-time dependency on room-cli.** Hive talks to room via WS and
   REST only. It may depend on `room-protocol` for message types, but never
   imports broker internals or transport code.

5. **WS is the primary transport.** WebSocket provides persistent bidirectional
   streaming — Hive connects once and receives messages in real-time. REST is
   secondary, used for stateless operations (health checks, one-off queries,
   admin actions from the web UI).

## Transport: room as infrastructure

Hive communicates with its bundled room server over WebSocket:

```
Hive Server  ──[WS]──►  Room Broker (ws://localhost:<port>/ws/<room_id>)
             ──[REST]──► Room Broker (http://localhost:<port>/api/...)
```

The current WS transport handles one room per connection. For Hive managing
multiple rooms, future work includes a multiplexed WS endpoint at the daemon
level with `room_id` in the message envelope, eliminating the need for N
separate connections.

### Plugin responses

Plugins currently emit human-readable system messages. Hive needs
machine-readable responses. A structured JSON `data` field alongside the
human-readable `content` will allow Hive to parse task state, queue contents,
and other plugin data programmatically without scraping chat text.

## PRDs in this folder

| Document | Status | Summary |
|---|---|---|
| [prd-workspace.md](prd-workspace.md) | Draft | Workspace concept: room grouping, team rosters, cross-room views |
| [prd-team-provisioning.md](prd-team-provisioning.md) | Draft | Agent team manifests, spawn/stop/scale, billing integration |
| [prd-agent-discovery.md](prd-agent-discovery.md) | Draft | Expertise indexing, capacity tracking, skill matching |

## Room features Hive depends on

| Feature | Status | Hive dependency |
|---|---|---|
| Multi-room daemon | Shipped (v3) | Hive bundles a daemon instance |
| UserRegistry | Shipped (v3) | Cross-room identity |
| WS + REST transport | Shipped (v3) | Primary communication channel |
| /taskboard plugin | Shipped (v3) | Task lifecycle management |
| /queue plugin | Shipped (v3) | Backlog management |
| Subscription tiers | Shipped (v3) | Per-room message filtering |
| Agent status tracking | Shipped (v3) | Capacity and status views |
| Token persistence | Shipped (v3) | Tokens survive broker restarts |

## Hive-readiness gaps in room

These room-side improvements would benefit Hive but are not blockers for
initial development:

| Gap | Description | Priority |
|---|---|---|
| WS multiplexing | Single connection with room_id in envelope (daemon-level) | High |
| Structured plugin responses | JSON `data` field alongside human text | High |
| Plugin trait decoupling (#454) | Move Plugin trait to room-protocol or room-plugin crate | Medium |
| read_line size limit (#470) | Bound incoming message size to prevent OOM | Medium |
| WS bind address (#455-MED-2) | Default to 127.0.0.1 instead of 0.0.0.0 | Low |

## Decisions made (2026-03-12)

These questions from the original PRDs have been answered by joao:

1. **Hive is a separate web/native app**, not a CLI tool or room subcommand.
2. **Web UI, not CLI.** Hive's interface is a web dashboard or native app.
3. **Hive owns token lifecycle.** Room's existing join/validate flow is
   sufficient — Hive calls `room join` to mint tokens and manages them in its
   own database.
4. **Agent lifecycle is its own domain** within Hive — managing clones,
   workdirs, spawning agents. Whether this becomes a separate crate is an
   implementation decision deferred to development phase.
5. **Billing tracks per-agent costs in detail**, including mixed local model
   and cloud API agents.
6. **Multi-host is future aspiration**, not near-term. WS relay between
   daemons would be needed but is not scoped.
7. **Single plugin definition generates all interfaces** — CLI + slash + WS.
   This is a joao directive for room's plugin framework.
