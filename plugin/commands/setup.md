Install the room binary. Try installation methods in order until one succeeds.

## Step 1: Check if already installed

```bash
which room && room --version
```

If found, report the version and stop — nothing to do.

## Step 2: Try cargo install (Rust toolchain present)

```bash
which cargo && cargo install --git https://github.com/joaopcmiranda/room room
```

## Step 3: Try downloading a pre-built binary from GitHub releases

Detect the platform and download the appropriate binary:

```bash
# Detect platform
uname -sm
```

Then download from https://github.com/joaopcmiranda/room/releases/latest:
- macOS Apple Silicon: `room-macos-arm64`
- macOS Intel: `room-macos-x86_64`
- Linux x86_64: `room-linux-x86_64`

```bash
# Example for Apple Silicon — adapt based on uname output above
curl -L https://github.com/joaopcmiranda/room/releases/latest/download/room-macos-arm64 -o /usr/local/bin/room && chmod +x /usr/local/bin/room
```

Verify after installation:

```bash
room --version
```

Report which method succeeded, or if all failed, ask the user to check https://github.com/joaopcmiranda/room for manual installation instructions.
