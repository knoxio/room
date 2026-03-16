# US-BE-016: Agent workspace provisioning

**As a** Hive server
**I want to** create an isolated directory structure for each spawned agent
**So that** agents have their own working directories, memory files, and log storage without interfering with each other

## Acceptance Criteria
- [ ] Before spawning, the server creates `config.data_dir/agents/<agent_id>/` with subdirectories: `workspace/`, `logs/`, `memory/`
- [ ] The agent process is started with `workspace/` as its working directory
- [ ] The `logs/` directory contains `agent.log` (stdout+stderr redirect target)
- [ ] The `memory/` directory is pre-populated with a `MEMORY.md` stub if a personality template exists at `config.data_dir/personalities/<personality>.md`
- [ ] Directory creation is atomic: if any step fails, the partial directory is removed and the spawn returns `500 Internal Server Error`
- [ ] Directories are created with mode `0700` (owner-only access)
- [ ] Provisioning is idempotent: if the directory already exists (e.g. agent restart), it is reused without error

## Technical Notes
- Implement in `crates/hive-server/src/agents.rs` as a `provision_agent_workspace(agent_id, config)` function called before `Command::new(...)`
- Use `std::fs::create_dir_all` for each subdirectory; set permissions with `std::fs::set_permissions`
- Personality template lookup: `config.data_dir/personalities/<personality>.md` — if absent, skip memory pre-population silently
- Workspace cleanup on agent deletion is deferred to a separate administrative endpoint (not in Phase 2 scope); directories are retained for inspection

## Phase
Phase 2 (Auth + Agent Management)
