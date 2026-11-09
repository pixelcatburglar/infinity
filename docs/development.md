# Development

- **Builds.** Use release builds only and the toolchain pinned in `rust-toolchain.toml`. Treat compiler warnings as errors (`RUSTFLAGS=-D warnings`). For the local test-compilation lane, run:

  ```sh
  env CARGO_PROFILE_RELEASE_OPT_LEVEL=3 CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo build --release --locked --workspace --all-targets
  env CARGO_PROFILE_RELEASE_OPT_LEVEL=3 CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo test --release --locked --workspace
  ```

  These command-scoped overrides must not be exported globally. They change only release LTO for this lane: optimization level 3, 16 codegen units, assertions, overflow checks and panic behavior remain unchanged. The lane uses the same target directory and linker; it does not claim to reduce memory use. `[profile.test]` does not control `--release`, and disabling LTO is not a `cargo check` speedup.

  Lane-built binaries are for tests and local runs only, never deployment. Team gates, including startup log comparisons, may use them, but log comparison is not performance verification. Deployment binaries come from the default `cargo build --release`, Docker builds or release packaging, without the overrides; the root release profile remains opt3/thin/16. Do not deploy a binary left by the lane's all-target build. No thin rebuild is required after each test gate. This local lane does not change existing CI, Docker or release test commands.

`infinity-core` owns `consts`, `messages` and `session`; server callers import them from `infinity_core`. Its session tests live beside their owner in `crates/core/src/session/tests.rs`. Use `cargo test --release --locked -p infinity-core` with the same command-scoped test-lane overrides to run them. A server-only test command does not run dependency crates' unit tests; workspace all-target builds include the new library and its tests. Runtime configuration, clocks and identity remain in the server crate.
- **Database tests.** The tests marked `#[ignore]` need a database: point `INFINITY_TEST_DATABASE_URL` at a separately initialized one. Compile them before acquiring the dedicated database lock:

  ```sh
  env CARGO_PROFILE_RELEASE_OPT_LEVEL=3 CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo test --release --locked --workspace --no-run --message-format=json
  ```

  Capture Cargo's `compiler-artifact` messages and select the exact reported test executables. Under the dedicated database lock, invoke those executables directly with the relevant test filter and `--ignored --test-threads=1`; do not use executable globs or run Cargo compilation or `cargo test` while holding the lock. Tests of the scheduler runner and of the fixed periodic seed refuse to start while the database has scheduled or running jobs they did not create; the industry and skill seeding tests also refuse while it has industry or skill state they would seed. Use a dedicated, quiet database; never clear jobs or player state from a live or shared one to make the tests pass.

  Database fixtures that schedule jobs must remove only their own exact job IDs,
  including after a caught scenario failure, before deleting the referenced rows.
  Do not assume the jobs table is globally empty.

  Mining fixtures that create a World prepare fields through the database and
  publish them to the system actor, never through process-global state or a
  private cache. They delete their own rocks durably before cleaning up the
  actor's metadata and balls. Run the Lua undock tests together with the effects
  and event tests, keep the assertion that natural asteroid balls are visible
  when undock returns, and do not hide readiness problems with sleeps or by
  isolating the test process.

  For a dedicated initialized database, the adaptation filter is
  `cargo test --release --locked -p server ship_adaption -- --ignored --test-threads=1`
  with `INFINITY_TEST_DATABASE_URL` set. For the locked lane above, compile first
  and apply that filter to the reported server test executable under the lock.
  Publisher invocation uses `cargo run --release --locked -p sde-import --bin
  adaption-content -- --input <decoded-bundle> --client-constants
  <constants-file> --output <external-root> --publish`; see [setup](setup.md).
  Backend tests do not prove client UI behavior. The maintainer walkthrough
  covers the adaptation window, ordinary injection, exchange entry if reachable,
  tree and mission views, and refusals; it has not been exercised in this
  environment. No UI pass is claimed.

  For deposit candidate regressions, apply the `lock_candidates` filter to the
  reported server test executable with `--ignored --test-threads=1` under the
  dedicated database lock. These tests require an initialized database with SDE
  data. This instruction does not claim a test result.

