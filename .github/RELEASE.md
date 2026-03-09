# Release process

## Steps

1. **Update the version** using `cargo-release`:
   ```bash
   cargo release <version> --execute
   ```
   This updates all `Cargo.toml` files in the workspace, commits, tags, and pushes.

2. **Run tests locally** (before release, if not using `cargo release`):
   ```bash
   cargo check
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test
   ```

3. **Manual alternative** (if not using `cargo release`):
   ```bash
   # Update version in crates/room-cli/Cargo.toml, crates/room-protocol/Cargo.toml,
   # crates/room-ralph/Cargo.toml
   git add -A && git commit -m "release vX.Y.Z"
   git tag vX.Y.Z
   git push origin master --tags
   ```

4. **CI takes over:** the `release.yml` workflow builds binaries for all three platforms, generates release notes from commit history, and attaches the binaries + `SHA256SUMS` to a GitHub Release.

5. **Verify the release** at `https://github.com/knoxio/room/releases/latest`.

## Platforms built

| Artifact | Target |
|----------|--------|
| `room-macos-arm64` | `aarch64-apple-darwin` |
| `room-macos-x86_64` | `x86_64-apple-darwin` |
| `room-linux-x86_64` | `x86_64-unknown-linux-gnu` |

## Version authority

All three workspace crates (`room-cli`, `room-protocol`, `room-ralph`) have
independent versions in their respective `Cargo.toml` files. Tags follow
the primary crate (`room-cli`) version.
