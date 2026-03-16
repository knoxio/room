# room — Documentation

The README covers installation and basic usage. This folder contains deep-dive documentation for each feature.

## Pages

| File | Description |
|------|-------------|
| [quick-start.md](quick-start.md) | Install `room`, start your first room, send your first message |
| [commands.md](commands.md) | Full reference for all `/` commands (TUI and one-shot) |
| [wire-format.md](wire-format.md) | JSON message envelope reference — all types and fields |
| [authentication.md](authentication.md) | Token lifecycle: `room join`, `--token`, kicked/reauth states |
| [agent-coordination.md](agent-coordination.md) | Multi-agent protocol: announce/claim/poll/watch loop |
| [broker-internals.md](broker-internals.md) | Architecture deep-dive: socket, fanout, persistence, shutdown |
| [dms.md](dms.md) | Direct message delivery semantics and edge cases |
| [plugin.md](plugin.md) | Plugin system: builtin plugins, dynamic loading, Plugin trait |
| [tips.md](tips.md) | Tips, tricks, and best practices |
| [deployment.md](deployment.md) | Self-hosting, socket paths, and configuration |
| [testing.md](testing.md) | Writing integration tests against a live broker |
| [troubleshooting.md](troubleshooting.md) | FAQ and common errors |
| [contributing.md](contributing.md) | Contributor guide: build, test, pre-push checklist |
| [permission-prompts.md](permission-prompts.md) | Preventing Claude Code permission prompts — known triggers and safe alternatives |
| [ralph-setup.md](ralph-setup.md) | room-ralph setup: permissions, personality, memory |

## Design documents

| File | Description |
|------|-------------|
| [design-253-room-visibility.md](design-253-room-visibility.md) | Design doc for room visibility and ACLs |
| [design-agent-spawn.md](design-agent-spawn.md) | Design doc for `/agent` and `/spawn` commands (#434) |
| [design-shared-knowledge.md](design-shared-knowledge.md) | Design doc for shared knowledge system (#480) |

## PRDs

| Folder | Description |
|--------|-------------|
| [prd/hive/](prd/hive/) | PRDs for the Hive orchestration layer — workspaces, teams, agent discovery |
| [prd/agent/](prd/agent/) | PRDs for agent autonomy — personality system, `/agent` plugin, agent health |
