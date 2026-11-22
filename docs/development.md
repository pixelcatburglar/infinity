# Development

- **Builds.** Use release builds only and the toolchain pinned in `rust-toolchain.toml`. Treat compiler warnings as errors (`RUSTFLAGS=-D warnings`). For the local test-compilation lane, build only the test binaries:

  ```sh
  env CARGO_PROFILE_RELEASE_OPT_LEVEL=3 CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo test --release --locked --workspace --no-run
  ```

  `--no-run` compiles the workspace test targets without executing them; the ordinary server binary is not rebuilt or delivered on each local gate, and deployment is unchanged. To also run the non-database tests in the same lane, repeat the command without `--no-run`. Build the server binary itself with `cargo build --release` only when a local run needs it. When a compile check over every target is needed, run `cargo check --release --locked --workspace --all-targets`; it type-checks all targets and reports warnings without producing linked artifacts.

  These command-scoped overrides must not be exported globally. They change only release LTO for this lane: optimization level 3, 16 codegen units, assertions, overflow checks and panic behavior remain unchanged. The lane uses the same target directory and linker; it does not claim to reduce memory use. `[profile.test]` does not control `--release`, and disabling LTO is not a `cargo check` speedup.

  Lane-built binaries are for tests and local runs only, never deployment. Team gates, including startup log comparisons, may use them, but log comparison is not performance verification. Deployment binaries come from the default `cargo build --release`, Docker builds or release packaging, without the overrides; the root release profile remains opt3/thin/16. Do not deploy a binary left by the test lane. No thin rebuild is required after each test gate. This local lane does not change existing CI, Docker or release test commands.

`infinity-core` owns `consts`, `messages` and `session`; server callers import them from `infinity_core`. Its session tests live beside their owner in `crates/core/src/session/tests.rs`. Use `cargo test --release --locked -p infinity-core` with the same command-scoped test-lane overrides to run them. A server-only test command does not run dependency crates' unit tests; workspace test builds include the new library and its tests. Runtime configuration, clocks and identity remain in the server crate.
`infinity-content` owns the retail-capture CDN payloads and cached machoNet calls, the verified achievement catalogue and the UUID text encoding; server callers import them from `infinity_content`. Its unit tests live beside their owners under `crates/content/src` and run independently with `cargo test --release --locked -p infinity-content` and the same command-scoped test-lane overrides. The retail payloads stay single-sourced under `crates/server/retail/`; the content crate includes them by relative path, with no copy or regeneration.
`infinity-wire` owns packet socket framing, framed marshal connections and machoNet packets and addresses; server callers import them from `infinity_wire`. The transport's default write timeout is defined once in `infinity_wire::conn` and reused by the server configuration. The configured timeout still overrides that default when a connection is created.

Its framing and connection unit tests live beside their owners under `crates/wire/src` and use ordinary `#[cfg(test)]` declarations. Run them with `cargo test --release --locked -p infinity-wire` and the same command-scoped test-lane overrides. A server-only test command does not run these dependency-crate unit tests; workspace test builds include them. Client, handshake, chat and configuration tests remain in the server, as do runtime configuration, clocks and session lifecycle management.

