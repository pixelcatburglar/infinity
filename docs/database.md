# Database

The database has four schemas:

- `sde` holds the typed, indexed static data tables.
- `sde_native` and `sde_compat` hold the raw static data bundles, one `jsonb` table per file.
- `game` holds everything players create: accounts, characters, items, the market, mail and so on.

The three static schemas are read once, at startup, into memory (`Sde`). The running server never queries them; while it runs it reads and writes `game` only.

Locked parent-row metadata comparisons are pure in-memory predicates and issue no SQL and take no locks. Callers still match item identity, decide ordinary jump-clone eligibility, and keep custody checks and refusal errors. A raw parent-to-parent identity comparison keeps exact jump-clone equality, including both sides absent.

`game.retail_prices` is written and read only during startup; after the captured-price upsert, the complete persisted table, including retained older entries, is loaded into the shared static dataset for all runtime price lookups.

Sovereignty development state and replay checkpoints are stored in
`game.sov_development` and `game.sov_development_checkpoints`. Journal indexes
support system and occurrence-ordered replay of the relevant durable events;
the original journal and receipts remain the source of facts.

`game.crate_rewards` records one receipt per reward item ID, with the opening character, crate type, destination location and flag, opening FILETIME and nullable first-confirmation FILETIME. Receipts are written in the same transaction as the reward items. They have no foreign key to items and survive a reward item's deletion or merge.

Repairable recovery uses `game.repairable_recovery_groups`, `game.repairable_recovery_items` and `game.item_service_life`. Physical inventory IDs and recovery group and member business IDs are distinct typed identities. Group and member IDs share `game.repairable_recovery_groups_group_id_seq`, bounded from 10000000000000 through 10049999999999 with `NO CYCLE`. A source item identifies at most one group; an active physical item belongs to at most one recovery member. Historical source and member item references deliberately have no foreign key to `game.items`; a withdrawn member records its replacement inventory ID. Service-life rows instead reference physical items with cascading deletion; an absent row means pristine.

Each member has immutable typed job identity and persisted payment, delivery and withdrawal deadlines managed by `game.jobs`. Due jobs resume after restart without resetting deadlines; RPC time checks and guarded transitions settle races. Fresh and existing databases can apply the schema repeatedly. Before rollback, stop old writers and the job runner and preserve recovery, item, life, wallet and job data; an older server cannot safely manage active custody or reconstruct consumed and recreated items. Do not destructively roll back the schema or discard history to downgrade.

Inventory quantity migration widens 15 scalar columns to `bigint`: `quantity`
in `items`, `market_transactions`, `contract_items`, `store_purchases`,
`redeem_tokens`, `module_charges` and `pending_reloads`, and `vol_entered`,
`vol_remaining`, `min_volume` and `plex_escrow` in each of `market_orders` and
`market_order_history`. It also widens `fittings.fit_qty` from `int4[]` to
`int8[]`, preserving existing array bounds and elements. The guarded migration
accepts the expected old or new type and refuses an unsupported shape.

Stop old server binaries before migration and back up the database. Reconnect
clients and pooled connections after the schema change so prepared statement
descriptors reflect the widened types. An older binary is not a safe downgrade
after migration; restore a compatible backup instead of narrowing live data.

`game.characters.newbie_bonus_claimed_at` is a nullable FILETIME: NULL leaves the character eligible for the one-time spare kit, and a successful grant writes it atomically with the six hangar items. The racial kit comes from the embedded server-authored rookie catalogue shared with provisioning; the independent replacement ship does not set this marker. Existing characters are not backfilled as claimed.

`game.characters.gm_npc_ignored` is a `boolean NOT NULL DEFAULT false` column installed by an idempotent `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`. A `true` value makes the character's piloted ship and the space drones it really owns ineligible NPC targets. The character DAO owns the only setter; positioned drone restore also reads it by joining the real owner's row, and unowned or missing owners read as false. The value belongs to the character row and follows ordinary character deletion; there is no revision or history column.

