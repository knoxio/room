# PRD: Hive Server (Backend)

**Status:** Draft
**Author(s):** samantha (based on r2d2 tech investigation, saphire component map)
**Date:** 2026-03-16
**Dependencies:** room v3.5.1 (shipped), room-protocol, axum
**Breaks down:** #798 (tech stack)

---

## Problem

Room provides the messaging and plugin infrastructure for multi-agent coordination, but there is no application layer to manage workspaces, authenticate humans, orchestrate agent lifecycles, or track costs. Users must manually run CLI commands to spawn agents, manage rooms, and coordinate work.

## Goal

Build the Hive server — a Rust/axum backend that sits between the Hive frontend and the room daemon, providing:
1. Human authentication (OAuth/API keys mapped to room tokens)
2. Agent lifecycle management (clone, spawn, monitor, stop)
3. Workspace grouping (rooms + team rosters)
4. Real-time room relay via WebSocket

## Non-goals

- Multi-host daemon federation (single daemon, per joao directive)
- Billing/metering (Phase 2)
- Agent discovery/expertise indexing (Phase 3)
- Plugin marketplace implementation (deferred)

## Architecture

```
Hive Frontend  --HTTP/WS-->  Hive Server  --WS-->  Room Daemon (bundled)
                                  |
                                  |-- spawns -->  room-ralph processes
```

### Crate structure

```
crates/
  hive-server/          -- axum HTTP/WS server, main entry point
    src/
      main.rs           -- CLI + server startup
      config.rs         -- TOML/env config (port, room socket, data dir)
      auth.rs           -- OAuth flow + API key validation
      rooms.rs          -- Room CRUD proxy (delegates to room daemon)
      agents.rs         -- Agent spawn/stop/list/logs (wraps room-ralph)
      ws_relay.rs       -- WebSocket relay: frontend <-> room daemon
      db.rs             -- SQLite for Hive-specific state (workspaces, teams)
```

### Tech stack (decided)

| Layer | Choice | Rationale |
|---|---|---|
| Language | Rust | Shares room-protocol types, single toolchain |
| HTTP framework | axum | Already used in room-daemon |
| Database | SQLite (rusqlite) | Zero-config, embedded, sufficient for single-host |
| WebSocket | tokio-tungstenite | Already used in room |
| Auth | OAuth2 (oxide-auth) | Standard flow for web apps |
| Config | TOML (toml crate) | Consistent with room-ralph |

## Implementation phases

### Phase 1: Skeleton + Room Proxy (MVP)
- axum server with health endpoint
- WebSocket relay: proxy frontend WS to room daemon WS
- REST proxy: `/api/rooms/*` delegates to room daemon REST API
- Config file (hive.toml): room socket path, HTTP port, data directory
- No auth (localhost-only, development mode)

### Phase 2: Auth + Agent Management
- OAuth2 login flow (GitHub provider)
- API key generation for programmatic access
- Token mapping: Hive user -> room token (via `room join`)
- Agent CRUD: spawn/stop/list/logs via room-ralph
- Agent workspace isolation (uses room's `~/.room/agents/` dirs)

### Phase 3: Workspaces + Teams
- Workspace CRUD (SQLite): name, rooms, team roster
- Team manifest parsing (JSON)
- Batch agent provisioning from manifest
- Cross-room unified timeline endpoint

## Data model

Hive owns (SQLite):
- `workspaces`: id, name, created_at
- `workspace_rooms`: workspace_id, room_id
- `users`: id, provider, email, room_token
- `api_keys`: id, user_id, key_hash, scopes
- `team_manifests`: id, workspace_id, manifest_json

Room owns (unchanged):
- Messages, tokens, subscriptions, chat files, plugin state

## Room features used

| Feature | API | Purpose |
|---|---|---|
| Room create/destroy | REST or UDS | Workspace room lifecycle |
| User join | `room join` via CLI or REST | Token minting for Hive users |
| Agent spawn | `room-ralph` CLI | Agent lifecycle |
| Message relay | WebSocket `/ws/<room_id>` | Real-time frontend updates |
| Plugin commands | `/taskboard`, `/agent` | Task and agent management |
| Health check | `GET /api/health` | Monitoring |

## Resolved questions

1. **Single binary or separate?** Single deployable unit — Hive server embeds room daemon or connects to co-located daemon. Per joao directive.
2. **Backend language?** Rust + axum. Shares room-protocol types. Per r2d2 investigation.
3. **Database?** SQLite — zero-config, sufficient for single-host deployment.
4. **Multi-host?** Not planned. Single daemon per joao directive.
