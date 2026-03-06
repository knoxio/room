Poll the shared room for new messages and summarise them before continuing.

Check the project CLAUDE.md or AGENTS.md for the room ID and your session token, then run:

```bash
room poll <room-id> --token <token>
```

If no broker is running, `room poll` still works — it reads the chat file directly. After polling, briefly summarise any new messages that are relevant to your current work before proceeding.
