# Development

- **Builds.** Only release builds are used: `cargo build --release`, `cargo test --release`, or `docker build --target test .` to run the tests in Docker. The toolchain version is pinned in `rust-toolchain.toml`.
- **CI.** Every pull request to `main` must pass the `CI` workflow before it can merge:
  - `test-linux`: build and tests on Linux.
  - `build-windows`: build on Windows.
  - Both treat compiler warnings as errors.
  - Both build only when the pull request changes code: `crates/`, the Cargo files, `rust-toolchain.toml` or the workflows. For a pull request that changes no code, they report as skipped and it can still merge.
- **Dev loop.** `scripts/iterate.ps1` does the following:
  - rebuilds and restarts the server container;
  - launches the client into the game;
  - waits (`-Seconds`, default 70);
  - prints the run's unhandled calls, server errors and client exceptions.

  `-NoBuild` skips the rebuild.
- **Tools.**
  - `marshal-dump [--check] <file>...` decodes `blue.marshal` files. `--check` re-encodes them and reports only the status.
  - `blue-patch [--check] <bin64/blue.dll>` is the patcher used by `patch-client.ps1`.
- **Contributing.** Contributor and coding-agent guidelines are in [AGENT.md](../AGENT.md).

## Repository layout

| Path | Contents |
|---|---|
| `crates/server` | The server (`infinity-server` binary) |
| `crates/server/lua` | Lua scripts the server loads at start (see [docs/lua.md](lua.md)) |
| `crates/server/src/services` | Remote services the client calls (`sm.RemoteSvc(name).Method(...)`), one module per feature area |
| `crates/server/sql` | `game_schema.sql` (applied at every start) and `market_seed.sql` |
| `crates/server/retail` | Data captured from the retail server for this build (CDN content, cached machoNet calls) |
| `crates/marshal` | `blue.marshal` codec, plus the `marshal-dump` tool |
| `crates/sde-import` | Loads static data bundles (JSONL) into PostgreSQL |
| `crates/lp-import` | Operator tool that imports NPC corporations' LP store offers from ESI (see [docs/lp-import.md](lp-import.md)) |
| `crates/blue-patch` | Patches the client's `blue.dll` so an edited `start.ini` still loads |
| `db/` | Per-table database dumps that are loaded into a fresh database |
| `scripts/` | Database init and dump, client patching and launching, and the dev loop |

## Releases

Tagged versions are published on the repository's GitHub releases page. Each release contains:

- `infinity-<version>-x86_64-linux.tar.gz`
- `infinity-<version>-x86_64-windows.zip`
- `SHA256SUMS`

Each package holds `infinity-server`, `sde-import`, `marshal-dump`, `blue-patch`, the README and the `docs` folder.

To release, a maintainer pushes a tag `vMAJOR.MINOR.PATCH` that points at a commit on `main`, for example `git tag v0.1.0 && git push origin v0.1.0`. Tags with any other name do not start a release.
