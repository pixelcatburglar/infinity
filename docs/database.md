# Database

The database has four schemas:

- `sde` holds the typed, indexed static data tables.
- `sde_native` and `sde_compat` hold the raw static data bundles, one `jsonb` table per file.
- `game` holds everything players create: accounts, characters, items, the market, mail and so on.

`game.crate_rewards` records one receipt per reward item ID, with the opening character, crate type, destination location and flag, opening FILETIME and nullable first-confirmation FILETIME. Receipts are written in the same transaction as the reward items. They have no foreign key to items and survive a reward item's deletion or merge.

`game.ship_skin_licenses` stores dynamic licenses by the composite character and SKIN primary key; `expires_at` is a FILETIME, or NULL for permanent. `game.ship_skin_applications` is keyed by the actual ship item, with an item foreign key and a composite license foreign key, both cascading on deletion. Ownership changes and repackaging clear applications atomically but retain licenses. Ordinary SKIN definitions, license types, full compatible hull lists and regional flags come from static data loaded once at startup.

The existing `game.jobs` scheduler keeps the nearest finite SKIN expiry per character; grants, upgrades and removals reschedule it to the nearest finite expiry, and cancel it only when no finite licenses remain. No separate SKIN cache or notice log table is used. The schema can be applied on fresh or existing databases and repeatedly; rollback retains the stored data, but consumed license items are not automatically reissued.

Lifecycle:

- **First start.** `scripts/db-init.sh` loads `db/<schema>/<table>.sql.gz` into an empty volume. Tables whose foreign keys aren't satisfied yet are retried until everything is loaded.
- **Every server start.** The server applies `crates/server/sql/game_schema.sql`, loads the captured retail prices, and seeds the NPC market if it is empty. It then reads the static data it uses from `sde`, `sde_native` and `sde_compat` into memory once; after startup the server never queries those schemas, so static data changes take effect at the next start. Scheduled jobs are kept in `game.jobs`: jobs that fell due while the server was down run right after the start, and a job left half-done by a crash runs again once its lease expires, so every job must be safe to repeat.
- **Reset.** Remove the database volume to start over (`docker compose down`, then `docker volume rm infinity_pgdata`). The next `docker compose up` imports the dumps again.
- **Rebuild the static data.** Put the static data JSONL bundles (`eve-online-client-static-data-<build>-jsonl` and `eve-online-static-data-<build>-jsonl`) under `./sde/`, run `docker compose run --rm sde-import`, then restart the server.
- **Refresh the dumps.** `scripts/db-dump.ps1` replaces the folders under `db/` with dumps of the running database.