`game.characters.safety_level` is a `bigint NOT NULL DEFAULT 2` column installed by an idempotent `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, with a `CHECK (safety_level BETWEEN 0 AND 2)` constraint. It persists the character's safety preference: `0` none, `1` partial, `2` full. New and existing characters default to full and no rows are backfilled. The ordinary bound `crimewatch.SetSafetyLevel` call and the GM `/cwatch safety` command share one guarded writer, and status reads go through the same DAO. Rolling the schema change back keeps the column and its stored values; older binaries simply ignore it.

`game.characters` stores `station_id`, `solar_system_id`, `constellation_id` and `region_id` as nullable columns. A character list may show a missing solar system. Selecting that character for the world still requires solar system, constellation and region: a missing coordinate returns a database decode error and is never replaced by zero or reported as an absent character.

`game.ship_dirt` stores one FILETIME per physical ship item, initialized once on the first authorized read or on the shared cold-prepare path for a physical hull, and atomically replaced by a reset through either the cleaning RPC or the GM `/dirt` command. Its item foreign key cascades on deletion; ownership changes do not reset it, and the timestamp belongs to the item, not its owner. It is independent of owner-bound bay settings.

`game.ship_skin_licenses` stores dynamic licenses by the composite character and SKIN primary key; `expires_at` is a FILETIME, or NULL for permanent. `game.ship_skin_applications` is keyed by the actual ship item, with an item foreign key and a composite license foreign key, both cascading on deletion. Ownership changes and repackaging clear applications atomically but retain licenses. Ordinary SKIN definitions, license types, full compatible hull lists and regional flags come from static data loaded once at startup.

`game.lua_content_versions` stores immutable published Lua content packages and `game.lua_content_activations` the append-only publication order per namespace. Both tables are read once at startup into the shared content catalogue and written only by the publication step, which stores new packages and appends activations in its transaction before swapping the in-memory generation; at runtime no code path reads either table. Direct edits take effect only at the next startup, and a package row plus its activation rows must be kept together when restoring data.

The existing `game.jobs` scheduler keeps the nearest finite SKIN expiry per character; grants, upgrades and removals reschedule it to the nearest finite expiry, and cancel it only when no finite licenses remain. No separate SKIN cache or notice log table is used. The schema can be applied on fresh or existing databases and repeatedly; rollback retains the stored data, but consumed license items are not automatically reissued.

`game.wallet_accounts` gives each owner and wallet key a stable account ID; it holds no balance. `game.market_transactions` stores the buyer's and seller's account IDs (NULL for an NPC side) and the matched order. `game.wallet_journal` records market cash movements and charged direct skill purchases, with their amounts as `numeric(36,2)` and the balance after each as `numeric(37,2)`, in the same transaction as the money. A sale's gross row shows the balance before tax, which can exceed the wallet cap by at most one tax amount, hence the extra digit; character and corporation wallet balances stay `numeric(36,2)` and remain authoritative. Applying the schema fills in account IDs for earlier personal fills without inventing journal rows for them.

`game.npc_bounty_pools` holds one character's open NPC bounty batch, keyed by `(character_id, system_id)` so each source system keeps its own absolute amount and kill count. Checkpoints overwrite values absolutely and arm the batch's `game.jobs` payout row in the same transaction; commits use the durable Default mode. There is no historical backfill. The payout writes one wallet journal row per source system (entry type `refBountyPrizes`, and `refCorporationTaxNpcBounties` for the corporation share), including zero-net rows. The schema applies on fresh or existing databases and repeatedly; a restart replays it as a no-op.

`game.market_orders` and `game.market_order_history` retain the issuer in `char_id`, nullable `corporation_id`, `wallet_key`, and nullable `source_location_id` and `source_flag_id`. Personal and NPC rows use NULL corporation, cash key and no source; corporate rows require an issuer and a valid division key, with both source fields for sells and neither for buys. Ownership checks enforce these shapes. Partial corporation/order and corporation/wallet/order indexes support active queries, and corporation/closed/order indexes support history. Accepted immediate buys receive unique order-sequence IDs and terminal history rows, including their actual remainder.

Transaction buyer and seller IDs identify actors, while their account IDs join `game.wallet_accounts` to the real financial owner and key. Corporate transaction reads project each authorized account side independently, so a cross-division fill can yield two rows without duplicating the stored fill or public volume. Legacy rows retain NULL corporation and the cash default; existing personal account backfill does not infer corporate intent, historical fees, cash movements or journals. Schema additions are designed for repeated application to existing databases; an older personal-only settlement binary must not be deployed while corporate orders exist. Preserve the ledger and settle with owner-aware code instead of clearing orders or destructively rolling back.

`game.progress_events` journals durable game events in the transaction that produces them, one row per event, keyed by its source: the natural row ID where the event has one, otherwise the transaction invocation and ordinal. `game.progress_subscriptions` and `game.progress_subscription_kinds` record the registered consumers, and `game.progress_deliveries` holds one receipt per consumer and event, acknowledged in the same transaction that applies it. Rows are not deleted automatically. See [progress events](progress-events.md).

`game.progress_subscriptions` is a startup projection: reconciliation writes the declared consumers and kinds, then reads back the enabled flags once, and receipt creation and polling afterwards use only that snapshot. A manually changed `enabled` flag takes effect at the next startup; consumers disabled during an interval keep their pending receipts but are not polled, and re-enabling does not create receipts for events journaled while disabled. `game.progress_deliveries` also records a capped `permanent_rejections` budget and a `terminal_at` FILETIME: a receipt whose consumer explicitly reports a permanent refusal more than `progress.permanent_retry_limit` times turns `terminal`, is never polled and is never acknowledged as applied. Terminal rows are retained; nothing deletes, applies or reschedules them automatically. Inspect them with:

```sql
SELECT d.consumer_key,d.event_id,d.attempts,d.permanent_rejections,
       d.terminal_at,d.last_error,e.kind,e.source_namespace,e.source_key,
       e.subject_character_id
