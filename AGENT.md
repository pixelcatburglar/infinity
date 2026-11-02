# AGENT.md: development handbook

These rules apply to anyone changing this repository, coding agents and humans alike. Read this file before making a change, and follow it for every change. [README.md](README.md) covers setup and operation.

## 1. Non-negotiables

- **No personal or machine-specific information anywhere in the repository.** That covers code, comments, docs, commit messages, test fixtures, and logs pasted into issues or pull requests. Do not include:
  - real names, user names or e-mail addresses
  - absolute local paths or host names
  - tokens or credentials

  Use placeholders such as `<client folder>` or `<name>` instead.
- **Never commit anything derived from a client install.** This includes:
  - `/sde/` and `/client/`
  - anything under `/tools/`
  - client caches, settings, and logs (`/logs/`)

  The only captured data in the tree is `crates/server/retail/`, which is wired in through `crates/server/src/retail.rs`, and the protobuf descriptor set `crates/server/proto/eve_protos.desc`, which the public gateway compiles in.
- **Release builds only.** Use `--release` for every build, test and run. Do not add debug profiles or rely on debug builds. Builds are warning-free; CI denies warnings.
- **The client is the specification.** Argument shapes, return shapes and behaviour come from the client's own Python (`eve/client/script/...`, `eve/common/script/...`, `carbon/common/script/...`) and from carbonengine sources. When the live server's data or logic never shipped with the client (dungeon layouts, LP offers, the store catalogue, the hacking board generator), build the closest faithful substitute and document it in the module docs.
- **Keep diffs minimal and on-topic.** No drive-by refactors, renames or reformatting of code you are not changing. Cleanups that the rules below require (removing a duplicate helper, naming a constant, migrating to the one mechanism) go into a commit of their own, never mixed into a feature commit.
- **One architecture per concern.** Each concern is handled one way across the whole server. When a mechanism replaces another, migrate every caller and delete the old one in the same change: no parallel paths, compatibility shims, aliases, re-exports or flags that keep an old design alive. For example, space state is reached only through `World`, and timed effects in space run only on actor timers (see Shared state and concurrency).
- **No hardcoding.** Logic contains no magic numbers and no literal type, group, attribute or item IDs.
  - Name every value as a `const`. Its doc comment states the source: a client constant (file and name), an SDE ID, a carbonengine value, or a design decision of this server. A constant only one module uses sits at the top of that module. A game-data constant used by more than one module is defined once in `consts.rs`, or in `consts/dogma.rs` for dogma attribute and effect IDs, and imported (see Shared helpers); never redefine it locally, and never give two different meanings the same name (an effect ID and an effect category, seconds and FILETIME units).
  - Values the SDE or dogma data provides are read from that data, not copied. For example, lock time comes from the ship's scan resolution and the target's signature radius, for players and NPCs alike.
  - Deployment settings (addresses, ports, capacities, the database connection) come from `Config` (`crates/server/src/config/`): the `config.toml` file, overridden by the environment variable named in `config.example.toml`. The server reads environment variables nowhere else.
  - Values an operator may tune (starting balances, fees and taxes, timers, limits, spawn ranges and sizes) are fields of the game configuration (`crates/server/src/config/`), each with a default and a comment naming its source, and are read through `config::game()`; they are never a module `const`. Values the client checks, values from the SDE and carbonengine destiny constants stay `const`.
  - Player-visible text and client message names are never written inline; they are defined in the `messages` module (see Player-visible text).
  - Unit conversions go through the shared helpers, never inline factors such as `* 10_000` or `/ 1000`.
  - Exempt: `0` and `1` as identities, array indices, and test fixtures.
- **No duplicated helpers.** Before writing a helper, search for an existing one. A helper needed by more than one module lives in one shared place (see Shared helpers); never copy it into another module.
- **Small files, one concern each.** A Rust source file covers one feature, answers at most one client service, and stays under 800 lines. Tests live in their own files, never in a code file. See File layout and Testing and verification.

## 2. Build, test, run

The server crate embeds `crates/server/proto/eve_protos.desc` at compile time; it is committed (see README).

