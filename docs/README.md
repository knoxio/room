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
| [plugin.md](plugin.md) | Claude Code plugin setup and slash commands |
| [tips.md](tips.md) | Tips, tricks, and best practices |
| [deployment.md](deployment.md) | Self-hosting, socket paths, and configuration |
| [testing.md](testing.md) | Writing integration tests against a live broker |
| [troubleshooting.md](troubleshooting.md) | FAQ and common errors |
| [contributing.md](contributing.md) | Contributor guide: build, test, pre-push checklist |
