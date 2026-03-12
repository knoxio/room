# Hive — Multi-Room Orchestration Layer

## What is Hive?

Hive is the product layer that sits above `room`. Where `room` provides the
low-level primitives (broker, sockets, messages, plugins), Hive provides the
user-facing experience for managing teams of agents across multiple rooms.

Think of it this way:

- **room** = a single chat room with a broker, message history, and plugins.
- **room daemon** = multiple rooms running on one host, with shared auth.
- **Hive** = the workspace that ties rooms, agents, teams, and tasks together
  into a coherent product experience.

## Relationship to room

Hive does not replace room — it orchestrates it. Every Hive feature is built on
top of existing room primitives:

| Hive concept | Built on |
|---|---|
| Workspace | Daemon multi-room + UserRegistry |
| Team | Agent personalities + /spawn + room subscriptions |
| Task board | /taskboard plugin + room messages |
| Agent discovery | /who + /stats + agent status tracking |
| Multi-room view | room poll --rooms + subscription tiers |

Hive never bypasses room's socket protocol. All communication flows through the
broker. Hive is a client-side experience layer, not a new server.

## Design principles

1. **room stays simple.** Hive adds features as plugins, CLI wrappers, and UI
   components — not by modifying broker internals.

2. **Agents are users.** Hive treats AI agents and human users identically at the
   protocol level. A spawned agent joins via `room join`, subscribes via
   `room subscribe`, and communicates via `room send`. The distinction is in the
   management layer (spawn, stop, monitor), not the protocol.

3. **Rooms are boundaries.** Each room is an independent coordination context.
   Cross-room coordination happens via agents subscribed to multiple rooms, not
   via server-side room linking.

4. **Progressive disclosure.** A user running `room myroom alice` today should
   get value immediately. Hive features like teams, workspaces, and agent
   discovery are additive — they enhance the experience without requiring
   migration.

## PRDs in this folder

| Document | Status | Summary |
|---|---|---|
| [prd-workspace.md](prd-workspace.md) | Draft | Multi-room workspace concept, team management |
| [prd-team-provisioning.md](prd-team-provisioning.md) | Draft | /team create, agent manifests, ephemeral teams |
| [prd-agent-discovery.md](prd-agent-discovery.md) | Draft | Expertise indexing, agent reputation, skill matching |

## Dependencies on room features

Hive depends on several room features that are in various stages of readiness:

| Feature | Status | Hive dependency |
|---|---|---|
| Multi-room daemon | Shipped (v3) | Required for workspaces |
| UserRegistry | Shipped (v3) | Required for cross-room identity |
| /spawn + personalities | In progress (#434, #439) | Required for team provisioning |
| /taskboard | In progress (#444) | Required for task management |
| Subscription tiers | Shipped (v3) | Required for multi-room views |
| Agent status tracking | Shipped (v3) | Required for agent discovery |

## Open questions for joao

1. **Should Hive be a separate binary or part of `room`?** Current assumption:
   Hive commands are subcommands of `room` (e.g. `room hive workspace create`).
   A separate `hive` binary adds deployment complexity but cleaner separation.

2. **Web UI or CLI-only?** These PRDs assume CLI-first. A web dashboard (reading
   room state via REST API) is a natural next step but not scoped here.

3. **Multi-host support?** Room daemon is currently single-host. Hive workspaces
   spanning multiple hosts would require WebSocket relay between daemons. Is this
   a near-term goal or future aspiration?

4. **Billing/metering?** If agents consume API credits (Claude API calls), should
   Hive track per-agent or per-team costs? This affects the /stats plugin design.