FROM game.progress_deliveries d
JOIN game.progress_events e ON e.event_id=d.event_id
WHERE d.state='terminal'
ORDER BY d.terminal_at,d.consumer_key,d.event_id LIMIT 100;
```

To restore one, stop the server and scheduler, keep the diagnostic rows, fix the consumer or the data it rejects, then reschedule the exact receipt by primary key:

```sql
BEGIN;
UPDATE game.progress_deliveries
SET state='pending',permanent_rejections=0,terminal_at=NULL,next_attempt_at=0
WHERE consumer_key=:'consumer_key' AND event_id=:'event_id'::uuid
  AND state='terminal'
RETURNING consumer_key,event_id,state,attempts,permanent_rejections,last_error;
COMMIT;
```

The update must return exactly the selected row; zero rows means it is not terminal. `attempts` and `last_error` are preserved, the event row is never edited and a receipt is never set to `applied` by hand. Restarting with a lower limit does not reschedule or terminate rows by itself; the next permanent rejection applies the new limit. A build that predates the `terminal` state cannot read or write these receipts, so do not roll back to one while terminal rows exist.

`game.skill_plans` stores ordered personal and corporation plan steps. Exactly one owner is set: `character_id` for a personal plan, or `corporation_id` plus a 16-byte `category_id` for a corporation plan. Sixteen zero category bytes mean no category. `game.skill_plan_milestones` stores their targets and cascades on plan deletion; `game.skill_plan_milestone_progress` stores derived viewer snapshots, not authoritative trained skills. `game.skill_plan_tracking` stores one personal or certified target per character, with no row for cancellation. Startup migrates the legacy active column without selecting among conflicting active plans; conflicts and structurally invalid rows stop migration with their identifiers. After SDE loading, obsolete or otherwise invalid legacy definitions are logged without rewriting them. Skill changes are journalled and consumed through the progress delivery mechanism; startup, login and milestone reads recover snapshots. The owner and category columns are additive and idempotent. A build that predates them cannot read corporation rows, so prefer a corrected build that preserves the data over a downgrade. A full rollback means a maintenance-window restore of a matching database snapshot and binary; the operator decides, knowing that writes since the snapshot are lost.

Personal plan names and descriptions are limited to 50 and 1000 UTF-16 code units respectively, matching the client's narrow Python Unicode ABI. Startup checks existing text using the same units and reports the plan and character on conflict; it never truncates stored text.

Milestone event reconciliation takes a character lock, reads initial definitions, locks their sorted parents, rereads definitions, then reads skills and queue and performs one batch upsert when targets exist. Durable delivery locking and acknowledgement are separate platform work. A specific-plan read or creation uses the same reconciliation after the existing training timeline has advanced and released its market lock; it does not query once per milestone or load fit solely for reconciliation.

Native prerequisite cycles do not prevent startup. A stored milestone whose target reaches a cycle is audited and reconciled to an invalid, incomplete snapshot with zero SP; `valid` is a derived database field, not a client wire field. Delivery is acknowledged normally.

`game.corporations_corporation_id_seq` is capped at `MAXVALUE 98999999 NO CYCLE`, matching the client's closed `maxPlayerCorporation` range; creating a corporation beyond it is refused to the caller. Before deploying this schema to an existing database, stop old writers, back up the database, and check the sequence and row high-water marks:

```sql
SELECT s.last_value, s.is_called, q.seqmax, q.seqcycle,
       (SELECT max(corporation_id) FROM game.corporations) AS max_corporation_id