- **Server test domains.** Server unit tests compile in sixteen domains selected by Cargo feature. A test module declaration names the domain of its source path: `#[cfg(all(test, feature = "tests-<domain>"))] mod tests;`.

  | Feature | Source paths under `crates/server/src` |
  |---|---|
  | `tests-effects` | `services/effects`, `services/fit`, `services/fitting`, `services/dogma`, `services/dynamic_items` |
  | `tests-combat` | `services/combat`, `services/drones`, `services/crimewatch`, `services/bounty` |
  | `tests-ships` | `services/ship`, `services/ship_cosmetics_mgr`, `services/ship_adaption`, `services/clones`, `services/repair` |
  | `tests-inventory` | `services/inventory`, `services/items`, `services/item_volume`, `services/item_launch` |
  | `tests-skills` | `services/skills`, `services/skill_plans`, `services/expert_systems`, `services/subscriptions` |
  | `tests-economy` | `services/market`, `services/contracts`, `services/industry`, `services/mining`, `services/trade`, `services/wallets`, `services/corpwallet`, `services/bills`, `services/store`, `services/crates`, `services/plex_vault`, `services/insurance` |
  | `tests-social` | `services/corps`, `services/alliances`, `services/offices`, `services/fleets`, `services/chat`, `services/mail`, `services/presence`, `services/standings`, `services/wars`, `services/access_groups`, `services/bookmarks` |
  | `tests-content` | `services/sovereignty`, `services/daily_goals`, `services/exploration`, `services/missions`, `services/hacking`, `services/planet_mgr`, `services/agency`, `services/fw_victory_point_mgr`, `services/map`, `services/universe` |
  | `tests-slash` | `services/slash` |
  | `tests-services` | every other `services/` path, including `services/tests.rs` and `services/tests/` |
  | `tests-space` | `space` |
  | `tests-lua` | `lua`, `tests/lua_events` |
  | `tests-storage` | `dao`, `db`, `progress`, `scheduler` |
  | `tests-gateway` | `gateway` |
  | `tests-sde` | `sde` |
  | `tests-runtime` | everything else remaining in the server, including client, handshake, chat, configuration, the root `tests.rs` and the shared test helpers |

  A listed stem covers both `P.rs` and the `P/` directory, and matches whole path components only, so `ship_adaption` is not part of `ship`. Domains are physical path partitions, not feature areas: the tests of the skills service belong to `tests-skills`, but the matching data access is `tests-storage`, the gateway module is `tests-gateway`, the static data is `tests-sde`, and a GM command entry under `services/slash` is `tests-slash`. A test mounted across directories belongs to its actual source file's path, not the mounting module's name. A cross-layer change selects the union of the domains it touches; for example a change spanning the skills service, its dao, the gateway skill plans, the SDE definitions and the GM entry point selects `tests-skills,tests-storage,tests-gateway,tests-sde,tests-slash`. Domains never enable each other. One deliberate exception: `images/tests.rs` is mixed-domain — its socket-bound request tests are `tests-runtime` while the CDN checkpoint test that exercises the verified retail catalogue is `tests-sde`, and the file's parent mount is `any(tests-runtime, tests-sde)`.

  Domain size is measured by the total lines in physical test source files, including helpers, comments and blank lines. Each physical file is counted once and assigned to its path owner; no domain may exceed 15% of the total lines. This is not a count of registered tests.
  The shared ship-change deadline is tested directly on the live pilot registry in `tests-space`. Board, Store, BoardStored and Eject fixtures register through the past fixture clock in `space/session_change/test_support.rs`, compiled for `tests-space` and `tests-ships`; they do not sleep or bypass the admission check. The CONCORD ticket check is in `tests-combat`: a reserved response ticket is pursuit, and a suspect flag alone is not. Foreign boarding custody and its proof are in `tests-ships`; the lock completion race against a reserved transition is in `tests-combat`.

  To compile only the tests of one domain, keep the usual test-lane overrides and select the feature:

  ```sh
  env CARGO_PROFILE_RELEASE_OPT_LEVEL=3 CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo test --release --locked -p server --no-default-features --features tests-skills --no-run
  ```

  Selection follows ordinary Cargo feature semantics. The default feature is `tests-all`, which enables every domain, so a plain `cargo test` and CI compile every test exactly as before. Passing `--features tests-skills` without `--no-default-features` still compiles everything, because features only add to the selection; a reduced set always needs `--no-default-features`. Listing several domain features selects their union, and explicit `tests-all` or `--all-features` restores the complete build. Selecting no domain at all makes the server test target fail to compile with an error naming the remedy; production builds never depend on these features.

  When adding a test module, declare it under the domain of the longest matching path prefix in the table; a new `services/` subtree falls into `tests-services` and any other unlisted path into `tests-runtime`. A mixed parent container may remain reachable for consumers in several domains, but each new behavioral child is gated separately by the owner of its actual source file. Children already enclosed by a pure owner gate need not repeat it. Forgetting the feature is safe but wasteful: a plain `#[cfg(test)]` declaration compiles in every selection, so a missed domain costs compile time instead of coverage. A test helper shared by several domains is gated per item with `all(test, any(feature = "tests-a", feature = "tests-b"))` over exactly the domains that consume it; that consumer set does not determine the owner of behavioral tests. Selecting one domain never pulls a whole other domain's tests in just to compile a shared helper.

  A workspace-wide `cargo check --release --locked --workspace --all-targets` type-checks every target, including all test code, and catches warnings in one pass, but produces no executable or test list. A domain `cargo test ... --no-run` builds the real server test binary containing only the selected domains; listing tests requires invoking that executable with `--list`, and the binary is what a name filter or the database lane below actually runs. Use the check for a whole-workspace compile gate and the domain binary when a runnable test artifact is needed.

