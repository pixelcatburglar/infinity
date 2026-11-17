-- Operator integrity checks for an initialized database. Ten independent
-- read-only statements, run by hand, for example:
--   PGOPTIONS='-c default_transaction_read_only=on -c statement_timeout=5000' psql "$DATABASE_URL" -X -v ON_ERROR_STOP=1 -f crates/server/sql/integrity_checks.sql
-- The server never loads this file. Each statement runs in its own autocommit
-- snapshot, so the report is not one consistent point in time.
-- Every statement returns exactly three columns: check_name text,
-- primary_key text, explanation text. A non-empty check emits one summary row
-- first with primary_key NULL and explanation total_count=N, where N is the
-- real number of offending rows before sampling, followed by at most 100
-- detail rows ordered by primary_key with collation C. A clean check returns
-- zero rows. A timeout, missing-table or permission error is a failure, not a
-- clean result. Keep the output and the data state for investigation.
-- Results are for operator investigation only: never auto-repair, delete
-- rows, or dial sequences back. Item IDs are never recycled.
-- Known exclusions: wallet balance is not compared to the newest
-- wallet_journal row because legal balance writes such as insurance premium
-- and payout go through plain debit and credit paths that record no journal
-- and gross or escrow journal snapshots are intermediate values, so no such
-- comparison can be made reliable. Volatile job targets that handlers may
-- delete before the runner acknowledges the job are likewise excluded:
-- kill-right-expiry, sovereignty destruct, war invites and chat restriction
-- expiry jobs, plus all periodic maintenance jobs.
-- Semicolons appear only as statement terminators, never inside comments or
-- strings, so a trivial split on them always yields whole statements.

-- item_parent_missing: a client-range location (IsPlayerItem >= 100000000)
-- that matches no real items row and is not a durable non-item parent.
-- Excluded shapes are legal locations, not verified parents: trade container
-- IDs share the items sequence and live in game.trades, the recovery ID band
-- holds repairable group and member business IDs, and brain items sit at
-- location_id = owner_id under flags 88 (flagBooster) and 89 (flagImplant).
-- Static stations, low-ID special locations and negative contract escrow
-- locations stay below the band and are not checked here.
WITH offenders AS MATERIALIZED (
    SELECT i.item_id::text AS pk
    FROM game.items AS i
    WHERE i.location_id >= 100000000
      AND NOT EXISTS (SELECT 1 FROM game.items AS p WHERE p.item_id = i.location_id)
      AND NOT EXISTS (SELECT 1 FROM game.trades AS t WHERE t.trade_id = i.location_id)
      AND i.location_id NOT BETWEEN 10000000000000 AND 10049999999999
      AND NOT (i.location_id = i.owner_id AND i.flag_id IN (88, 89))
),
sample AS (SELECT pk FROM offenders ORDER BY pk COLLATE "C" LIMIT 100)
SELECT check_name, primary_key, explanation FROM (
    SELECT 0 AS ord, 'item_parent_missing' AS check_name, NULL::text AS primary_key,
           'total_count=' || count(*) AS explanation FROM offenders HAVING count(*) > 0
    UNION ALL
    SELECT 1, 'item_parent_missing', pk,
           'player-item-band location has no item or durable trade parent' FROM sample
) AS r ORDER BY ord, primary_key COLLATE "C";

-- item_location_cycle: an item whose location chain of real items rows
-- returns to itself. UNION deduplication terminates the walk. Only actual
-- cycle members are reported, never items that merely descend into a cycle.
-- No depth cap: the recursion runs until the closure is exhausted.
WITH RECURSIVE edges AS MATERIALIZED (
    SELECT child.item_id AS origin_id, parent.item_id AS next_id
    FROM game.items AS child
    JOIN game.items AS parent ON parent.item_id = child.location_id
),
walk(origin_id, next_id) AS (
    SELECT origin_id, next_id FROM edges
    UNION
    SELECT w.origin_id, e.next_id FROM walk AS w JOIN edges AS e ON e.origin_id = w.next_id
),
offenders AS MATERIALIZED (
    SELECT DISTINCT origin_id::text AS pk FROM walk WHERE origin_id = next_id
),
sample AS (SELECT pk FROM offenders ORDER BY pk COLLATE "C" LIMIT 100)
SELECT check_name, primary_key, explanation FROM (
    SELECT 0 AS ord, 'item_location_cycle' AS check_name, NULL::text AS primary_key,
           'total_count=' || count(*) AS explanation FROM offenders HAVING count(*) > 0
    UNION ALL
    SELECT 1, 'item_location_cycle', pk,
           'item location chain of real items returns to itself' FROM sample
) AS r ORDER BY ord, primary_key COLLATE "C";