- **Inventory item locks.** For affected hull and ordinary-container contents
  operations, capture the complete set of existing write targets and lineage
  rows before the first item lock, then use the shared `destination::lock_items`
  plan. Lock hulls in ascending item ID order before nonhulls in ascending order,
  each once at its final strength; never acquire additional existing rows or
  upgrade a lock later. Each distinct cold source hull and direct ordinary
  quota parent gets UPDATE initially; new lineage-only nonhulls use SHARE,
  while existing stronger custody intents remain. Revalidate captured parent
  metadata, closure, lineage and access after locking. Source quota parents
  participate when reserving, completing, returning or recovering reloads,
  and in relevant trash and repackage settlement. `ParentGuard` only
  revalidates snapshots against that same token; it does not authorize access.

  For a cold deposit into a real item destination, capture its exact optional
  merge candidate before locking. If it no longer qualifies, insert a new stack
  rather than choosing or locking another existing stack later. Type or
  hull-classification drift still refuses, as does drift in a mandatory source
  or parent. Station and system destinations are classified from `Sde` without
  a query; an authorized office uses the existing `Id` and `office_owner`
  classification once. Prepared deposits and the wrapper share that
  classification, with no station candidate or helper work and unchanged
  output statement counts.
  External pre-lock drift still inserts a new stack. Repeated writes for the
  same destination and type share one preparation, advanced only by their
  actual receipts. A receipt can contain both a merged row and a newly created
  row, in that order; its added quantities sum to the requested amount. Progress
  follows the actual new row when one is created, otherwise the merged row.
  Subsequent writes guard that exact row; only a verified full row can advance
  to another new row. A fresh transaction attempt resets progress to the
  immutable pre-lock capture.


  One DAO update-or-insert CTE handles both candidate supply modes: the captured
  candidate for a cold item destination, or selection inside the statement when
  the destination is excluded or its hull UPDATE lock is already held. The
  latter uses the existing `LockedItems` proof from `capacity` or
  `check_destination`, not a fabricated hull or another merge path. Actual IDs
  returned by INSERT in the same transaction have new-row provenance and can be
  outside the preexisting-row token.

  A single physical row and one requested quantity are bounded by `i64::MAX`;
  totals across physical rows and independent reservations use `i128`. Item
  locks precede sorted pending-reload locks. Source changes, receipt rows and
  outbox effects belong to the same transaction; a refused attempt publishes
  none of them.

  The server's ordinary-box `GetCapacity` reply reads parent, direct children and
  source reservations in one tagged UNION snapshot without a writer lock,
  comparing the parent with previously authorized metadata. The reply's snapshot
  can be stale after return. Writes read all children and source reservations under the
  initially UPDATE-held parent and use the same checked aggregation of
  nonnegative nonsingletons by type and net comparison; singleton and negative
  quantity rows retain their individual own-volume semantics. `Replaced` validates
  actual item and reload IDs, membership, type and quantities; duplicate or
  missing credit is refused. A mixed refund validates the complete group,
  credits only matching source reservations, and requires incoming quantities
  at least as large as the credited quantities. Uncredited arrivals need room.
  Only hull admission returns a held-hull token; a box never fabricates one or
  loads instance fitting. Existing hull effective capacity and hold rules stay
  intact.

  Structurally, a normal ordinary-box admission performs two accounting reads:
  direct children and source-pending reservations. Selected box unload keeps
  its pre-mutation check and the shared deposit check, for four accounting reads
  per module in this cold manual action. These counts exclude existing metadata,
  authorization and lock queries; they are not measured transaction costs or
  timings. Nonempty refund reservation locking is a cold sorted acquisition
  after item locks. Native direct-hull and static paths and held periodic paths
  add no generic box accounting, captures or extra fitting work.

  Automatic reload and direct-hull completion reuse their original hull fence
  and business reads without generic preplanning. Manual external-box completion
  is a cold delayed action: capture and revalidate the box's current ancestry,
  not an old routing hint. Trash without a refund does not discover refund
  destinations. Existing station, office, corporate deliveries and synthetic
  economic outputs remain outside the contents-lock plan under the existing
  market lock; fuel and weapon hot paths are unchanged.
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

  **Ordinary-container client verification:** Feed a current production inventory
  capture or production-encoder artifact to the original client Python bytecode
  inventory consumer, identifying which source supplied the input. Execute the
  original bytecode, not decompiled text or an AST, for its capacity and precheck
  path and record the actual objects, types, values and decisions.
  Compare the physical rows returned by `List` and the client's physical used
  space with the separate server capacity reply, including pending reservations.
  Correlate a pending-reservation case where the local precheck allows a move but
  server admission refuses it, and check that refusal changes neither state nor
  notifications. Retain the run record identifying the source, input and
  backend used.

  Where decoding requires native behavior, the original Python must call the
  matching real native backend or a reviewed backend built from matching source.
  Do not substitute fake Blue or DBRow objects. Projections, static inspection
  and reverse engineering are diagnostic only. Partial method-body diagnostics
  do not establish complete native decoding, precheck, cache or UI compatibility.
  Track each unexecuted required native path as blocked, with its reason and the
  conditions for unblocking it when a matching host is available; it is not a
  full pass.

  Inventory quantity client verification uses three artifacts: a test capturing
  production output as JSON, a script feeding that input to original client
  bytecode, and a run record identifying the source revision and changes,
  bytecode manifest and input checksums, case results and unexecuted native
  paths. The capturing tests print each record on one line prefixed
  `CLIENT_QUANTITY_INPUT_V1`; run them with `--nocapture` to collect the input.
  This does not by itself establish GUI behavior or a native-path pass; record
  actual results and blockers separately.

  The public GM reload source remains ship or station; it is not a box-source
  success path.
- **Tools.**
  - `marshal-dump [--check] <file>...` decodes `blue.marshal` files. `--check` re-encodes them and reports only the status.
  - `blue-patch [--check] <bin64/blue.dll>` is the patcher used by `patch-client.ps1`.
- **Contributing.** Contributor and coding-agent guidelines are in [AGENT.md](../AGENT.md).

## Repository layout

| Path | Contents |
|---|---|
| `crates/server` | The server (`infinity-server` binary) |
| `crates/core` | `infinity-core`: shared game constants, player-visible messages and session data; depends only on `marshal` |
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