- **Database tests.** The tests marked `#[ignore]` need a dedicated, already initialized database with the SDE imported: point `INFINITY_TEST_DATABASE_URL` at it. Compile the test binaries before acquiring the dedicated database lock:

  ```sh
  env CARGO_PROFILE_RELEASE_OPT_LEVEL=3 CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo test --release --locked --workspace --no-run --message-format=json
  ```

  Capture Cargo's `compiler-artifact` messages and select the exact reported test executables. Under the dedicated database lock, invoke those executables directly with the relevant name filter and `--ignored --test-threads=1`; do not use executable globs or run Cargo compilation or `cargo test` while holding the lock. A libtest name filter only reduces which tests execute within the selected feature set; domains outside the selection are left out of the compile by the Cargo features, not by the filter. Tests of the scheduler runner and of the fixed periodic seed refuse to start while the database has scheduled or running jobs they did not create; the industry and skill seeding tests also refuse while it has industry or skill state they would seed. Use a dedicated, quiet database; never clear jobs or player state from a live or shared one to make the tests pass.

  Ordinary `#[ignore]` database tests still share one process in the serial
  batch above. A test annotated `run this exact test in its own process`
  runs as one complete exact name per process, for example
  `<reported-test-executable> <fully-qualified-test-name> --exact --ignored
  --test-threads=1`, still the compiler-artifact executable run under the
  dedicated lock plus any listener or publication lock it names:
  `--test-threads=1` does not reset `OnceLock` or other process-local state.

  Exclude capture/export tests marked as requiring capture source metadata from ordinary database batches with `--skip <fully-qualified-test-name>`; run them explicitly by exact name only when their declared inputs are available.

  The first call to the synchronous `test_database` helper initializes the schema once per process: it runs the complete production `Db::connect` startup (schema application and cutover, then the usual seed and reprice steps) on its own OS thread with its own runtime, while every concurrent caller waits for the same result. The temporary `Db` and its pool are dropped inside that worker; no connection, runtime or world escapes to be shared, and each test still opens its own `Db`. Initialization failure is sticky: the first and every later call panic with the underlying error, and after fixing the environment the test process must be restarted. One process binds one dedicated database; the URL and dataset never switch mid-run.

  Once initialization has succeeded, the returned configuration carries `schema_on_start: false`, so each test's own `Db::connect` runs migration validation and the remaining startup steps without replaying the schema. A test that needs the replay passes an explicit `schema_on_start: true` on its own connect; `false` still validates, and neither setting undoes the initialization that already ran. The shared configuration also carries `schema_downgrade_guard: false`, so test connects never refuse on recorded schema history; coverage of the guard sets the flag explicitly.

  Test helpers share one `Arc<Sde>` per process: the first caller loads the static data once and later callers clone the same `Arc`. The shared test client and gateway factories both use it. The cache holds owned, read-only data and does not retain the `Db` it was loaded from, so it outlives individual test runtimes; a successfully loaded `Sde` is never reloaded, while a failed or cancelled first load may be retried by a later caller. Do not modify the dedicated database's static data externally while tests run, and a test that itself writes static-data rows must not feed the mutated input into the shared cache: it loads a fresh `Arc<Sde>` and passes it through `client_with_sde` instead of the shared client helper. Each test still creates and cleans up its own fixture rows.
  The raw client factory takes the caller's database, static data, world, configuration and login; it does not load static data. Character selection, session registration and startup stay with the caller. Its listener and peer stay in the caller's original scope.

  The service fixture `create`/`cleanup` helpers return boxed futures, so their state machines are not inlined into every caller's future. The boxed interface is unchanged; `create` now also records a valid in-space system/constellation/region ancestry for the character, and `cleanup` also deletes the character's exact skill-completion job.

  Database fixtures that schedule jobs must remove only their own exact job IDs,
  including after a caught scenario failure, before deleting the referenced rows.
  Do not assume the jobs table is globally empty.

  Run the database tests before any startup comparison. A job-free case on
  an already startup-seeded database needs a clean dedicated clone; never
  delete or back up and restore jobs the fixture did not create, and never
  relax the existing guards. Teardown verifies only the absence of owned
  rows whose keys were captured before the first delete; a refusal or
  rollback compares only the affected rows.

  Mining fixtures that create a World prepare fields through the database and
  publish them to the system actor, never through process-global state or a
  private cache. They delete their own rocks durably before cleaning up the
  actor's metadata and balls. Run the Lua undock tests together with the effects
  and event tests, keep the assertion that natural asteroid balls are visible
  when undock returns, and do not hide readiness problems with sleeps or by
  isolating the test process.
  Annotating a case for its own process-local Lua host does not reclassify
  the undock tests.

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

