# Security Audit Report — `knoxio/room`

**Date:** 2026-03-12  
**Audited version:** `3.0.0-rc.6` (commit `1af739d`)  
**Scope:** Full source-code audit of `crates/room-cli`, `crates/room-protocol`, and `crates/room-ralph`  

---

## Executive Summary

`room` is a multi-user chat system designed for human/AI agent coordination, exposing both
Unix Domain Socket (UDS) and optional WebSocket/REST transports. The audit identified
**one critical**, **four high**, **five medium**, and **four low/informational** security
findings. The most severe issue is a logic error in the `validate_token` function that
allows a kicked user to bypass the kick enforcement and continue sending messages.

---

## Table of Contents

1. [Critical Findings](#1-critical-findings)
2. [High Findings](#2-high-findings)
3. [Medium Findings](#3-medium-findings)
4. [Low / Informational Findings](#4-low--informational-findings)
5. [Positive Security Observations](#5-positive-security-observations)
6. [Recommendations Summary](#6-recommendations-summary)

---

## 1. Critical Findings

### CRIT-1 · KICKED Sentinel Usable as a Valid Authentication Token

| Field | Value |
|---|---|
| **Severity** | Critical |
| **CVSS estimate** | 8.1 (AV:N/AC:L/PR:L/UI:N/S:U/C:H/I:H/A:N) via WebSocket TOKEN: path; 7.8 (AV:L/AC:L/PR:L/UI:N/S:U/C:H/I:H/A:N) via UDS-only path |
| **Location** | `crates/room-cli/src/broker/auth.rs:126` |

**Description**

When a user is kicked, the broker inserts a sentinel entry into the token map:

```rust
// admin.rs:60
map.insert(format!("KICKED:{target}"), target.clone());
// key = "KICKED:alice", value = "alice"
```

The `validate_token` function is documented as treating `KICKED:<username>` sentinels
as invalid, but the actual implementation performs a plain hash-map lookup with no
filtering:

```rust
/// A `KICKED:<username>` sentinel is treated as invalid so kicked users
/// cannot authenticate.
pub(crate) async fn validate_token(token: &str, token_map: &TokenMap) -> Option<String> {
    token_map.lock().await.get(token).cloned()  // ← returns a value for KICKED: keys!
}
```

Because the sentinel key is `KICKED:<username>` → `<username>`, a kicked user who
sends the handshake `TOKEN:KICKED:alice` (where `alice` is their own username) will
pass authentication and receive `Some("alice")` back. This allows the kicked user to
continue sending one-shot messages via the `TOKEN:` UDS path and the WebSocket
`TOKEN:` frame, completely circumventing the kick.

The attack pattern is predictable because the sentinel format is documented in code
comments and the project's public `CLAUDE.md` coordination guide.

**Impact:** Privilege-enforcement bypass — a kicked user can continue to inject
messages into any room as their original identity.

**Recommendation**

Filter out `KICKED:` prefixed keys inside `validate_token`:

```rust
pub(crate) async fn validate_token(token: &str, token_map: &TokenMap) -> Option<String> {
    if token.starts_with("KICKED:") {
        return None;
    }
    token_map.lock().await.get(token).cloned()
}
```

---

## 2. High Findings

### HIGH-1 · Legacy `SEND:` Handshake Allows Identity Spoofing

| Field | Value |
|---|---|
| **Severity** | High |
| **Location** | `crates/room-cli/src/broker/handshake.rs:47`, `crates/room-cli/src/oneshot/transport.rs:206` |

**Description**

The `SEND:<username>` UDS handshake variant is explicitly documented as
"legacy unauthenticated". Any process that can reach the daemon socket can send
any message as any username with no token:

```rust
// transport.rs:206
w.write_all(format!("SEND:{username}\n").as_bytes()).await?;
```

The broker's `handle_oneshot_send` accepts this without verifying that the
declared `username` belongs to the caller. This enables:

- Impersonating any user (including the room host) to send messages
- Injecting false system-like messages attributed to real users
- Bypassing DM send-permission checks for DM rooms

**Impact:** Full message-spoofing for any process that can connect to the socket.
On shared machines or containers with a shared `/tmp`, this is trivially exploitable.

**Recommendation**

Deprecate and gate the `SEND:` path behind an authentication check. At minimum,
require that the declared username is registered in the token map before accepting
a `SEND:` connection. Consider removing `SEND:` entirely — all callers in the
codebase can use the `TOKEN:` path instead.

---

### HIGH-2 · Interactive UDS Join Does Not Verify Token Ownership

| Field | Value |
|---|---|
| **Severity** | High |
| **Location** | `crates/room-cli/src/broker/mod.rs:184–231` |

**Description**

The `ClientHandshake::Interactive` path (a bare username with no prefix) joins the
room without any proof of identity:

```rust
ClientHandshake::Interactive(u) => u,  // no token check
```

After join permission is checked (room visibility), `run_interactive_session` is
called with the unverified username. This means any client connected to the socket
can present themselves as any username — including the room host — gaining full
interactive access (including the ability to send admin commands if they adopt the
host's name before the real host joins).

**Impact:** Username squatting and privilege escalation to host-level admin in rooms
where the first connecting client becomes host.

**Recommendation**

Require that interactive joins also hold a valid token. The preferred flow is: client
runs `JOIN:<username>` first to obtain a token, then uses `TOKEN:<uuid>` for the
interactive reconnect. The plain username interactive path should be restricted to
known/trusted token holders or removed entirely.

---

### HIGH-3 · UDS `CREATE` and `DESTROY` Commands Require No Authentication

| Field | Value |
|---|---|
| **Severity** | High |
| **Location** | `crates/room-cli/src/broker/daemon.rs:968–975`, `crates/room-cli/src/broker/daemon.rs:887` |

**Description**

The daemon UDS protocol accepts `CREATE:<room_id>` and `DESTROY:<room_id>` commands
from any connecting client without checking a bearer token:

```rust
DaemonPrefix::Create(room_id) => {
    return handle_create(&room_id, &mut reader, ...).await;  // no auth
}
DaemonPrefix::Destroy(room_id) => {
    return handle_destroy(&room_id, &mut write_half, rooms).await;  // no auth
}
```

In contrast, the REST equivalents (`POST /api/rooms` and a future `DELETE`) do
require a `Bearer` token. The inconsistency means that on a shared machine,
any local process can silently create or destroy rooms.

**Impact:** Denial of service via room destruction; room ID squatting; ability to
create malicious public rooms in the daemon's name space.

**Recommendation**

Require a valid token in `handle_create` and `handle_destroy` before proceeding.
Add a `TOKEN:<uuid>` second-line protocol to these handlers (or require the token
in the same line before the room ID).

---

### HIGH-4 · Admin Token Revocations Not Persisted in Single-Room Mode

| Field | Value |
|---|---|
| **Severity** | High |
| **Location** | `crates/room-cli/src/broker/admin.rs:55–131`, `crates/room-cli/src/broker/auth.rs:16–20` |

**Description**

The admin commands `/kick`, `/reauth`, and `/clear-tokens` mutate the in-memory
`TokenMap` but never call `save_token_map()` to persist those changes:

```rust
// admin.rs kick handler — modifies memory only, never calls save_token_map()
map.retain(|_, u| u != &target);
map.insert(format!("KICKED:{target}"), target.clone());
drop(map);
// ← no save_token_map() call
```

`save_token_map` is only invoked from `issue_token()`. After the broker is restarted,
the `.tokens` file is reloaded, restoring all previously revoked tokens and erasing
all KICKED sentinels. A user who was kicked will regain full access the next time the
broker process restarts.

In daemon mode, the `UserRegistry` _is_ persisted (`revoke_user_tokens` saves
`users.json`), which partially mitigates the issue for the registry-backed validation
path. However, the `system_token_map` (seeded from `tokens.json`) still contains the
stale entry, and the UDS `TOKEN:` path checks the system token map _first_ without
always consulting the registry.

**Impact:** Kicked/reauthed users regain access after broker restart — a common event
during upgrades, crashes, or system reboots.

**Recommendation**

Call `save_token_map(&*map, &state.token_map_path)` at the end of each mutating
admin command (kick, reauth, clear-tokens) in `handle_admin_cmd`. In daemon mode,
also ensure the system `tokens.json` is flushed after revocation.

---

## 3. Medium Findings

### MED-1 · Unauthenticated Room-List Endpoint Exposes Private Room IDs

| Field | Value |
|---|---|
| **Severity** | Medium |
| **Location** | `crates/room-cli/src/broker/ws/rest.rs` (`daemon_api_rooms`) |

**Description**

`GET /api/rooms` returns the IDs of all active rooms without requiring a bearer
token:

```rust
pub(super) async fn daemon_api_rooms(State(state): State<DaemonWsState>) -> impl IntoResponse {
    let rooms = state.rooms.lock().await;
    let ids: Vec<&String> = rooms.keys().collect();
    Json(serde_json::json!({ "rooms": ids }))
}
```

This exposes the names of all rooms, including `Unlisted` and `Private` rooms whose
IDs are supposed to be known only to invited participants.

**Impact:** Enumeration of unlisted and private room IDs by an unauthenticated
attacker with network access to the WebSocket port.

**Recommendation**

Require a bearer token on `GET /api/rooms`. The response can optionally be filtered
to only the rooms the authenticated user is a member of.

---

### MED-2 · WebSocket/REST Server Binds to `0.0.0.0` with No TLS

| Field | Value |
|---|---|
| **Severity** | Medium |
| **Location** | `crates/room-cli/src/broker/mod.rs:129`, `crates/room-cli/src/broker/daemon.rs:402` |

**Description**

When `--ws-port` is supplied, the broker binds to all interfaces:

```rust
let tcp = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
```

There is no TLS, no IP allowlist, and no CORS restriction. Bearer tokens are
transmitted in plaintext HTTP headers, making them trivially interceptable on any
network that is not fully trusted. Additionally, the absence of CORS headers means
any web page can make cross-origin requests to the API.

**Impact:** Token theft via network eavesdropping; CSRF-style attacks from malicious
web pages on the same network.

**Recommendation**

1. Default the bind address to `127.0.0.1` and require an explicit `--bind 0.0.0.0`
   flag for external exposure.
2. Add TLS support (or document that a reverse proxy with TLS is required for
   external deployments).
3. Add restrictive CORS headers (e.g. `tower-http`'s `CorsLayer` with
   `AllowOrigin::list([...])` rather than wildcard).

---

### MED-3 · Unbounded `read_line()` Allows Memory Exhaustion

| Field | Value |
|---|---|
| **Severity** | Medium |
| **Location** | `crates/room-cli/src/broker/mod.rs:183`, `crates/room-cli/src/broker/mod.rs:375`, `crates/room-cli/src/broker/daemon.rs:800,953` |

**Description**

Every `read_line()` call in the broker uses the default `BufReader`, which imposes no
limit on the number of bytes read before a newline is encountered:

```rust
reader.read_line(&mut first).await?;     // handshake — no size limit
reader.read_line(&mut line).await?;      // message loop — no size limit
reader.read_line(&mut config_line).await?; // CREATE config — no size limit
```

A malicious client can send a multi-gigabyte line without a newline, causing the
broker to allocate unbounded memory in `first` / `line` / `config_line` until the
process OOMs or is killed by the OS.

**Impact:** Denial of service against the broker process; all rooms in a daemon
instance share the same process, so a single connection can take down all rooms.

**Recommendation**

Limit line reads to a reasonable maximum (e.g. 1 MiB for messages, 64 KiB for
handshakes). One approach using `tokio::io`:

```rust
const MAX_LINE: usize = 1024 * 1024; // 1 MiB
let n = reader.take(MAX_LINE as u64).read_line(&mut line).await?;
if n == MAX_LINE && !line.ends_with('\n') {
    return Err(anyhow::anyhow!("line too long"));
}
```

---

### MED-4 · Token Cleanup in Admin Commands Hardcodes `/tmp`

| Field | Value |
|---|---|
| **Severity** | Medium |
| **Location** | `crates/room-cli/src/broker/admin.rs:93,110` |

**Description**

The `reauth` and `clear-tokens` admin commands attempt to delete on-disk token files
from `/tmp`:

```rust
if let Ok(entries) = std::fs::read_dir("/tmp") {
    for entry in entries.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.starts_with(&prefix) && s.ends_with(&suffix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
```

However, since version 2.x token files are stored in `~/.room/state/` (via
`paths::token_path()`), not in `/tmp`. The cleanup code scans the wrong directory and
silently leaves revoked token files on disk. While the in-memory token map is the
authoritative source, the stale file on disk can confuse clients that cache the path.
More importantly, the `/tmp` scan itself is a TOCTOU race: an attacker can place a
symlink in `/tmp` matching the expected pattern to cause deletion of an arbitrary file
owned by the broker process.

**Impact:** (1) Revoked token files persist in `~/.room/state/`, causing client
confusion. (2) TOCTOU race in `/tmp` that could be used for limited file deletion.

**Recommendation**

Replace the hardcoded `/tmp` scan with a targeted deletion of the known path:

```rust
// Use the canonical state directory, not /tmp
let token_file = crate::paths::token_path(room_id, &target);
let _ = std::fs::remove_file(&token_file);
// Also try the global path
let global_token = crate::paths::global_token_path(&target);
let _ = std::fs::remove_file(&global_token);
```

---

### MED-5 · User-Supplied Regex Recompiled on Every Filter Call

| Field | Value |
|---|---|
| **Severity** | Medium |
| **Location** | `crates/room-cli/src/query.rs:92` |

**Description**

The `content_regex` query filter compiles the regex from the user-supplied string
on _every invocation_ of `QueryFilter::matches()`:

```rust
if let Some(ref pattern) = self.content_regex {
    match Regex::new(pattern) {  // ← compiled N times for N messages
        Ok(re) => match msg.content() { ... },
        Err(_) => return false,
    }
}
```

For a history file with tens of thousands of messages, this means the regex is
compiled that many times per query. The Rust `regex` crate itself is immune to
classical ReDoS (it uses a linear-time NFA/DFA engine), but recompilation is
expensive and can be used to cause noticeable latency spikes or high CPU usage on
endpoints that apply the regex to a large history.

**Impact:** CPU-based denial of service for authenticated users on the
`GET /api/{room_id}/query?regex=...` and `room query --regex` endpoints.

**Recommendation**

Compile the regex once before the filter loop:

```rust
let compiled_re = self.content_regex.as_ref().and_then(|p| Regex::new(p).ok());
// then in the loop:
if let Some(ref re) = compiled_re {
    match msg.content() {
        Some(c) if re.is_match(c) => {}
        _ => return false,
    }
}
```

---

## 4. Low / Informational Findings

### LOW-1 · Health Endpoints Leak Room Metadata Without Authentication

| Field | Value |
|---|---|
| **Severity** | Low |
| **Location** | `crates/room-cli/src/broker/ws/rest.rs` (`api_health`, `daemon_api_health`) |

**Description**

`GET /api/health` returns room IDs and the number of online users without requiring
any authentication:

```json
{ "status": "ok", "rooms": [{"room": "private-ops", "users": 3}] }
```

This exposes user-count metadata and (in daemon mode) room IDs to unauthenticated
callers.

**Recommendation**

Either require a bearer token for `/api/health` or limit the unauthenticated response
to a minimal liveness indicator (`{"status":"ok"}`), omitting room/user counts.

---

### LOW-2 · No Rate Limiting on Any Endpoint

| Field | Value |
|---|---|
| **Severity** | Low |
| **Location** | All UDS and REST/WebSocket handlers |

**Description**

There is no rate limiting on token issuance (`JOIN:`), message sending (`SEND:`,
`TOKEN:`), or REST endpoints. An attacker can:

- Flood the broker with message sends to fill the chat history file.
- Enumerate usernames by issuing rapid `JOIN:` requests and observing `username_taken`
  errors.
- Cause repeated regex compilations by spamming the query endpoint (see MED-5).

**Recommendation**

Add a per-IP (or per-token) rate limiter using `tower-http`'s `RateLimitLayer` for
REST endpoints. For UDS connections, consider a per-PID or per-UID connection limit
using `SO_PASSCRED` / `getpeercred`.

---

### LOW-3 · Token Files Stored as Plaintext in User Home Directory

| Field | Value |
|---|---|
| **Severity** | Low (mitigated by 0700 directory permissions) |
| **Location** | `crates/room-cli/src/paths.rs:186–196` |

**Description**

Session tokens (UUID v4 strings) are stored as plaintext JSON files in
`~/.room/state/`. The directory is created with mode `0700`, which prevents other
local users from reading the tokens. However:

1. Backup tools, cloud-sync agents (Dropbox, iCloud), or misconfigured `umask`
   settings can expose the directory contents.
2. The token files themselves are created with the default `umask` of the calling
   process; if `umask` is permissive (e.g. `022`) the files may be world-readable.
3. There is no token rotation or expiry mechanism — a token leaked today remains valid
   indefinitely.

**Recommendation**

Ensure token files are created with mode `0600` using `OpenOptions` with explicit
permission bits. Consider adding a configurable token TTL or a `room logout` command
that revokes the on-disk token.

---

### LOW-4 · No Connection Timeout for Idle UDS Clients

| Field | Value |
|---|---|
| **Severity** | Low |
| **Location** | `crates/room-cli/src/broker/mod.rs:375–460` |

**Description**

Interactive client sessions have no idle timeout. A client that connects and then
stops sending or receiving will hold a slot in `ClientMap` and a broadcast channel
sender indefinitely. With the broadcast channel at capacity 256, a single slow client
causes `RecvError::Lagged` for _all_ messages it missed — the error is only logged,
not propagated to the client:

```rust
Err(broadcast::error::RecvError::Lagged(n)) => {
    eprintln!("[broker] cid={cid} lagged by {n}");
}
```

**Impact:** Resource exhaustion (open file descriptors, `ClientMap` entries); silent
message loss for slow consumers.

**Recommendation**

Add a configurable idle timeout (e.g. 5 minutes with no inbound message) and
disconnect lagging clients after a configurable threshold of dropped messages. For the
REST/WS path, axum's `Tower` stack already supports `TimeoutLayer`.

---

## 5. Positive Security Observations

The following security controls are correctly implemented and should be preserved:

| Area | Observation |
|---|---|
| **Directory permissions** | `~/.room/state/` and `~/.room/data/` are created with `0700` (`create_dir_0700`). |
| **Room ID validation** | `validate_room_id()` rejects path traversal (`..`), null bytes, whitespace, and shell-unsafe characters with a comprehensive blocklist. |
| **DM visibility** | `Message::is_visible_to()` consistently enforces that DMs are visible only to sender, recipient, and room host. Applied in history replay, oneshot poll/pull/query, and REST poll/query. |
| **Join permission checks** | `check_join_permission()` enforces `Private`/`Unlisted`/`DM` room ACLs before issuing tokens and before entering interactive sessions. |
| **Admin authorization** | All admin commands verify the issuer is the room host before executing. |
| **Token uniqueness** | `issue_token()` checks for username collisions before issuing a new UUID token. |
| **Registry persistence** | `UserRegistry` auto-saves on every mutation; tokens, membership, and status survive daemon restarts. |
| **ReDoS immunity** | The Rust `regex` crate uses a linear-time engine; no classical ReDoS is possible regardless of pattern complexity. |
| **Stale socket cleanup** | Broker removes the old socket file at startup to prevent bind failures from previous crashes. |
| **PID file liveness** | `is_pid_alive()` uses `kill(pid, 0)` to detect stale PID files before overwriting, preventing duplicate daemon start. |

---

## 6. Recommendations Summary

| ID | Severity | Action |
|---|---|---|
| CRIT-1 | **Critical** | Fix `validate_token` to return `None` for any key starting with `KICKED:`. |
| HIGH-1 | **High** | Require token verification before processing `SEND:<username>` handshake; plan deprecation. |
| HIGH-2 | **High** | Require a valid token for interactive UDS joins; remove or gate the bare-username path. |
| HIGH-3 | **High** | Add token authentication to `handle_create` and `handle_destroy` on the UDS path. |
| HIGH-4 | **High** | Call `save_token_map()` after `kick`, `reauth`, and `clear-tokens` to persist revocations. |
| MED-1 | **Medium** | Require bearer token on `GET /api/rooms`. |
| MED-2 | **Medium** | Default bind to `127.0.0.1`; add TLS guidance; restrict CORS. |
| MED-3 | **Medium** | Limit `read_line()` to a configurable maximum (e.g. 1 MiB). |
| MED-4 | **Medium** | Replace hardcoded `/tmp` scan in admin commands with `paths::token_path()` / `paths::global_token_path()`. |
| MED-5 | **Medium** | Compile the content regex once before filtering the history. |
| LOW-1 | **Low** | Restrict `/api/health` to authenticated callers or remove room/user details from unauthenticated response. |
| LOW-2 | **Low** | Add per-IP / per-token rate limiting on all REST endpoints and UDS join requests. |
| LOW-3 | **Low** | Create token files with mode `0600`; add token TTL or logout command. |
| LOW-4 | **Low** | Add idle-timeout and lagging-client disconnect logic. |

---

*This report was generated as part of a full source-code security audit. No live
exploitation was performed. Findings are based on static analysis of the source code
at the referenced commit.*
