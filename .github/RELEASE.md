# Release process

## Steps

1. **Update the version** in two places:
   - `Cargo.toml`: `version = "X.Y.Z"`
   - `.claude-plugin/marketplace.json`: `"version": "X.Y.Z"` in the `room` plugin entry

2. **Run tests locally:**
   ```bash
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test
   ```

3. **Commit and tag:**
   ```bash
   git add Cargo.toml Cargo.lock .claude-plugin/marketplace.json
   git commit -m "release vX.Y.Z"
   git tag vX.Y.Z
   git push origin main --tags
   ```

4. **CI takes over:** the `release.yml` workflow builds binaries for all three platforms, generates release notes from commit history, and attaches the binaries + `SHA256SUMS` to a GitHub Release.

5. **Verify the release** at `https://github.com/joaopcmiranda/room/releases/latest`.

## Platforms built

| Artifact | Target |
|----------|--------|
| `room-macos-arm64` | `aarch64-apple-darwin` |
| `room-macos-x86_64` | `x86_64-apple-darwin` |
| `room-linux-x86_64` | `x86_64-unknown-linux-gnu` |

## Version authority

The `Cargo.toml` version and `marketplace.json` version must match the tag.
The `plugin/.claude-plugin/plugin.json` intentionally has no version — for relative-path
plugin sources the marketplace entry is the version authority (see plugin docs).
