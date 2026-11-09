# Database

The database has four schemas:

- `sde` holds the typed, indexed static data tables.
- `sde_native` and `sde_compat` hold the raw static data bundles, one `jsonb` table per file.
- `game` holds everything players create: accounts, characters, items, the market, mail and so on.

`game.crate_rewards` records one receipt per reward item ID, with the opening character, crate type, destination location and flag, opening FILETIME and nullable first-confirmation FILETIME. Receipts are written in the same transaction as the reward items. They have no foreign key to items and survive a reward item's deletion or merge.

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

`game.ship_dirt` stores one FILETIME per physical ship item, initialized on the first authorized read and atomically replaced by a reset. Its item foreign key cascades on deletion; ownership changes do not reset it. It is independent of owner-bound bay settings.

`game.ship_skin_licenses` stores dynamic licenses by the composite character and SKIN primary key; `expires_at` is a FILETIME, or NULL for permanent. `game.ship_skin_applications` is keyed by the actual ship item, with an item foreign key and a composite license foreign key, both cascading on deletion. Ownership changes and repackaging clear applications atomically but retain licenses. Ordinary SKIN definitions, license types, full compatible hull lists and regional flags come from static data loaded once at startup.

The existing `game.jobs` scheduler keeps the nearest finite SKIN expiry per character; grants, upgrades and removals reschedule it to the nearest finite expiry, and cancel it only when no finite licenses remain. No separate SKIN cache or notice log table is used. The schema can be applied on fresh or existing databases and repeatedly; rollback retains the stored data, but consumed license items are not automatically reissued.

`game.wallet_accounts` gives each owner and wallet key a stable account ID; it holds no balance. `game.market_transactions` stores the buyer's and seller's account IDs (NULL for an NPC side) and the matched order. `game.wallet_journal` records market cash movements and charged direct skill purchases, with their amounts as `numeric(36,2)` and the balance after each as `numeric(37,2)`, in the same transaction as the money. A sale's gross row shows the balance before tax, which can exceed the wallet cap by at most one tax amount, hence the extra digit; character and corporation wallet balances stay `numeric(36,2)` and remain authoritative. Applying the schema fills in account IDs for earlier personal fills without inventing journal rows for them.

`game.market_orders` and `game.market_order_history` retain the issuer in `char_id`, nullable `corporation_id`, `wallet_key`, and nullable `source_location_id` and `source_flag_id`. Personal and NPC rows use NULL corporation, cash key and no source; corporate rows require an issuer and a valid division key, with both source fields for sells and neither for buys. Ownership checks enforce these shapes. Partial corporation/order and corporation/wallet/order indexes support active queries, and corporation/closed/order indexes support history. Accepted immediate buys receive unique order-sequence IDs and terminal history rows, including their actual remainder.

Transaction buyer and seller IDs identify actors, while their account IDs join `game.wallet_accounts` to the real financial owner and key. Corporate transaction reads project each authorized account side independently, so a cross-division fill can yield two rows without duplicating the stored fill or public volume. Legacy rows retain NULL corporation and the cash default; existing personal account backfill does not infer corporate intent, historical fees, cash movements or journals. Schema additions are designed for repeated application to existing databases; an older personal-only settlement binary must not be deployed while corporate orders exist. Preserve the ledger and settle with owner-aware code instead of clearing orders or destructively rolling back.

`game.progress_events` journals durable game events in the transaction that produces them, one row per event, keyed by its source: the natural row ID where the event has one, otherwise the transaction invocation and ordinal. `game.progress_subscriptions` and `game.progress_subscription_kinds` record the registered consumers, and `game.progress_deliveries` holds one receipt per consumer and event, acknowledged in the same transaction that applies it. Rows are not deleted automatically. See [progress events](progress-events.md).

`game.skill_plans` stores ordered personal plan steps. `game.skill_plan_milestones` stores their targets and cascades on plan deletion; `game.skill_plan_milestone_progress` stores derived viewer snapshots, not authoritative trained skills. `game.skill_plan_tracking` stores one personal or certified target per character, with no row for cancellation. Startup migrates the legacy active column without selecting among conflicting active plans; conflicts and structurally invalid rows stop migration with their identifiers. After SDE loading, obsolete or otherwise invalid legacy definitions are logged without rewriting them. Skill changes are journalled and consumed through the progress delivery mechanism; startup, login and milestone reads recover snapshots.

Personal plan names and descriptions are limited to 50 and 1000 UTF-16 code units respectively, matching the client's narrow Python Unicode ABI. Startup checks existing text using the same units and reports the plan and character on conflict; it never truncates stored text.

Milestone event reconciliation takes a character lock, reads initial definitions, locks their sorted parents, rereads definitions, then reads skills and queue and performs one batch upsert when targets exist. Durable delivery locking and acknowledgement are separate platform work. A specific-plan read or creation uses the same reconciliation after the existing training timeline has advanced and released its market lock; it does not query once per milestone or load fit solely for reconciliation.

Native prerequisite cycles do not prevent startup. A stored milestone whose target reaches a cycle is audited and reconciled to an invalid, incomplete snapshot with zero SP; `valid` is a derived database field, not a client wire field. Delivery is acknowledged normally.

Lifecycle:

- **First start.** `scripts/db-init.sh` loads `db/<schema>/<table>.sql.gz` into an empty volume. Tables whose foreign keys aren't satisfied yet are retried until everything is loaded.
- **Every server start.** The server applies `crates/server/sql/game_schema.sql`, loads the captured retail prices, and seeds the NPC market if it is empty. Schema replay never moves an ID sequence backwards. It then reads the static data it uses from `sde`, `sde_native` and `sde_compat` into memory once; after startup the server never queries those schemas, so static data changes take effect at the next start. Scheduled jobs are kept in `game.jobs`: jobs that fell due while the server was down run right after the start, and a job left half-done by a crash runs again once its lease expires, so every job must be safe to repeat.
- **Reset.** Remove the database volume to start over (`docker compose down`, then `docker volume rm infinity_pgdata`). The next `docker compose up` imports the dumps again.
- **Rebuild the static data.** Put the static data JSONL bundles (`eve-online-client-static-data-<build>-jsonl` and `eve-online-static-data-<build>-jsonl`) under `./sde/`, run `docker compose run --rm sde-import`, then restart the server.
- **Refresh the dumps.** `scripts/db-dump.ps1` replaces the folders under `db/` with dumps of the running database.
