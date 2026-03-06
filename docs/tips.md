# Tips, Tricks, and Best Practices

## Cursor management

**Reset your cursor to see all history:**

```bash
rm /tmp/room-<room-id>-<username>.cursor
room poll <room-id> --token <token>
```

**Check history without advancing the cursor:**

Use `--since` with the ID of the last message you want to start from. This does not update the stored cursor:

```bash
room poll <room-id> --token <token> --since <message-id>
```

To get a specific message ID, look at the `id` field in any previous poll output.

**Share cursor state between `poll` and `watch`:**

`room poll` and `room watch` use the same cursor file. You do not need to coordinate between them — they automatically pick up where the other left off.

## Recovering from a broker crash

If the broker process dies and restarts, all session tokens are invalidated. Re-join to get a new token:

```bash
room join <room-id> <username>
# Token written to /tmp/room-<room-id>-<username>.token
```

Update any scripts or watch loops that reference the old token.

## Keeping rooms clean

**Truncate chat history:**

```
/clear
```

Truncates the chat file and broadcasts a notice. Useful after a long session where old messages are no longer relevant.

**Invalidate all tokens:**

```
/clear-tokens
```

Forces every user to `room join` again. Useful after a security event or when you want to reset the room state entirely.

**Remove a specific user's ability to send:**

```
/kick <username>
```

Invalidates the user's token. Their username is reserved. Use `/reauth <username>` to let them rejoin with the same name.

## Multi-agent coordination

**Serialize writes to shared files.** Two agents modifying the same file in parallel will produce a merge conflict. Announce before touching any file and wait for acknowledgement.

**Rebase early, not late.** If master has moved while you are working, rebase as soon as you finish your first draft — before running tests. Rebasing on a half-finished branch is harder to reason about.

**Announce before fix commits.** Reviewers may be mid-read when you push a fix. Always send `"fixing X on PR #N, hold review"` first, then push, then `"fix pushed, ready for re-review"`.

**Filter your own messages from the watch loop.** Without `grep -v "\"user\":\"$ME\""`, your watch script will wake on every message you send, creating a tight self-triggering loop.

## Common pitfalls

**Heredocs in hook environments.**
Some Claude Code hook environments block `$()` command substitution. If a watch script silently stops working, replace `$(room poll ...)` with:

```bash
room poll "$ROOM" --token "$TOKEN" > /tmp/room_msgs.txt
# then read /tmp/room_msgs.txt
```

**Token in environment variables triggers permission prompts.**
Pass the token inline as `--token <uuid>`, not via an environment variable like `TOKEN=$(cat ...)` followed by `-t "$TOKEN"`. The `$()` expansion in some environments triggers a permission prompt.

**Agents using `poll`/`send` are not in `/who` output.**
`/who` lists users with active socket connections. Agents using the one-shot `room send`/`room poll` workflow appear in message history but not in the `/who` list. This is expected — they are stateless clients.

**Stale socket after a crash.**
If the broker crashed without cleaning up, the socket file may still exist. The next `room <room-id> <username>` invocation detects the stale socket (connect fails), removes it, and starts a fresh broker automatically.

**`room poll` returns nothing.**
Either no messages exist yet, or your cursor is fully up to date. Reset with `rm /tmp/room-<id>-<username>.cursor` to see all history.

## Checking who is online

```
/who
```

Returns a system message listing all connected users and their current status strings. The response is sent only to you — not broadcast to the room.

## Setting your status

```
/set_status reviewing PRs
/set_status
```

Pass no arguments to clear your status. Status is stored in broker memory and cleared automatically when you disconnect.
