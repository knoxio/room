# Design: Room Visibility & ACLs (#253)

## Problem

All rooms are currently public and joinable by anyone with the room ID. There's no concept of private rooms, invite-only access, or per-room permissions. With roomd (#251) managing multiple rooms, we need visibility controls before DMs (#254) and multi-room (#258) can work.

## Terminology

- **public** — anyone can discover and join
- **private** — discoverable in listings but requires invite to join
- **unlisted** — not discoverable, join requires knowing room ID + invite
- **dm** — private, max 2 members, auto-created by `/dm` command

## Design

### 1. RoomVisibility Enum (room-protocol)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RoomVisibility {
    Public,
    Private,
    Unlisted,
    Dm,
}
```

Lives in `room-protocol` since clients need it for `room ls` output.

### 2. RoomConfig (new struct in broker/state.rs)

```rust
pub struct RoomConfig {
    pub visibility: RoomVisibility,
    pub max_members: Option<usize>,     // None = unlimited
    pub invite_list: HashSet<String>,   // usernames allowed to join (private/unlisted/dm)
    pub created_by: String,             // creator username
    pub created_at: String,             // ISO 8601
}
```

Stored alongside RoomState. With roomd (#251), this becomes part of the DaemonState room registry.

### 3. Join Authorization

Current flow: `handle_oneshot_join` → `issue_token` → success.

New flow adds a gate before token issuance:

```
JOIN request → check_join_permission(username, room_config) → issue_token or reject
```

Rules:
- **Public**: always allowed
- **Private/Unlisted**: allowed if `username ∈ invite_list` OR `username == created_by`
- **Dm**: allowed if `username ∈ invite_list` AND `invite_list.len() <= 2`
- **Kicked**: rejected (existing KICKED sentinel still applies on top of visibility)

### 4. Room Discovery (`room ls`)

New oneshot command that queries roomd for room listing:

```
LIST → daemon returns Vec<RoomListEntry>
```

```rust
pub struct RoomListEntry {
    pub room_id: String,
    pub visibility: RoomVisibility,
    pub member_count: usize,
    pub created_by: String,
}
```

Filtering: only `Public` and `Private` rooms appear in listings. `Unlisted` and `Dm` are hidden. Private rooms show a lock indicator but are still joinable if invited.

### 5. Room Creation (`room create`)

New oneshot command:

```
room create <room-id> [--visibility public|private|unlisted] [--max-members N] [--invite user1,user2]
```

Defaults: `--visibility public`, no member limit, empty invite list.

For DMs, the `/dm` command creates a room with:
- `visibility: Dm`
- `max_members: Some(2)`
- `invite_list: {sender, recipient}`
- `room_id: dm-{sorted_usernames_hash}` (deterministic, so `/dm user` always finds the same room)

### 6. Invite Management

New commands for room owners/hosts:

- `/invite <username>` — add to invite_list, works for private/unlisted/dm rooms
- `/uninvite <username>` — remove from invite_list (does not kick if already joined)
- `/room-info` — show visibility, max_members, invite_list, member count

Authorization: only `created_by` or `host_user` can manage invites.

### 7. Integration with User Registry (#252)

The user registry maps persistent user identity to capabilities:

```rust
pub struct UserRecord {
    pub username: String,
    pub created_at: String,
    pub rooms: Vec<String>,          // rooms this user belongs to
    pub dm_rooms: Vec<String>,       // DM room IDs
}
```

When a user joins a room, their `rooms` list is updated. When they leave, it's removed. This enables:
- `room ls --mine` — list rooms the user has joined
- Cross-room identity for @mentions (#256)
- DM room lookup without scanning all rooms

### 8. Wire Format Changes (room-protocol)

Minimal additions:

```rust
// New fields on Join message (optional, backward-compatible)
pub enum Message {
    Join { ..., visibility: Option<RoomVisibility> },
    // ...
}

// New message type for room lifecycle
RoomCreated { id, room, user, ts, seq, visibility, max_members },
```

Alternative: keep Message enum unchanged, use System messages for room lifecycle events. Simpler, no wire format change.

**Recommendation**: Use System messages. Room lifecycle is metadata, not chat. Example: `"joao created room 'design-review' (private, max 5 members)"`.

### 9. Persistence

RoomConfig stored as JSON in roomd data directory:

```
~/.room/rooms/<room-id>/config.json
~/.room/rooms/<room-id>/chat.ndjson
~/.room/users.json
```

This replaces the current `/tmp/room-<id>.chat` convention. Rooms persist across daemon restarts.

### 10. Migration Path

1. Existing rooms default to `Public` visibility, no max_members, empty invite_list
2. `room_id` used as-is (no namespace change)
3. Token files in `/tmp/` continue to work during transition
4. Old `room <room-id> <username>` command still works (creates public room if doesn't exist)

## File Impact

| File | Change |
|------|--------|
| `room-protocol/src/lib.rs` | Add `RoomVisibility` enum, `RoomListEntry` struct |
| `broker/state.rs` | Add `RoomConfig` to RoomState |
| `broker/auth.rs` | Add `check_join_permission()` gate |
| `broker/mod.rs` | Wire join gate into `handle_oneshot_join` |
| `broker/commands.rs` | Add `/invite`, `/uninvite`, `/room-info` commands |
| `broker/mod.rs` | Add `LIST` oneshot handler for room discovery |
| `cli (main.rs)` | Add `room create`, `room ls` subcommands |

## Dependencies

- **Depends on**: #251 (roomd daemon) — RoomConfig lives in DaemonState
- **Depends on**: #252 (user registry) — invite_list references persistent user identity
- **Blocks**: #254 (DM rooms), #255 (unified stream, room filtering)

## Open Questions

1. Should room visibility be changeable after creation? (e.g., public → private)
2. Should we support room deletion, or just archival?
3. For DMs: if user A blocks user B, where does that live? User registry or room config?
4. Should invite_list use username strings or user IDs (if we add IDs in #252)?