-- item_owner_missing: owner_id outside OWNER_UNOWNED = 1 and every authority
-- the owner resolver consults. Deleted and static identities stay legal
-- owners, so no deleted filter is applied anywhere. Low IDs are not
-- whitelisted and owner 0 is not special-cased.
WITH offenders AS MATERIALIZED (
    SELECT i.item_id::text AS pk
    FROM game.items AS i
    WHERE i.owner_id <> 1
      AND NOT EXISTS (SELECT 1 FROM game.characters AS e WHERE e.character_id = i.owner_id)
      AND NOT EXISTS (SELECT 1 FROM game.corporations AS e WHERE e.corporation_id = i.owner_id)
      AND NOT EXISTS (SELECT 1 FROM game.alliances AS e WHERE e.alliance_id = i.owner_id)
      AND NOT EXISTS (SELECT 1 FROM sde.npc_characters AS e WHERE e.character_id = i.owner_id)
      AND NOT EXISTS (SELECT 1 FROM sde.crp_npc_corporations AS e WHERE e.corporation_id = i.owner_id)
      AND NOT EXISTS (SELECT 1 FROM sde.chr_factions AS e WHERE e.faction_id = i.owner_id)
),
sample AS (SELECT pk FROM offenders ORDER BY pk COLLATE "C" LIMIT 100)
SELECT check_name, primary_key, explanation FROM (
    SELECT 0 AS ord, 'item_owner_missing' AS check_name, NULL::text AS primary_key,
           'total_count=' || count(*) AS explanation FROM offenders HAVING count(*) > 0
    UNION ALL
    SELECT 1, 'item_owner_missing', pk,
           'owner is in no character corporation alliance or static identity set' FROM sample
) AS r ORDER BY ord, primary_key COLLATE "C";

-- item_quantity_shape: quantities outside the stored shapes writers produce.
-- Ordinary singletons carry quantity 1, -1 is SINGLETON_QUANTITY (blueprint
-- original) and -2 is BLUEPRINT_COPY_QUANTITY. Type 3468 (plastic wrap) encodes
-- volume as a negative quantity bounded by int32. A non-singleton zero stack
-- is deliberately not reported: existing readers accept it, but this check
-- makes no health claim about it. Wrap research state and blueprint -2 type
-- validation are out of scope.
WITH offenders AS MATERIALIZED (
    SELECT i.item_id::text AS pk
    FROM game.items AS i
    WHERE (NOT i.singleton AND i.quantity < 0)
       OR (i.singleton AND i.type_id = 3468 AND i.quantity NOT BETWEEN -2147483647 AND -1)
       OR (i.singleton AND i.type_id <> 3468 AND i.quantity NOT IN (1, -1, -2))
),
sample AS (SELECT pk FROM offenders ORDER BY pk COLLATE "C" LIMIT 100)
SELECT check_name, primary_key, explanation FROM (
    SELECT 0 AS ord, 'item_quantity_shape' AS check_name, NULL::text AS primary_key,
           'total_count=' || count(*) AS explanation FROM offenders HAVING count(*) > 0
    UNION ALL
    SELECT 1, 'item_quantity_shape', pk,
           'quantity breaks the singleton or wrap quantity rules' FROM sample
) AS r ORDER BY ord, primary_key COLLATE "C";

-- module_charge_ship_missing: a persisted charge whose ship item is gone.
-- The composite key is reported as ship_id:flag_id.
WITH offenders AS MATERIALIZED (
    SELECT c.ship_id::text || ':' || c.flag_id::text AS pk
    FROM game.module_charges AS c
    WHERE NOT EXISTS (SELECT 1 FROM game.items AS i WHERE i.item_id = c.ship_id)
),
sample AS (SELECT pk FROM offenders ORDER BY pk COLLATE "C" LIMIT 100)
SELECT check_name, primary_key, explanation FROM (
    SELECT 0 AS ord, 'module_charge_ship_missing' AS check_name, NULL::text AS primary_key,
           'total_count=' || count(*) AS explanation FROM offenders HAVING count(*) > 0
    UNION ALL
    SELECT 1, 'module_charge_ship_missing', pk,
           'module charge ship item does not exist' FROM sample
) AS r ORDER BY ord, primary_key COLLATE "C";