- **Reward amounts.** `services::corps::tax::settle_reward` accepts an
  `IskAmount` gross in the caller's transaction and outbox. Whole-ISK mission
  rewards use `from_whole_isk`; floating NPC payouts are rounded at the
  producer boundary before conversion. Settlement preserves the exact gross
  and subtracts the rounded tax in cents. Per-reward tax eligibility,
  floating-rate tax rounding, loyalty splitting and postcommit notification
  rules are unchanged.

- **Snapshot gate order.** Operations that need both the economy lock and a
  system snapshot gate acquire the economy lock first and never wait for it
  while holding the snapshot gate. An NPC death context freezes the ordinary
  draw and its reserved item IDs. Without those IDs the death journal business
  does not start. After the death commit is confirmed, the same actor
  finalization turn installs the temporary wreck. The snapshot gate is released
  before the existing map and bounty paths.

- **NPC bounty pools.** Kills mutate actor-owned pools under the short shared
  batch mutex only; pools join the periodic snapshot, dock/transfer/logout and
  payout-final writes through one shared checkpoint receipt. The payout freezes
  the batch, captures every live source and keeps the complete proven durable
  preimage; the character row lock fences each proof transaction. A snapshot
  carrying bounty rows must commit Default, never LossTolerant.

  Ordinary NPC wrecks and their uncollected loot belong only to the current
  solar-system actor. Opening, towing, splitting or merging them does not
  create item rows. Taking loot persists only the quantity taken, through the
  existing item transaction. Successful salvage persists its materials and
  keeps any uncollected ordinary loot in a temporary replacement container.
  These temporary roots refuse deposits of already-persistent items; historical
  wrecks, exploration containers and ordinary GM-created containers retain
  their existing behavior. Uncollected contents are discarded when the actor
  unloads or the server restarts, and no expiry timer or journal-based
  restoration is added.

  The ordinary root and its uncollected loot stay in the actor as fresh plain
  items. They do not join the ship snapshot, sleep, or a shutdown flush. Real
  sequence IDs are reserved in a batch and published only after that batch's
  commit is confirmed. A batch may contain gaps, and unused IDs are never
  recycled. A successful take claims the source, arms loaded holds before
  market and other gates, and inserts only the taken slice in the existing
  item transaction; destination permission, capacity and fit are unchanged.
  Dispatch records the actor disposition before notifying anyone.

  After an ambiguous commit or a cancellation the source stays closed. The
  next real business operation proves itself with the same database
  transaction fence and the complete canonical payload. Current cargo quantity
  or a missing root row is not that proof, and an old move is not rerun.
  Splitting, merging, renaming and a pure memory cleanup use no database.
  Salvage materials are still acquired in an immediate business transaction.
  A mixed cleanup deletes memory only after its persistent part commits.

  An ordinary wreck and its loot add no SQL per kill, and a tractor cycle adds
  none. The item-ID batch worker is a separate cost. Every NPC death still
  uses the existing journal, checkpoint and map SQL, so a kill is not free of
  database work. Reward idempotence and player loss are unchanged.

  Drone launch holds the economy lock without a snapshot gate and retains an
  existing singleton's pre-transaction fitting snapshot through retries until
  commit. Rolled-back attempts publish no actor state or notifications, and
  committed launch inventory changes are addressed to the owning pilot.

  Item-change notifications keep the explicit receiver and the caller's outbox position, and do not reread a provided row. The health payload builder accepts already converted wire coordinates, and each of its three rows reads its own timestamp. Role-only session updates keep corprole, All, HQ, Base, Other; membership session updates keep their own field order, with Base before HQ.

