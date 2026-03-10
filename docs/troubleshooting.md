# Troubleshooting and FAQ

---

## "cannot connect to broker" / "no broker found"

**Symptom:** Running `room send` or `room poll` prints something like:

```
Error: cannot connect to broker: No such file or directory (os error 2)
```

**Cause:** No broker is running for the room. The broker socket
(`/tmp/room-<room-id>.sock`) does not exist.

**Fix:** Start the broker by opening an interactive session:

```bash
room <room-id> <username>
```

The first user to connect automatically starts the broker. Leave the TUI
session open; the broker exits when it does.

---

## "stale socket detected (ECONNREFUSED)"

**Symptom:** The room prints `stale socket detected (ECONNREFUSED), cleaning up`
then starts normally.

**Cause:** A previous broker process crashed or was killed without removing its
socket file. The next user to connect detects the stale file, removes it, and
becomes the new broker.

**Fix:** No action needed — this is handled automatically.

---

## Duplicate username error

**Symptom:** `room join` returns an error like `username 'alice' already in use`.

**Cause:** Another session is already registered with that name.

**Fix:**
- If you previously crashed and want to reclaim your name, ask the host to run
  `/kick alice` then `/reauth alice`, then call `room join` again.
- Or choose a different username.

---

## "permission denied" on admin commands

**Symptom:** Typing `/kick`, `/exit`, or another admin command returns a
`permission denied` system message.

**Cause:** Admin commands are restricted to the room **host** (the first user
to join the current broker session). You are not the host.

**Fix:** Ask the host to run the command, or restart the broker (if appropriate)
and be the first to reconnect.

---

## Token rejected / authentication error

**Symptom:** `room send` or `room poll` returns an authentication error.

**Common causes:**
1. The broker was restarted (e.g. via `/exit`) — all tokens are invalidated on
   restart.
2. The host ran `/clear-tokens` — all tokens were revoked.
3. You were kicked with `/kick`.

**Fix:** Run `room join <username>` to obtain a new token, then
update any scripts or environment variables that hold the old token.

---

## Messages from before I joined are missing

**Symptom:** When connecting with the TUI, you only see the last ~20 messages.

**Cause:** By default, the broker replays only the last 20 history lines on
connect.

**Fix:** Connect with a larger `-n` value to replay more history:

```bash
room <room-id> <username> -n 100
```

Or use `room pull` to read history without joining the TUI:

```bash
room pull <room-id> --token "$TOKEN" -n 200
```

---

## Poll cursor is wrong / I'm seeing old messages again

**Symptom:** `room poll` returns messages you've already processed.

**Cause:** The cursor file (`~/.room/state/room-<room-id>-<username>.cursor`) was
deleted, overwritten, or your username changed.

**Fix:** Let the cursor advance naturally (each `poll` call updates it), or
use `--since <message-id>` to manually set the starting point for one call.

To reset the cursor intentionally (re-read all history):

```bash
rm ~/.room/state/room-<room-id>-<username>.cursor
```

---

## `/exit` doesn't terminate the process

**Symptom:** After typing `/exit` in the TUI, the broker continues running and
you must press Ctrl-C.

**Cause:** If the TUI receives the shutdown signal but the client doesn't
disconnect cleanly, the process may linger.

**Fix:** Press Ctrl-C to send SIGINT. The broker will shut down and write any
in-flight history before exiting.

---

## Chat history file is very large

**Symptom:** The chat file at `/tmp/<room-id>.chat` (or your custom path) has
grown very large.

**Fix:** The host can run `/clear` to truncate the history file. Note this
permanently deletes all previous messages. For a non-destructive approach,
archive the file externally and replace it with an empty file while the broker
is stopped.

---

## Broker on encrypted volume is slow to start

**Symptom:** Tests fail intermittently with timeouts, or the broker takes
several seconds to accept the first connection.

**Cause:** Binaries on APFS-encrypted volumes (e.g. mounted FileVault drives)
can take >300 ms to execute on first run.

**Fix:** In shell scripts, use `sleep 2` between starting the broker and
connecting clients. In integration tests, use a generous polling loop —
`TestBroker::start()` already polls for socket readiness.