-- weapon_bank_ship_missing: a weapon bank whose ship item is gone. The row
-- primary key is slave_id, not ship_id.
WITH offenders AS MATERIALIZED (
    SELECT b.slave_id::text AS pk
    FROM game.weapon_banks AS b
    WHERE NOT EXISTS (SELECT 1 FROM game.items AS i WHERE i.item_id = b.ship_id)
),
sample AS (SELECT pk FROM offenders ORDER BY pk COLLATE "C" LIMIT 100)
SELECT check_name, primary_key, explanation FROM (
    SELECT 0 AS ord, 'weapon_bank_ship_missing' AS check_name, NULL::text AS primary_key,
           'total_count=' || count(*) AS explanation FROM offenders HAVING count(*) > 0
    UNION ALL
    SELECT 1, 'weapon_bank_ship_missing', pk,
           'weapon bank ship item does not exist' FROM sample
) AS r ORDER BY ord, primary_key COLLATE "C";

-- insurance_ship_missing: a stored policy whose ship item is gone, including
-- expired policies that remain stored.
WITH offenders AS MATERIALIZED (
    SELECT s.ship_id::text AS pk
    FROM game.insurance AS s
    WHERE NOT EXISTS (SELECT 1 FROM game.items AS i WHERE i.item_id = s.ship_id)
),
sample AS (SELECT pk FROM offenders ORDER BY pk COLLATE "C" LIMIT 100)
SELECT check_name, primary_key, explanation FROM (
    SELECT 0 AS ord, 'insurance_ship_missing' AS check_name, NULL::text AS primary_key,
           'total_count=' || count(*) AS explanation FROM offenders HAVING count(*) > 0
    UNION ALL
    SELECT 1, 'insurance_ship_missing', pk,
           'insurance ship item does not exist' FROM sample
) AS r ORDER BY ord, primary_key COLLATE "C";

-- entity_job_target_missing: a job in any state whose closed-set
-- kind + job_id shape names an entity row that no longer exists. The shapes
-- table maps each accepted kind to its anchored job_id pattern and the table
-- holding the target. The captured number is the canonical positive decimal
-- of the target key, compared as text so malformed names can never raise a
-- cast error. For alpha-injection-available the target is the first number
-- (the character), not the expected timestamp. For trade.idle_timeout the
-- name prefix trade-timeout differs from the kind. For wars.deadline only
-- the start and finish shapes are checked. Payloads are never parsed, and
-- kinds outside the closed set are never guessed: jobs whose targets may
-- legally be deleted before acknowledgement (kill rights, sovereignty
-- structures, war invites, chat restrictions) and periodic maintenance jobs
-- are intentionally not covered.
WITH shapes(kind, target, pattern) AS (VALUES
    ('skills.complete',                'characters',                 '^skills\.complete:([1-9][0-9]*)$'),
    ('skill_plans.recover',            'characters',                 '^skill_plans\.recover:([1-9][0-9]*)$'),
    ('ship_skins.license.expire',      'characters',                 '^ship_skins\.license\.expire:([1-9][0-9]*)$'),
    ('corporations.pending_kick',      'characters',                 '^corporations\.pending_kick:([1-9][0-9]*)$'),
    ('alpha-injection-available',      'characters',                 '^alpha-injection-available:([1-9][0-9]*):[1-9][0-9]*$'),
    ('subscription.clone_grade.expire','accounts',                   '^subscription\.clone_grade\.expire:([1-9][0-9]*)$'),
    ('industry.ready',                 'industry_jobs',              '^industry\.ready:([1-9][0-9]*)$'),
    ('bounty-payout',                  'killmails',                  '^bounty-payout:([1-9][0-9]*)$'),
    ('corp.vote.close',                'corp_vote_cases',            '^corp\.vote\.close:([1-9][0-9]*)$'),
    ('corp.sanction.expire',           'corp_sanctioned_actions',    '^corp\.sanction\.expire:([1-9][0-9]*)$'),
    ('repairable.payment.expiry',      'repairable_recovery_items',  '^repairable\.payment\.expiry:([1-9][0-9]*)$'),
    ('repairable.delivery.arrival',    'repairable_recovery_items',  '^repairable\.delivery\.arrival:([1-9][0-9]*)$'),
    ('repairable.withdrawal.expiry',   'repairable_recovery_items',  '^repairable\.withdrawal\.expiry:([1-9][0-9]*)$'),
    ('trade.idle_timeout',             'trades',                     '^trade-timeout:([1-9][0-9]*)$'),
    ('wars.deadline',                  'wars',                       '^wars\.deadline:(?:start|finish):([1-9][0-9]*)$')
),
targets AS MATERIALIZED (
    SELECT j.job_id AS pk, s.target, (regexp_match(j.job_id, s.pattern))[1] AS target_text
    FROM game.jobs AS j
    JOIN shapes AS s ON s.kind = j.kind AND j.job_id ~ s.pattern
),
offenders AS MATERIALIZED (
    SELECT pk FROM targets AS t
    WHERE NOT EXISTS (SELECT 1 FROM game.characters AS e WHERE t.target = 'characters' AND e.character_id::text = t.target_text)
      AND NOT EXISTS (SELECT 1 FROM game.accounts AS e WHERE t.target = 'accounts' AND e.user_id::text = t.target_text)
      AND NOT EXISTS (SELECT 1 FROM game.industry_jobs AS e WHERE t.target = 'industry_jobs' AND e.job_id::text = t.target_text)
      AND NOT EXISTS (SELECT 1 FROM game.killmails AS e WHERE t.target = 'killmails' AND e.kill_id::text = t.target_text)
      AND NOT EXISTS (SELECT 1 FROM game.corp_vote_cases AS e WHERE t.target = 'corp_vote_cases' AND e.id::text = t.target_text)
      AND NOT EXISTS (SELECT 1 FROM game.corp_sanctioned_actions AS e WHERE t.target = 'corp_sanctioned_actions' AND e.case_id::text = t.target_text)
      AND NOT EXISTS (SELECT 1 FROM game.repairable_recovery_items AS e WHERE t.target = 'repairable_recovery_items' AND e.member_id::text = t.target_text)
      AND NOT EXISTS (SELECT 1 FROM game.trades AS e WHERE t.target = 'trades' AND e.trade_id::text = t.target_text)
      AND NOT EXISTS (SELECT 1 FROM game.wars AS e WHERE t.target = 'wars' AND e.war_id::text = t.target_text)
),
sample AS (SELECT pk FROM offenders ORDER BY pk COLLATE "C" LIMIT 100)
SELECT check_name, primary_key, explanation FROM (
    SELECT 0 AS ord, 'entity_job_target_missing' AS check_name, NULL::text AS primary_key,
           'total_count=' || count(*) AS explanation FROM offenders HAVING count(*) > 0
    UNION ALL
    SELECT 1, 'entity_job_target_missing', pk,
           'job names an entity row that no longer exists' FROM sample
) AS r ORDER BY ord, primary_key COLLATE "C";