- **Character row before items.** Within a transaction, retain any organization
  admission before character-row locks, and acquire the affected character row
  before checkpoint or inventory item locks. Player loss, stored-ship transitions
  and clone jumps follow this order; existing helper-level checks may reacquire
  that same character row. Existing inventory lock plans retain their
  hull-before-nonhull ordering and final lock strengths.
  In a piloted loss the victim hull row lock and the frozen original loss source
  precede any checkpoint or loaded write; the full loss lock set rechecks the
  same shape, and a rolled-back attempt still proves the source for the rollback
  that returns the victim to Live.

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

  Station reprocessing authorizes its selected output once inside the
  transaction, using reachability captured before it. All materials share the
  authorized ownership and capacity destination; same-location hull outputs
  join the existing item-lock plan and use its held-hull deposit path. Source
  quantities, materials and fees commit together before notifications.

  Probe recovery admits only the current character's non-capsule ship in space
  and returns eligible probes from the current solar system. It reuses the
  current-hull admission before cargo deposits; inventory changes commit
  before probe removal and client notifications.

  Ordinary trash capture classifies each selected root by its true ancestry
  terminal before the transaction; the remote-hangar exception covers only the
  selected root's closure, and locked lineage and `check_many` revalidate it.

- **FILETIME arithmetic.** Deadlines and durations are i64 ticks; the shared helpers own the repeated conversions, so reuse them instead of open-coded arithmetic.
  `client::filetime_day_start` anchors a timestamp to its day start with `at / FILETIME_DAY * FILETIME_DAY`, integer division toward zero, including negative timestamps.
  `client::filetime_deadline_from_seconds_saturating` saturates the seconds-to-ticks multiplication before the deadline addition, so an out-of-range duration clamps.
  `client::filetime_ticks_from_seconds` is the ordinary `seconds * FILETIME_SECOND` multiply and keeps ordinary i64 overflow behavior for callers that already bound their input.
  `config::game::positive_duration` is shared only by fields with the identical rule: a strictly positive value that still yields a representable future deadline.
  Fields with different value text, units, checks or ordering keep their own validation; field keys, diagnostics and their order stay field-specific.
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
| `crates/content` | `infinity-content`: retail CDN content and cached calls, the verified achievement catalogue and UUID text encoding; payload resources remain under `crates/server/retail` |
| `crates/server/lua` | Lua scripts the server loads at start (see [docs/lua.md](lua.md)) |
| `crates/server/src/services` | Remote services the client calls (`sm.RemoteSvc(name).Method(...)`), one module per feature area |
| `crates/server/sql` | `game_schema.sql` (applied at every start) and `market_seed.sql` |
| `crates/server/retail` | Data captured from the retail server for this build (CDN content, cached machoNet calls) |
| `crates/wire` | `infinity-wire`: packet socket framing, framed marshal transport, machoNet packets and addresses, and the shared default write timeout |
| `crates/marshal` | `blue.marshal` codec, plus the `marshal-dump` tool |
| `crates/sde-import` | Loads static data bundles (JSONL) into PostgreSQL |
| `crates/lp-import` | Operator tool that imports NPC corporations' LP store offers from ESI (see [docs/lp-import.md](lp-import.md)) |
| `crates/blue-patch` | Patches the client's `blue.dll` so an edited `start.ini` still loads |
| `db/` | Per-table database dumps that are loaded into a fresh database |
| `scripts/` | Database init and dump, client patching and launching, and the dev loop |

## Build identity

The server binary carries a compile-time identity: the server package version and the commit captured while compiling from `INFINITY_BUILD_COMMIT`, falling back to `dev` when the variable is unset. Neither value is read at runtime. Startup writes one `server build` INFO line with both fields, and the GM command `/version` reports the same values.

A Docker build stamps the commit with `--build-arg INFINITY_BUILD_COMMIT=<sha>`; the `dev` default keeps the image buildable without it. The GitHub release workflow sets `INFINITY_BUILD_COMMIT` to the triggering commit's `github.sha` in the shared binary build step for both platforms. A source tree synchronized out of the original repository can legitimately carry a commit ID that does not exist in that repository's history, so the value is meaningful only together with the build context that produced it; comparing SHAs across different source trees does not prove the sources are the same or different.

## Releases

Tagged versions are published on the repository's GitHub releases page. Each release contains:

- `infinity-<version>-x86_64-linux.tar.gz`
- `infinity-<version>-x86_64-windows.zip`
- `SHA256SUMS`

Each package holds `infinity-server`, `sde-import`, `marshal-dump`, `blue-patch`, the README and the `docs` folder.

To release, a maintainer pushes a tag `vMAJOR.MINOR.PATCH` that points at a commit on `main`, for example `git tag v0.1.0 && git push origin v0.1.0`. Tags with any other name do not start a release.