| Task | Command |
|---|---|
| Build everything | `cargo build --release` |
| All unit tests | `cargo test --release` |
| Tests for one crate | `cargo test --release -p marshal` (packages: `server`, `marshal`, `sde-import`, `blue-patch`) |
| Tests in Docker | `docker build --target test .` |
| Rebuild and restart the server | `docker compose up -d --build server` |
| End-to-end check with the client | `scripts/iterate.ps1` (`-Seconds`, `-AutoSelect`, `-NoBuild`) |
| Inspect a marshal stream | `marshal-dump [--check] <file>...` |

**CI.** Every change reaches `main` through a pull request.
- `main` accepts a merge only when both checks of the `CI` workflow pass on a branch that is up to date with `main`:
  - `test-linux`: release build of every target, and all tests.
  - `build-windows`: release build.
- Both deny warnings (`RUSTFLAGS=-D warnings`) and use the toolchain pinned in `rust-toolchain.toml`.
- The Dockerfile's `rust:` image uses the same version. Change both together.
- Both build only when the change touches code.
  - Code means `crates/` (except Markdown files), `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` and `.github/workflows/`.
  - For any other change, such as docs, `db/`, `scripts/` or Docker files, the checks report as skipped, which satisfies the merge rule.
  - If the workflow cannot tell what changed, it builds.

**Releases.** Pushing a tag `vMAJOR.MINOR.PATCH` (for example `v0.1.0`) on a commit that is on `main` runs the `Release` workflow:
1. It reruns the Linux tests.
2. It packages every workspace binary for x86_64 Linux and Windows, plus `SHA256SUMS`.
3. It publishes them as a GitHub release.

Any other tag starts nothing.

## 3. Code map