-- character_ship_invalid: a live character whose selected ship is missing or
-- owned by someone else. NULL ship_id is legal before first selection and is
-- not reported. Deleted characters are not audited.
WITH offenders AS MATERIALIZED (
    SELECT c.character_id::text AS pk
    FROM game.characters AS c
    LEFT JOIN game.items AS i ON i.item_id = c.ship_id
    WHERE NOT c.deleted AND c.ship_id IS NOT NULL
      AND (i.item_id IS NULL OR i.owner_id <> c.character_id)
),
sample AS (SELECT pk FROM offenders ORDER BY pk COLLATE "C" LIMIT 100)
SELECT check_name, primary_key, explanation FROM (
    SELECT 0 AS ord, 'character_ship_invalid' AS check_name, NULL::text AS primary_key,
           'total_count=' || count(*) AS explanation FROM offenders HAVING count(*) > 0
    UNION ALL
    SELECT 1, 'character_ship_invalid', pk,
           'selected ship is missing or owned by another entity' FROM sample
) AS r ORDER BY ord, primary_key COLLATE "C";

-- character_system_missing: a live character with NULL solar_system_id,
-- which every maintained path requires to be set. Whether the system exists
-- and constellation or region consistency are out of scope.
WITH offenders AS MATERIALIZED (
    SELECT c.character_id::text AS pk
    FROM game.characters AS c
    WHERE NOT c.deleted AND c.solar_system_id IS NULL
),
sample AS (SELECT pk FROM offenders ORDER BY pk COLLATE "C" LIMIT 100)
SELECT check_name, primary_key, explanation FROM (
    SELECT 0 AS ord, 'character_system_missing' AS check_name, NULL::text AS primary_key,
           'total_count=' || count(*) AS explanation FROM offenders HAVING count(*) > 0
    UNION ALL
    SELECT 1, 'character_system_missing', pk,
           'active character has no solar system' FROM sample
) AS r ORDER BY ord, primary_key COLLATE "C";