FROM game.corporations_corporation_id_seq AS s
JOIN pg_sequence AS q
  ON q.seqrelid = 'game.corporations_corporation_id_seq'::regclass;
```

A sequence `last_value` or corporation row ID above 98999999 blocks the upgrade, including IDs of deleted rows: the schema transaction fails and rolls back without changing the sequence or rows. Preserve the original values, the offending rows and the error, and hand the historical data to operator-approved remediation; do not delete rows, renumber IDs, dial the sequence back or raise the production cap automatically. A sequence at 98999999 with `is_called` true is exhausted but valid: startup succeeds and only further creation is refused. Deployments that run with `schema_on_start=false` must apply the schema explicitly inside a single transaction and stop on any error; running the ALTER and setval statements separately under autocommit is not equivalent to the startup transaction. Downgrading does not mean lifting the cap to keep issuing out-of-range IDs: restore a compatible backup or keep the service down until the approved fix lands.

Lifecycle:

- **First start.** `scripts/db-init.sh` loads `db/<schema>/<table>.sql.gz` into an empty volume. Tables whose foreign keys aren't satisfied yet are retried until everything is loaded.
- **Every server start.** The server applies `crates/server/sql/game_schema.sql`, loads the captured retail prices, and seeds the NPC market if it is empty. Schema replay never moves an ID sequence backwards. A start that applies the schema also records its hash and build commit in `game.schema_application_history` (see Schema history below). It then reads the static data it uses from `sde`, `sde_native` and `sde_compat` into memory once; after startup the server never queries those schemas, so static data changes take effect at the next start. Scheduled jobs are kept in `game.jobs`: jobs that fell due while the server was down run right after the start, and a job left half-done by a crash runs again once its lease expires, so every job must be safe to repeat.
- **Reset.** Remove the database volume to start over (`docker compose down`, then `docker volume rm infinity_pgdata`). The next `docker compose up` imports the dumps again.
- **Rebuild the static data.** Put the static data JSONL bundles (`eve-online-client-static-data-<build>-jsonl` and `eve-online-static-data-<build>-jsonl`) under `./sde/`, run `docker compose run --rm sde-import`, then restart the server.
- **Refresh the dumps.** `scripts/db-dump.ps1` replaces the folders under `db/` with dumps of the running database.

Schema history:

Every start that applies `game_schema.sql` records one row in `game.schema_application_history`: the SHA-256 of the embedded schema text and the build commit. Rows are append-only and ordered by identity, so the newest row is the latest application regardless of timestamps. Replaying the same schema from the same build adds no duplicate: the pair of schema hash and build commit suppresses a second row rather than recording every successful startup.

With `database.schema_downgrade_guard` at its default `true`, a start whose embedded schema hash was already recorded but is no longer the newest refuses before the schema batch runs, and the error names the build commit that applied the newest recorded schema. Setting it `false` is an explicit compatibility opt-out: the refusal is skipped but every successful application is still recorded. `database.schema_on_start = false` bypasses both, computing no hash and touching no history.

The guard's limits are fixed. A binary built before the guard existed cannot enforce it, since only builds carrying the check read history at all. A first start on a fresh or never-recorded database is always accepted, because nothing is recorded to compare against, and an unrelated divergent branch whose schema hash was never recorded cannot be recognized as a downgrade either. Recovery means stopping the old writer and checking a compatible backup; never erase history rows and never narrow the schema to pretend an older one is current.

Every pooled connection keeps a prepared-statement cache: `Db` statements and
the managed transaction facade prepare each distinct SQL text once per
connection and reuse it, so a repeated statement costs one round trip fewer
than its first execution; cold and uncached executions are unchanged. The
startup schema batch and the startup schema migrations bypass the cache, as do
the contract owner query (its row LIMIT is interpolated into the SQL text),
corporation member searches (per-clause generated SQL) and regional best-ask
reads (the generic plan misestimates their input rows). Verified connection
recycling and the no-statement-retry policy are unchanged.

## Commit durability

Ordinary transactions retain PostgreSQL's configured commit policy. The server can opt in individual loss-tolerant transactions by issuing `SET LOCAL synchronous_commit = off` immediately after BEGIN. It does not change global or session settings, and the setting ends with that transaction, including rollback.

The approved writes are periodic ship and drone position and condition snapshots, their frozen combat facts, and the separate NPC map-event projection. A combat snapshot inserts its source journal and initial pending deliveries together; consumers still claim and apply receipts in ordinary synchronous transactions. Ownership, money, skills, scheduler control, receipt updates, NPC death settlement and actor-boundary writes are not asynchronous. Sovereignty capture checkpoints remain synchronous because their lock helper can initialize an ownership row.

Acknowledgement of an asynchronous commit is not confirmation that WAL reached durable storage. A PostgreSQL crash, an operating-system crash, power loss or an immediate PostgreSQL shutdown can lose recent whole asynchronous transactions. The normal WAL-writer risk window is up to three times `wal_writer_delay`, about 600 ms with the default 200 ms setting. The actor's existing snapshot interval is a separate window; periodic failures and skipped saves are not bounded by the WAL-writer delay. A later synchronous write commit flushes the earlier WAL it depends on, preserving a consistent recovery prefix.

A game-server exit does not stop PostgreSQL. PostgreSQL continues flushing WAL, and a clean PostgreSQL shutdown flushes pending WAL. Final snapshot writes use the ordinary commit policy, but an empty final flush is still a no-op: it does not make earlier asynchronous commits durable. There is no extra shutdown WAL barrier or forced rewrite of clean state.

Set `database.loss_tolerant_async_commit = false`, or `DATABASE_LOSS_TOLERANT_ASYNC_COMMIT=false`, and restart to disable every opt-in. This requires no schema rollback and does not change any PostgreSQL setting or retroactively confirm earlier asynchronous commits. Keep the ordinary PostgreSQL commit policy and `fsync` enabled for durable business operations. See [PostgreSQL asynchronous commit](https://www.postgresql.org/docs/current/wal-async-commit.html) for the durability model.

## Integrity checks

`crates/server/sql/integrity_checks.sql` is a standalone operator audit; the server never loads or runs it. Run it by hand against an initialized database:

```sh
PGOPTIONS='-c default_transaction_read_only=on -c statement_timeout=5000' psql "$DATABASE_URL" -X -v ON_ERROR_STOP=1 -f crates/server/sql/integrity_checks.sql
```

Each of the ten independent SELECTs reports one invariant: item locations in the player band missing a real item or durable trade parent, item location cycles through real items, item owners absent from every player and static identity set, item quantities outside the singleton and plastic-wrap shapes, `module_charges`, `weapon_banks` and `insurance` rows whose ship item is gone, entity scheduler jobs whose closed-set name target no longer exists, live characters with a missing or foreign-owned selected ship, and live characters with no solar system.

Every statement returns `check_name`, `primary_key`, `explanation`. A non-empty check emits one summary row first (`primary_key` NULL, `explanation` = `total_count=N`, the count before sampling) then at most 100 detail rows sorted by key. A clean check returns zero rows; a timeout, missing table or permission error is a failure, not a clean result. Statements run in autocommit snapshots, so the report is not one consistent point in time and may run online.

Boundaries are deliberately narrow. The parent check covers only client item-band locations (>= 100000000) and treats real trade IDs, the complete recovery business-ID band (10000000000000 through 10049999999999) and brain-in-body locations (`location_id` = `owner_id`, flags 88/89) as legal non-item parents without verifying the referenced entity exists; below-band locations such as static stations and negative contract escrow are not audited. The quantity check accepts singleton -1 (original) and -2 (copy) and plastic-wrap negative volumes without blueprint-type or research validation, and reports negative non-singleton stacks while making no health claim about a zero stack. Owner existence includes retained soft-deleted and static identities, so no `deleted` filter applies there; only the two character checks exclude deleted characters, and a NULL `ship_id` before first selection is legal. The entity-job check audits only the kind + name shapes enumerated in the SQL file; unknown or malformed job names are not validated, and jobs whose targets may legally be deleted before the runner acknowledges them (kill-right, sovereignty, war-invite and chat-restriction expiries, plus periodic maintenance jobs) are out of scope. There is no wallet check: legal balance writes such as insurance premium and payout go through plain debit and credit paths that record no journal row, and gross or escrow journal snapshots are intermediate values, so comparing the balance to the newest journal row cannot be made reliable.

Results are for investigation only: they never repair data, delete rows or dial sequences back. Rolling back the checks means removing the file and its test; it never touches business data.