| Area | Where |
|---|---|
| Listeners and startup | `crates/server/src/main.rs` (`Config::from_env`, one task per connection) |
| Server logs | `logging.rs` (stdout, plus time-rotated files when `logging.dir` is set; `main` keeps the writer guards until exit so the last lines are flushed) |
| Login handshake | `handshake.rs` (GPS, Placebo crypto), `live_sessions.rs` (one live session per account) |
| Packet loop, session changes, notifications | `client.rs`, `session.rs`, `macho.rs`, `conn.rs`, `framing.rs` |
| Remote service dispatch | `services/mod.rs` (`dispatch`), one module per feature under `services/`, and `services/fixed.rs` (constant answers for services that are not modelled) |
| Lua scripts | `lua/` (VM, typed values, host functions and reload), `dao/lua_store.rs` (`game.lua_store`); the shipped scripts are in `crates/server/lua/`, and `docs/lua.md` documents the script API |
| Server-side dogma | `services/fit.rs` (effective attributes), `services/dogma.rs` (dogmaIM), `services/effects/` (effect registry, activation lifecycle and the effect families; see `docs/effects.md`) |
| Space simulation and destiny | `space/mod.rs` (`World` facade, `Ball`, destiny encoding, integration and interpolation, pilot registry and jump queues), `space/system.rs` (`System`: one system's state, destiny clock, action queue, tick, timers), `space/actor.rs` (per-system mailbox and the wall-aligned 100 ms loop), `space/npc.rs` (NPC brains, spawn groups, effects worker), `space/drone.rs` (drone orders, control and automatic engagement; drones use the NPC effects worker), `space/snapshot.rs` (periodic pilot position saves and the shutdown flush) |
| Scheduled jobs | `scheduler/` (job registration, the claim runner, retries and the shutdown drain), `dao/jobs.rs` (`game.jobs`); handlers register in `services/mod.rs` |
| Public gateway (gRPC) | `gateway/mod.rs`, `gateway/handlers.rs` |
| Chat | `chat.rs` and `chat/` (XMPP transport, connected occupants and stanzas); `services/chat/` (`XmppChatMgr`, room admission and moderation authority, moderation expiry); `dao/chat_channels.rs` |
| Image server and content CDN | `images.rs`, `cdn.rs` |
| Captured retail data | `retail.rs` and `crates/server/retail/` |
| Database access and schema | `db/mod.rs` (`Db`: pool, statement methods, startup), `db/transaction.rs` (`Db::transaction`), `db/executor.rs` (`Executor`), `db/outbox.rs` (`Outbox`), `crates/server/sql/game_schema.sql` |
| Static data (SDE) | `sde/mod.rs` (`Sde`: loaded once at startup, read-only, shared as `Arc<Sde>`; one module per dataset such as `types`, `dogma`, `map`, `industry`, `missions`), `dao/sde/` (the only static-data SQL, run once by the loader) |
| Game data access (all `game.*` SQL) | `dao/`: one module per kind of data (`accounts`, `characters`, `items`, `wallets`, `skills`, `market`, `contracts`, ...); a feature's statements on shared data live in `dao/<data>/<feature>.rs` |
| Player-visible text and client message names | `messages.rs` and `messages/<feature>.rs` |
| Wire format | `crates/marshal` |

## 4. Conventions

### Module docs and comments

- Every module starts with a `//!` block that explains four things:
  - what the module covers;
  - which client modules it mirrors (with their paths);
  - which tables or data sources it uses;
  - what is **not** supported.

  Update this block whenever behaviour changes.
- Comments state facts and reasons the code can't show: protocol details, client file and function references, formulas and their sources. Don't narrate what the code obviously does.
- Comments, test names and file names stand on their own. Never cite internal planning labels (decision, work-item or pull request numbers such as `D3`, `W13`, `PR7`) or tool jargon; state the fact or reason the label stood for.
- Code, comments, log messages and docs are written in English.

### File layout

- **One feature per module.**
  - A module covers one feature or mechanism, such as fleets, industry jobs or the space tick.
  - Two features never share a file.
  - Code that two features need goes to a shared home (see Shared helpers), not into one of them.
- **One client service per file.** All `(service, method)` arms of a client service, such as `fleetMgr` or `industryManager`, sit in one file, and that file serves no other service.
  - A feature the client reaches through one service is one file, `services/<feature>.rs`.
  - A feature with several services is a directory, `services/<feature>/`, holding:
    - `mod.rs`: the module doc, the feature's shared types, state and queries, and a `dispatch` that only routes each service to its file.
    - `<service>.rs`: that service's handlers, named after the service in snake case (`industry_manager.rs`).
    - Mechanics that answer no service, such as timers, formulas or order matching: each in its own file, named after the mechanic.
  - The one exception is the table of fixed answers for services the server does not model yet (Remote services, step 5). It holds constant answers only. Once a service needs logic, its arms move to a feature module.
- **`mod.rs` files route and share; they do not implement.** `services/mod.rs` holds the module declarations, the dispatch chain and the shared helpers. It answers no service itself.
- **Size.** Keep Rust source files under 500 lines where practical. No file passes 800 lines, and that includes test files.
  - A change that would take a file past 800 lines first splits it, by service or by sub-feature. The split is a commit of its own that only moves code and does not change behaviour. The change then goes on top.
  - Tests are never part of a code file (see Testing and verification).
  - Exempt: captured retail data under `crates/server/retail/`, SQL files and generated code.
- **The same rules apply outside `services/`.** A file in `space/`, `gateway/` or the crate root covers one concern. When it outgrows the limit or picks up a second concern, it becomes a directory module.
- **A split leaves one path.**
  - After a split, callers import from the new location. No re-export or alias keeps the old path alive (see One architecture per concern).
  - Items are only as visible as their callers need: `pub(super)` before `pub`.

### Remote services

1. Find the caller in the client: the service, the method, the arguments, and what the client does with the result.
2. Handle the call in the matching module's `dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult>` as a `(service, method)` match arm. Return `None` for calls the module does not own, so dispatch falls through to the next module.
3. If the feature needs a new module, add `mod <name>;` and a `dispatch` call to the fallthrough chain in `services/mod.rs`. A new client service gets its own file (see File layout).
4. `retail::cached_call` answers first. Do not shadow a captured retail answer unless you replace it on purpose.
5. If a feature isn't modelled but the client needs an answer, add an empty but well-formed constant answer to `services/fixed.rs`, in the function for its position in the dispatch chain, with a comment saying what the client reads from it.
6. Unhandled calls log `UNHANDLED <service>::<method>` and return `None`. Make that line go away by handling the call. Never suppress the warning.

### Lua scripts

- Scripts in `crates/server/lua/` can claim remote calls, GM commands and events. A claimed call is answered after `retail::cached_call` and before every Rust module, and the claims are logged when the scripts load. `docs/lua.md` documents the script API.
- Event handlers receive a plain table of scalar values, never a live context: an event is delivered after the change that caused it has committed, in the order the changes were made. Interceptors claim one exact service and method; they run after `retail::cached_call` and before Lua call claims and every Rust module, and a refusal reaches the client as a `UserError`.
- Host functions under `lua/host/` call the same service functions as the Rust handlers, with their transactions, outbox notifications and `World` operations. A new host function calls an existing service function; never write a second version of a game rule for scripts.
- Scripts get no SQL. A script's own persistent data goes through the `store` host functions, which keep typed values per script in `game.lua_store` (`dao/lua_store.rs`).
- Lua is compiled from source by `mlua` (`vendored`). Do not add a dependency on a system Lua or on any other system library.

### Values on the wire

- Build results with the helpers in `services/mod.rs` (`keyval`, `rowset`, `crowset`, `item_descriptor`) and with `marshal::Value` (`Value::instance`, `Value::reduce`, `Value::dict`, `Value::list`, `Value::tuple`).
- `Value::str` is a Python 2 `str`. Use it for identifiers, keys and class names.
- `Value::unicode` is a Python `unicode`. Use it for any text a player reads.
- Client times are FILETIME values: 100 ns units since 1601, from `client::filetime_now()`.
- Destiny event stamps are destiny ticks, read from `System::current_time()`: each system's clock starts at the Unix second it loads and advances by one per one-second evolution. Never derive a stamp from wall-clock milliseconds (`space::now_ms()`, `System::now_ms()`); those only schedule the actor loop, timers and interpolation. Never send milliseconds as a destiny stamp.

### Errors

- `CallError::User { msg, dict }` raises a client `UserError`. `msg` is the error name the client knows, for example `CustomNotify` with a `notify` text, or `TargetingAttemptCancelled`.
- `CallError::Internal` (usually via `?` on `anyhow`) is a server fault. Never use it for a normal game refusal.

### Player-visible text

- The Infinity client runs in Simplified Chinese. Any text a player sees, such as notify messages, mail, agent lines and default names, is written in Simplified Chinese, matching the existing strings.
- Every such text is defined once in the `messages` module (`crates/server/src/messages.rs` and its children), and call sites refer to it by its module path.
  - Text that predates the children stays in `messages.rs`, in the section for its feature (`messages::NAME`).
  - GM slash-command text (help lines, replies and refusals) lives in `messages/gm.rs` (`messages::gm::NAME`).
  - Any other new text goes in its feature's child, `messages/<feature>.rs` (`messages::<feature>::NAME`), declared with one `pub(crate) mod <feature>;` line in `messages.rs`. New text is not added to `messages.rs` itself, and nothing is re-exported.
  - Plain text is a `const`.
  - Text that carries values is a function in the same file as its texts that returns `String`; no other file formats player-visible text.
  - Client message names (the keys of the client's message catalogue passed to `user_error`, `CallError::User` or a notification, such as `CustomNotify`) are `MSG_` consts in their own section of `messages.rs`, one catalogue shared by every feature.
  - Reuse an existing entry when the meaning is the same; add a new one when it differs.
  - Not messages: names read from the SDE or written by players, and protocol identifiers such as service, method and notification names (`OnItemChange`).
- Everything else (code, comments, logs, docs) stays in English.

### Shared state and concurrency

- Module-level runtime state uses `static STATE: OnceLock<Mutex<State>>` behind a `state()` accessor. Keep critical sections short. Static data is not runtime state: never cache it in a module, read it from `Sde`.
- Never hold a `std::sync::Mutex` guard across `.await`.
- Every solar system in space is an actor: a tokio task that owns its `System`. Its loop runs on every whole 100 ms of Unix time and drives timers, NPC decisions and the mailbox; ball motion advances only on whole seconds, once per destiny tick (carbonengine `Ballpark::Evolve`). Reach it only through `World`:
  - `world.with(system, move |s| ...).await` runs a closure inside the actor and returns its value (`None` if the system is not running).
  - `world.post(system, move |s| ...)` sends a write and does not wait. It can be called from synchronous code.
  - `world.schedule(system, delay_ms, move |s| ...)` runs a closure inside the actor on the first tick after the delay. `world.after(system, delay_ms).await` waits for that tick from async code (`false` if the system is not running). Every timed effect on something in space uses one of these: target locks, module cycles and effect durations, reloads, missile flight, jump delays, scans and probes, site and belt respawns, CONCORD response. Never `tokio::spawn` + `sleep` for them.
  - Work outside space that must survive a restart (market order expiry, auction settlement, bills, chat mute and ban expiry, and any new deadline kept in the database) is a scheduled job: register an idempotent handler and call `scheduler::schedule`/`cancel`. Scheduling inside a transaction wakes the runner from the post-commit dispatch. `tokio::spawn` + `sleep` is only for chat connection lifetimes and for character timers that follow the pilot across systems and stations and need not survive a restart (Crimewatch).
  - `with`, `post` and `schedule` never load a system. Code that puts something into a system (`add_ship`, `observe`, `World::system`) loads it first.
- Rules for closures that run inside a system, including `command`, on-cleared and timer closures:
  - Never `.await`, never touch the database, and never call `World`. Closures must be `Send + 'static`: use `move` and capture owned values.
  - Copy out only what the caller needs, such as a few balls or scalar facts. Never clone every ball of a system.
  - Hand database or other async work to `System::defer(async move { ... })`. The future runs on the system's worker, outside the actor, and may use the database and `World`.
- Ball state that carbonengine destiny changes through an action (movement, speed, max speed, mass and agility, cloak, missile launch, and the AddBalls/RemoveBalls notices for balls entering or leaving) changes only through the system's action queue: `System::command` / `World::command`, or the `System` method for that action (`launch_missile`, `add_ship`, `remove_ball`, `uncloak`). An action issued while the system holds tick C is stamped C and applies before the step to C + 1. Never change a ball's motion state directly from a timer or a service.
- `Ball::pos` and `Ball::vel` are the state at the current destiny tick; the state stream, position snapshots and the position saved at logout use them. Anything that runs between ticks and needs where a ball is (ranges, NPC decisions, scans, warp and gate checks, where a destroyed ball leaves its wreck or capsule) reads `System::sample_position(id)`, `System::sampled_ball(id)` or `Ball::position_at(ms)`, which interpolate the last completed tick as the client renders it; `System::destroy_ball` returns the ball at its sampled position. Never integrate in smaller steps.
- Look pilots up through the pilot index (`System::pilots`, `System::pilot_ball`), not by scanning every ball.
- NPCs are `NpcBrain`s owned by their system and decide inside the tick. Their shots and repairs run on the system's worker through `NpcEffects`. NPCs that must be cleared together form a spawn group (`System::spawn_group`), whose on-cleared closure runs once when the last member leaves. Do not add polling loops that watch the space.
- Long-running background tasks (`tokio::spawn`) log failures with `warn!` and never panic.

### Shared helpers

Helpers and constants used by more than one module, each defined once:

- `services/mod.rs`: `CallError` builders (`db_err`, `user_error`, `notify_error`), random numbers (`rand01`, `rand_dir`), reading a field of a client `Value`, and other service-level queries shared by several services.
- `space/mod.rs`: vector math on `V3` (`add`, `sub`, `scale`, `len`, `dist`), the wall clock (`now_ms`) and unit conversions (`ms_to_seconds`, `seconds_to_ms`). Destiny stamps are not conversions of the wall clock; they come from `System::current_time()`.
- `client.rs`: FILETIME conversions (`filetime_now` and conversions from unix time).
- `consts.rs` and its child `consts/dogma.rs`: game-data constants used by more than one module, grouped by source. Dogma attribute and effect IDs live in `consts/dogma.rs` and are imported as `crate::consts::dogma::...`; `consts.rs` holds the rest: FILETIME durations, distances, SDE type, group and category IDs, inventory flags, locations and row columns, corporation roles and wallet account keys. Protocol constants that are part of one module's interface stay in that module, for example `macho::cmd` and the space ball modes and flags.
- `dao/`: statements used by several features, in the module of their data (see Database).
- `sde/mod.rs`: `Sde::celestial_radius`, the one collision radius of a celestial (its own radius, then its type's, then one documented fallback), as the `f32` destiny sends. Static balls and every placement next to a celestial widen that value; never apply a separate fallback or cast.
- Game formulas used by several callers (such as lock time) are one function in the module that owns the mechanic, called by every user, players and NPCs alike.

### Database

- `Db` owns a pool of connections and is the only way to reach the database.
  - For a single statement, call `db.query`, `db.query_one`, `db.query_opt` or `db.execute`. Each borrows one pooled connection for that statement.
  - Never hold a connection outside `Db`.
- **SQL lives in `dao/`.** No `game.*` statement is written outside `crates/server/src/dao/`; services call dao functions.
  - A dao function takes `ex: &(impl Executor + ?Sized)` and returns `Result<_, db::Error>`, so the same function runs on `Db` or inside a transaction. It never opens a transaction, notifies a client, touches `World` or the outbox, or builds wire values; the service does those.
  - Rows are read by column name into a named struct with one `from_row` per row shape, never by position. Give computed columns an alias so they have a name. A presence check is `query_opt(...).await?.is_some()`.
  - Data used by several features lives in the dao module of that data (`dao/items`, `dao/characters`, `dao/wallets`, `dao/skills`, ...). A feature that adds statements there does so in a file named after the feature. Search the module before adding a statement; never write a second copy.
  - SQL assembled from fragments lives in `dao/` together with its builder. Services pass typed filter values, never SQL text.
  - Static data (`sde.*`, `sde_native.*`, `sde_compat.*`) is read only by the loaders in `dao/sde/`, once at startup, into `Sde`. Services read it from `&Sde`: synchronous, no lock, no await, no query. A statement never joins `game.*` with static tables; the dao returns game rows and the service enriches them from `Sde`.
- **One business operation, one transaction.** An operation that writes more than one row, or more than one table, runs in `db.transaction(operation, dispatch)`.
  - `operation` gets the transaction and an `Outbox`. The runner commits, or rolls back on error.
  - The runner reruns `operation` once on a deadlock or serialization failure, so `operation` must be safe to repeat: capture inputs before the call and rebuild any mutable state inside it.
  - `dispatch` runs after COMMIT and receives the outbox.
  - Dao functions take an `Executor`; service helpers that queue effects also take the outbox. Both work inside a transaction and on their own, and never open a transaction themselves.
- **Notify after commit.** Inside `operation`:
  - Never notify a client, call `World`, or change memory state directly. Queue the effect in the outbox; a rolled-back operation sends nothing.
  - Never `.await` on `World` or on a client. Read what the operation needs from `World` before the transaction starts.
- **Guarded writes.** A write that depends on a value read earlier checks it in SQL, for example `UPDATE ... SET quantity = quantity - $2 WHERE item_id = $1 AND quantity >= $2`. The same applies to a status transition (`AND status = $old`) and a balance (`AND balance >= $2`).
  - The guarded write is the dao function that performs it. It returns the affected count, an `Option` or a `bool`; the service turns a miss into the player-visible refusal and rolls the transaction back.
  - Quantity changes to existing item rows go through the shared plan in `dao/items/quantities.rs`.
- **Lock order.** An operation that writes several rows of one table writes them in ascending primary key order.
- **Economy operations** (wallets, market, contracts, industry, corporation and alliance fees, bills, offices, the LP store) take `market::lock()` before opening the transaction and hold it until `dispatch` has run. Do not add row locks on orders or wallets.
- `game_schema.sql` runs on every server start (`Db::connect`), so every statement must be idempotent and safe on an existing database:
  - `CREATE TABLE IF NOT EXISTS`, `CREATE INDEX IF NOT EXISTS`, `CREATE SEQUENCE IF NOT EXISTS`
  - `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`
  - guarded backfill `UPDATE`s
- Never edit or remove an existing statement in a way that breaks a database created by an earlier version. Add new statements to the relevant section, with a comment naming the owning module.
- The `dao/sde/` loaders read the typed `sde.*` tables, and the raw `sde_native` / `sde_compat` `jsonb` tables only for data that has no typed table yet. Load only the tables and fields the server reads; a new static read adds a field to a dataset in `sde/`, never a runtime query.
- Never edit the dumps under `db/` by hand. Regenerate them with `scripts/db-dump.ps1`, in a commit of their own.
- Persist what players would expect to survive a restart. If state is kept in memory on purpose, say so in the module docs.

### IDs

Stay inside the existing ranges, and pick a fresh, non-overlapping range for any new kind of ID.

| IDs | Range start | Source |
|---|---|---|
| Accounts | 10,000,000 | `game.accounts_user_id_seq` |
| Player characters | 90,000,000 | `game.characters_character_id_seq` |
| Player corporations | 98,000,000 | `game.corporations_corporation_id_seq` |
| Player items | 1,000,000,000,000 | `game.items_item_id_seq` |
| Trade containers | 1,000,000,000,000 (shared with player items) | `game.items_item_id_seq`: a trade's ID comes from the item sequence, so it never collides with an item ID |
| Spawned NPCs (temporary) | 9,000,000,000,000 | `missions::NEXT_NPC_ID` |
| Exploration sites and wormholes | 9,500,000,000,000 | `game.exploration_site_seq` |
| Probes (temporary) | 9,600,000,000,000 | `exploration::NEXT_PROBE_ID` |
| Asteroids | 9,700,000,000,000 | `game.asteroid_seq` |
| Missiles in flight (temporary) | 9,800,000,000,000 | `combat::MISSILE_ID_START` |

### Logging

- Use `tracing`.
- `info!(user = c.login.user_id, ...)` records notable player actions.
- `warn!` records failures that the server survives.
- `debug!` is for noisy detail.
- Never log passwords, password hashes or chat authentication tokens.

## 5. Testing and verification

- **Tests live in their own files, never in a code file.**
  - A module's unit tests go in a `tests.rs` beside it:
    - `foo.rs` → `foo/tests.rs`
    - `foo/mod.rs` → `foo/tests.rs`
    - a crate root (`lib.rs`, `main.rs`) → `src/tests.rs`, as the marshal crate already does
  - The module declares them with one line at its end: `#[cfg(test)] mod tests;`.
  - A code file holds no test code: no inline `mod tests { ... }` block, and no test-only helpers, fixtures or other `#[cfg(test)]` items. Test helpers belong in the test file.
  - As a child module, `tests.rs` already sees the module's private items through `use super::*`. Never widen an item's visibility for a test.
- Test behaviour a client or player would notice: wire encoding round-trips, formulas, boundaries, state transitions and error cases. Do not write tests that only restate constants or wiring.
- Protocol and gameplay changes must be exercised with the real client. Run `scripts/iterate.ps1`, walk through the changed path, and confirm the run adds none of the following:
  - `UNHANDLED` calls
  - `failed` or `ERROR` lines
  - client exceptions

  State what you exercised in the pull request.
- Schema changes must work on both an existing database and a fresh volume.

## 6. Commits and pull requests

- Make one logical change per commit.
- Start the message with a short imperative summary line. Use the body to explain why, and which client behaviour the change matches.
- Update the module `//!` docs, the README (features, limitations, configuration) and this handbook when behaviour or workflow changes.
- Do not commit generated or local artifacts: `target/`, logs, client files, editor or tool state.
- A pull request merges only when CI passes (see Build, test, run).
