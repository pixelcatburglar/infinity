-- Mutable game state (accounts, characters, items...). Idempotent; applied by
-- the server at startup. Static data lives in schema `sde` (sde_typed.sql).

CREATE SCHEMA IF NOT EXISTS game;
-- dao/items/crates: durable direct-list opening receipts.
CREATE TABLE IF NOT EXISTS game.crate_rewards (
    item_id bigint PRIMARY KEY,
    character_id bigint NOT NULL,
    crate_type_id integer NOT NULL,
    destination_location bigint NOT NULL,
    destination_flag integer NOT NULL,
    opened_at bigint NOT NULL,
    acknowledged_at bigint
);
-- dao/map_telemetry: immutable typed event receipts and cumulative arrival visits.
-- Event kinds: 1 jump, 2 ship, 3 NPC, 4 pod. Sources: 1 jump occurrence,
-- 2 killmail, 3 NPC occurrence, 4 unpiloted ship.
CREATE SEQUENCE IF NOT EXISTS game.map_occurrence_id_seq;
CREATE TABLE IF NOT EXISTS game.map_events (
    source smallint NOT NULL CHECK (source IN (1, 2, 3, 4)),
    source_id bigint NOT NULL,
    solar_system_id bigint NOT NULL,
    event_time bigint NOT NULL,
    kind smallint NOT NULL CHECK (kind IN (1, 2, 3, 4)),
    PRIMARY KEY (source, source_id),
    CHECK ((source = 1 AND kind = 1) OR (source = 2 AND kind IN (2, 4))
        OR (source = 3 AND kind = 3) OR (source = 4 AND kind = 2))
);
CREATE INDEX IF NOT EXISTS map_events_event_time_idx ON game.map_events (event_time);
CREATE TABLE IF NOT EXISTS game.map_visits (
    character_id bigint NOT NULL,
    solar_system_id bigint NOT NULL,
    last_date_time bigint NOT NULL,
    visits bigint NOT NULL CHECK (visits > 0),
    PRIMARY KEY (character_id, solar_system_id)
);


CREATE TABLE IF NOT EXISTS game.accounts (
    user_id       bigserial PRIMARY KEY,
    user_name     text NOT NULL,
    -- macho.PasswordHash(): SHA-1 chain over UTF-16 password + lower(user name).
    password_hash bytea NOT NULL,
    role          bigint NOT NULL,
    user_type     integer NOT NULL DEFAULT 20,  -- const.userTypePBC
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_login    timestamptz
);
CREATE UNIQUE INDEX IF NOT EXISTS accounts_user_name_idx ON game.accounts (lower(user_name));
-- User IDs in the live game are large; start well clear of NPC/static ID ranges.
SELECT setval('game.accounts_user_id_seq', GREATEST((SELECT last_value FROM game.accounts_user_id_seq), (SELECT max(user_id) FROM game.accounts), 10000000), true);
-- dao/accounts/subscriptions: one durable clone-grade authority per account.
CREATE TABLE IF NOT EXISTS game.account_clone_grades (
    user_id bigint PRIMARY KEY REFERENCES game.accounts(user_id) ON DELETE CASCADE,
    grade smallint NOT NULL CHECK (grade IN (0, 1)),
    omega_until bigint,
    grace_until bigint,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    CHECK (
        (grade = 0 AND omega_until IS NULL AND grace_until IS NULL)
        OR (grade = 1 AND omega_until IS NULL AND grace_until IS NULL)
        OR (grade = 1 AND omega_until IS NOT NULL AND grace_until IS NOT NULL
            AND omega_until > 0 AND grace_until > 0 AND grace_until >= omega_until)
    )
);
CREATE TABLE IF NOT EXISTS game.account_clone_grade_audit (
    audit_id bigserial PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES game.accounts(user_id) ON DELETE CASCADE,
    actor_user_id bigint REFERENCES game.accounts(user_id),
    before_grade smallint NOT NULL,
    after_grade smallint NOT NULL,
    before_omega_until bigint,
    after_omega_until bigint,
    before_grace_until bigint,
    after_grace_until bigint,
    reason text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
-- Existing accounts retain the historical unlimited-Omega policy; this never overwrites rows.
INSERT INTO game.account_clone_grades (user_id, grade)
SELECT user_id, 1 FROM game.accounts
ON CONFLICT (user_id) DO NOTHING;

-- dao/shared_settings: permanent forwardable settings and overview links.
CREATE SEQUENCE IF NOT EXISTS game.shared_setting_links_sq_id_seq
    START 9950000000000 MAXVALUE 9999999999999 NO CYCLE;
CREATE TABLE IF NOT EXISTS game.shared_setting_links (
    sq_id bigint PRIMARY KEY DEFAULT nextval('game.shared_setting_links_sq_id_seq'),
    kind text NOT NULL CHECK (kind IN ('shared', 'overview')),
    setting_type_id integer,
    hash_value text NOT NULL,
    payload text NOT NULL,
    creator_user_id bigint NOT NULL REFERENCES game.accounts(user_id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((kind = 'shared' AND setting_type_id IN (2, 3) AND setting_type_id IS NOT NULL)
        OR (kind = 'overview' AND setting_type_id IS NULL))
);

CREATE TABLE IF NOT EXISTS game.characters (
    character_id     bigserial PRIMARY KEY,
    user_id          bigint NOT NULL REFERENCES game.accounts,
    name             text NOT NULL,
    race_id          integer NOT NULL,
    bloodline_id     integer NOT NULL,
    gender_id        integer NOT NULL,
    ancestry_id      integer,
    school_id        integer,
    corporation_id   bigint NOT NULL,
    station_id       bigint,
    solar_system_id  bigint,
    constellation_id bigint,
    region_id        bigint,
    balance          numeric(20, 2) NOT NULL DEFAULT 0,
    skill_points     bigint NOT NULL DEFAULT 0,
    -- Marshalled paperdoll appearance / portrait blobs exactly as the client sent them.
    appearance       bytea,
    portrait         bytea,
    created_at       timestamptz NOT NULL DEFAULT now(),
    logoff_at        timestamptz NOT NULL DEFAULT now(),
    deleted          boolean NOT NULL DEFAULT false
);

-- dao/characters/newbie: once-per-character spare rookie kit.
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS newbie_bonus_claimed_at bigint;

-- dao/skills/alpha_injections: persistent daily Alpha injector cooldowns.
CREATE TABLE IF NOT EXISTS game.character_alpha_injections (
    character_id bigint PRIMARY KEY REFERENCES game.characters ON DELETE CASCADE,
    next_available bigint NOT NULL CHECK (next_available > 0),
    available_notified boolean NOT NULL DEFAULT false
);

-- services/slash.rs / dao/wallets: exact 36-digit character ISK storage.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
               WHERE table_schema = 'game' AND table_name = 'characters' AND column_name = 'balance'
                 AND (data_type <> 'numeric' OR numeric_precision IS DISTINCT FROM 36 OR numeric_scale IS DISTINCT FROM 2)) THEN
        ALTER TABLE game.characters ALTER COLUMN balance TYPE numeric(36, 2);
    END IF;
END $$;
CREATE UNIQUE INDEX IF NOT EXISTS characters_name_idx ON game.characters (lower(name)) WHERE NOT deleted;
CREATE INDEX IF NOT EXISTS characters_user_idx ON game.characters (user_id);

-- dao/chat_channels: durable custom room metadata and access control.
CREATE SEQUENCE IF NOT EXISTS game.chat_channel_id_seq;
CREATE TABLE IF NOT EXISTS game.chat_channels (
    name text PRIMARY KEY,
    category text NOT NULL CHECK (category IN ('player', 'private')),
    display_name text NOT NULL,
    owner_id bigint NOT NULL REFERENCES game.characters(character_id),
    public boolean NOT NULL DEFAULT false,
    motd text NOT NULL DEFAULT '',
    moderated boolean NOT NULL DEFAULT false,
    members_by_default boolean NOT NULL DEFAULT true,
    private_low bigint REFERENCES game.characters(character_id),
    private_high bigint REFERENCES game.characters(character_id),
    CHECK (owner_id > 0),
    CHECK (
        (category = 'player' AND name ~ '^player_[1-9][0-9]*$'
         AND private_low IS NULL AND private_high IS NULL)
        OR
        (category = 'private' AND private_low IS NOT NULL AND private_high IS NOT NULL
         AND private_low > 0 AND private_low < private_high
         AND owner_id IN (private_low, private_high) AND NOT public
         AND name = 'private_' || private_low::text || '_' || private_high::text)
    )
);
CREATE UNIQUE INDEX IF NOT EXISTS chat_channels_player_display_name_idx
    ON game.chat_channels (lower(display_name)) WHERE category = 'player';
CREATE TABLE IF NOT EXISTS game.chat_channel_members (
    channel_name text NOT NULL REFERENCES game.chat_channels(name) ON DELETE CASCADE,
    character_id bigint NOT NULL REFERENCES game.characters(character_id),
    affiliation text NOT NULL CHECK (affiliation IN ('none', 'member', 'admin', 'outcast')),
    invited boolean NOT NULL DEFAULT false,
    voice text NOT NULL DEFAULT 'default' CHECK (voice IN ('default', 'muted', 'voiced')),
    voice_expires_at bigint CHECK (voice_expires_at IS NULL OR voice_expires_at > 0),
    ban_expires_at bigint CHECK (ban_expires_at IS NULL OR ban_expires_at > 0),
    PRIMARY KEY (channel_name, character_id),
    CHECK (character_id > 0)
);
CREATE INDEX IF NOT EXISTS chat_channel_members_character_idx
    ON game.chat_channel_members (character_id, channel_name);
-- Special-room MOTDs are metadata only; session authority controls admission.
CREATE TABLE IF NOT EXISTS game.chat_system_motds (
    channel_name text PRIMARY KEY CHECK (channel_name ~ '^(corp|alliance)_[1-9][0-9]*$'),
    motd text NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS game.items (
    item_id     bigserial PRIMARY KEY,
    type_id     integer NOT NULL,
    owner_id    bigint NOT NULL,
    location_id bigint NOT NULL,
    flag_id     integer NOT NULL,
    quantity    bigint NOT NULL DEFAULT 1,
    singleton   boolean NOT NULL DEFAULT false,
    custom_info text,
    name        text
);
CREATE INDEX IF NOT EXISTS items_location_idx ON game.items (location_id, flag_id);
CREATE INDEX IF NOT EXISTS items_owner_idx ON game.items (owner_id);
-- dao/items/ship_configuration: sharing switches follow the current hull owner.
CREATE TABLE IF NOT EXISTS game.ship_configuration (
    ship_item_id bigint PRIMARY KEY REFERENCES game.items(item_id) ON DELETE CASCADE,
    owner_id bigint NOT NULL,
    fleet_hangar_fleet boolean NOT NULL DEFAULT false,
    fleet_hangar_corp boolean NOT NULL DEFAULT false,
    maintenance_fleet boolean NOT NULL DEFAULT false,
    maintenance_corp boolean NOT NULL DEFAULT false
);
-- dao/items/ship_dirt: dirt follows the physical hull across ownership changes.
CREATE TABLE IF NOT EXISTS game.ship_dirt (
    ship_item_id bigint PRIMARY KEY REFERENCES game.items(item_id) ON DELETE CASCADE,
    dirt_timestamp bigint NOT NULL
);


-- Ship SKIN licenses and applications; expiry timestamps use FILETIME.
CREATE TABLE IF NOT EXISTS game.ship_skin_licenses (
    character_id bigint NOT NULL REFERENCES game.characters(character_id) ON DELETE CASCADE,
    skin_id integer NOT NULL,
    expires_at bigint,
    PRIMARY KEY (character_id, skin_id)
);
CREATE INDEX IF NOT EXISTS ship_skin_licenses_expiry_idx
    ON game.ship_skin_licenses (expires_at) WHERE expires_at IS NOT NULL;
CREATE TABLE IF NOT EXISTS game.ship_skin_applications (
    ship_item_id bigint PRIMARY KEY REFERENCES game.items(item_id) ON DELETE CASCADE,
    character_id bigint NOT NULL,
    skin_id integer NOT NULL,
    FOREIGN KEY (character_id, skin_id) REFERENCES game.ship_skin_licenses(character_id, skin_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS ship_skin_applications_license_idx
    ON game.ship_skin_applications (character_id, skin_id);
-- dao/items/brain: durable augmentation metadata on installed item rows.
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS brain_slot integer;
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS brain_activated_at bigint;
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS brain_expires_at bigint;
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS brain_effect_ids integer[];
-- Replace the legacy index only when installing custody, not on later startups.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'game' AND table_name = 'items' AND column_name = 'jump_clone_id') THEN
        ALTER TABLE game.items ADD COLUMN jump_clone_id bigint;
        DROP INDEX IF EXISTS game.items_brain_slot_idx;
    END IF;
END $$;
CREATE UNIQUE INDEX IF NOT EXISTS items_brain_slot_idx ON game.items (owner_id, flag_id, brain_slot)
    WHERE location_id = owner_id AND brain_slot IS NOT NULL AND jump_clone_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS items_clone_brain_slot_idx ON game.items (jump_clone_id, brain_slot)
    WHERE jump_clone_id IS NOT NULL;
-- dao/items/dogma: per-item dogma base overrides.
CREATE TABLE IF NOT EXISTS game.item_attributes (
    item_id bigint NOT NULL REFERENCES game.items(item_id) ON DELETE CASCADE,
    attribute_id integer NOT NULL,
    value double precision NOT NULL,
    PRIMARY KEY (item_id, attribute_id)
);
-- dao/items/dogma: FILETIME of the persisted shield charge.
ALTER TABLE game.item_attributes ADD COLUMN IF NOT EXISTS updated_at bigint NOT NULL DEFAULT 0;

-- dao/dynamic_items: mutation provenance; consumed inputs intentionally have no foreign keys.
CREATE TABLE IF NOT EXISTS game.dynamic_items (
    item_id bigint PRIMARY KEY REFERENCES game.items(item_id) ON DELETE CASCADE,
    source_item_id bigint NOT NULL,
    mutator_item_id bigint NOT NULL,
    source_type_id integer NOT NULL,
    mutator_type_id integer NOT NULL,
    result_type_id integer NOT NULL,
    creator_id bigint NOT NULL,
    created_at bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS game.dynamic_item_modifiers (
    item_id bigint NOT NULL REFERENCES game.dynamic_items(item_id) ON DELETE CASCADE,
    attribute_id integer NOT NULL,
    factor double precision NOT NULL,
    PRIMARY KEY (item_id, attribute_id)
);
-- idCheckers.IsPlayerItem: >= 100,000,000; start far above character IDs.
SELECT setval('game.items_item_id_seq', GREATEST((SELECT last_value FROM game.items_item_id_seq), (SELECT max(item_id) FROM game.items), 1000000000000), true);

ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS ship_id bigint;
-- Position of items in space (location_id = solar system).
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS x double precision;
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS y double precision;
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS z double precision;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS free_skill_points bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS home_station_id bigint;
UPDATE game.characters SET home_station_id = station_id WHERE home_station_id IS NULL AND station_id IS NOT NULL;
-- A character's private notes on anything with an info window (charMgr GetNote/SetNote).
CREATE TABLE IF NOT EXISTS game.char_notes (
    char_id bigint NOT NULL,
    item_id bigint NOT NULL,
    note    text NOT NULL,
    PRIMARY KEY (char_id, item_id)
);
-- dao/characters/contacts: independent personal contacts and blocked owners.
CREATE TABLE IF NOT EXISTS game.char_contacts (
    char_id bigint NOT NULL,
    contact_id bigint NOT NULL,
    relationship real NOT NULL,
    in_watchlist boolean NOT NULL,
    label_mask bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (char_id, contact_id)
);
CREATE INDEX IF NOT EXISTS char_contacts_contact_idx ON game.char_contacts (contact_id, char_id);
CREATE TABLE IF NOT EXISTS game.char_blocked (
    char_id bigint NOT NULL,
    owner_id bigint NOT NULL,
    PRIMARY KEY (char_id, owner_id)
);
-- The activities window's auto-popup setting (activityMgr pop state; 0 opens it at login).
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS activities_pop_state integer NOT NULL DEFAULT 1;

CREATE TABLE IF NOT EXISTS game.skills (
    character_id bigint NOT NULL REFERENCES game.characters,
    type_id      integer NOT NULL,
    level        integer NOT NULL,
    skill_points bigint NOT NULL,
    PRIMARY KEY (character_id, type_id)
);
-- Expert-system installation lifecycles (dao/skills/expert_systems).
CREATE TABLE IF NOT EXISTS game.character_expert_systems (
    character_id bigint NOT NULL REFERENCES game.characters ON DELETE CASCADE,
    type_id integer NOT NULL,
    installed_at bigint NOT NULL,
    expires_at bigint NOT NULL,
    PRIMARY KEY (character_id, type_id),
    CHECK (character_id > 0 AND type_id > 0 AND installed_at > 0 AND expires_at > installed_at)
);
CREATE INDEX IF NOT EXISTS character_expert_systems_expiry_idx
    ON game.character_expert_systems (expires_at, character_id);


-- Skill history and authoritative character remaps (dao/skills/history, dao/characters/attributes).
CREATE TABLE IF NOT EXISTS game.skill_history (
    history_id bigserial PRIMARY KEY,
    character_id bigint NOT NULL REFERENCES game.characters ON DELETE CASCADE,
    skill_type_id integer NOT NULL,
    event_type_id integer NOT NULL,
    absolute_points bigint NOT NULL,
    log_date bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS skill_history_character_date_idx ON game.skill_history (character_id, log_date DESC, history_id DESC);
CREATE TABLE IF NOT EXISTS game.character_respec (
    character_id bigint PRIMARY KEY REFERENCES game.characters ON DELETE CASCADE,
    bases integer[],
    free_respecs bigint NOT NULL,
    last_respec_date bigint,
    next_timed_respec bigint
);

-- Active training queue; times are FILETIME (100ns since 1601) like the client's.
CREATE TABLE IF NOT EXISTS game.skill_queue (
    character_id bigint NOT NULL REFERENCES game.characters,
    position     integer NOT NULL,
    type_id      integer NOT NULL,
    to_level     integer NOT NULL,
    start_sp     bigint NOT NULL,
    dest_sp      bigint NOT NULL,
    start_time   bigint,
    end_time     bigint,
    PRIMARY KEY (character_id, position)
);

-- Player character IDs live in the 90,000,000+ range (idCheckers.IsEvePlayerCharacter).
SELECT setval('game.characters_character_id_seq', GREATEST((SELECT last_value FROM game.characters_character_id_seq), (SELECT max(character_id) FROM game.characters), 90000000), true);

-- Regional market. char_id NULL marks the NPC orders seeded at the starter stations
-- (sql/market_seed.sql, and buy orders from them in db.rs); issued NULL means "re-issued today".
CREATE TABLE IF NOT EXISTS game.market_orders (
    order_id         bigserial PRIMARY KEY,
    type_id          integer NOT NULL,
    char_id          bigint,
    station_id       bigint NOT NULL,
    solar_system_id  bigint NOT NULL,
    constellation_id bigint NOT NULL,
    region_id        bigint NOT NULL,
    price            double precision NOT NULL,
    vol_entered      bigint NOT NULL,
    vol_remaining    bigint NOT NULL,
    min_volume       bigint NOT NULL DEFAULT 1,
    bid              boolean NOT NULL,
    range            integer NOT NULL DEFAULT -1,  -- marketutil.const.rangeStation
    duration         integer NOT NULL,             -- days
    issued           bigint                        -- FILETIME
);
CREATE INDEX IF NOT EXISTS market_orders_type_region_idx ON game.market_orders (type_id, region_id);
CREATE INDEX IF NOT EXISTS market_orders_station_idx ON game.market_orders (station_id, type_id);
CREATE INDEX IF NOT EXISTS market_orders_system_idx ON game.market_orders (solar_system_id, type_id);
CREATE INDEX IF NOT EXISTS market_orders_region_idx ON game.market_orders (region_id, type_id);
-- ISK a player's buy order holds for its remaining volume.
ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS escrow double precision NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS market_orders_char_idx ON game.market_orders (char_id) WHERE char_id IS NOT NULL;

-- Players' orders once filled, expired (state 2) or cancelled (3): marketHistory's orderState.
CREATE TABLE IF NOT EXISTS game.market_order_history (
    order_id      bigint PRIMARY KEY,
    type_id       integer NOT NULL,
    char_id       bigint NOT NULL,
    station_id    bigint NOT NULL,
    region_id     bigint NOT NULL,
    price         double precision NOT NULL,
    vol_entered   bigint NOT NULL,
    vol_remaining bigint NOT NULL,
    min_volume    bigint NOT NULL,
    bid           boolean NOT NULL,
    range         integer NOT NULL,
    duration      integer NOT NULL,
    issued        bigint NOT NULL,  -- FILETIME
    state         integer NOT NULL,
    closed        bigint NOT NULL   -- FILETIME
);
CREATE INDEX IF NOT EXISTS market_order_history_char_idx ON game.market_order_history (char_id, closed);

-- Corporate market ownership and original sell custody (dao/market).
ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS corporation_id bigint;
ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS wallet_key integer NOT NULL DEFAULT 1000;
ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS source_location_id bigint;
ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS source_flag_id integer;
ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS corporation_id bigint;
ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS wallet_key integer NOT NULL DEFAULT 1000;
ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS source_location_id bigint;
ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS source_flag_id integer;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.market_orders'::regclass AND conname = 'market_orders_ownership_check') THEN
        ALTER TABLE game.market_orders ADD CONSTRAINT market_orders_ownership_check CHECK (
            (corporation_id IS NULL AND wallet_key = 1000 AND source_location_id IS NULL AND source_flag_id IS NULL)
            OR (corporation_id IS NOT NULL AND char_id IS NOT NULL AND wallet_key BETWEEN 1000 AND 1006
                AND ((bid AND source_location_id IS NULL AND source_flag_id IS NULL)
                  OR (NOT bid AND source_location_id IS NOT NULL AND source_flag_id IS NOT NULL))));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.market_order_history'::regclass AND conname = 'market_order_history_ownership_check') THEN
        ALTER TABLE game.market_order_history ADD CONSTRAINT market_order_history_ownership_check CHECK (
            (corporation_id IS NULL AND wallet_key = 1000 AND source_location_id IS NULL AND source_flag_id IS NULL)
            OR (corporation_id IS NOT NULL AND char_id IS NOT NULL AND wallet_key BETWEEN 1000 AND 1006
                AND ((bid AND source_location_id IS NULL AND source_flag_id IS NULL)
                  OR (NOT bid AND source_location_id IS NOT NULL AND source_flag_id IS NOT NULL))));
    END IF;
END $$;
CREATE INDEX IF NOT EXISTS market_orders_corporation_idx ON game.market_orders (corporation_id, order_id) WHERE corporation_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS market_orders_corporation_wallet_idx ON game.market_orders (corporation_id, wallet_key, order_id) WHERE corporation_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS market_order_history_corporation_idx ON game.market_order_history (corporation_id, closed, order_id) WHERE corporation_id IS NOT NULL;

-- Every fill. An NPC side is the station's owner. Read back as each side's wallet
-- transactions and as the regional price history.
CREATE TABLE IF NOT EXISTS game.market_transactions (
    transaction_id bigserial PRIMARY KEY,
    type_id        integer NOT NULL,
    quantity       bigint NOT NULL,
    price          double precision NOT NULL,
    buyer_id       bigint NOT NULL,
    seller_id      bigint NOT NULL,
    station_id     bigint NOT NULL,
    region_id      bigint NOT NULL,
    date           bigint NOT NULL  -- FILETIME
);
CREATE INDEX IF NOT EXISTS market_transactions_buyer_idx ON game.market_transactions (buyer_id, date);
CREATE INDEX IF NOT EXISTS market_transactions_seller_idx ON game.market_transactions (seller_id, date);
CREATE INDEX IF NOT EXISTS market_transactions_history_idx ON game.market_transactions (region_id, type_id, date);

-- Identity is separate from the canonical character/corporation balance.
CREATE TABLE IF NOT EXISTS game.wallet_accounts (
    account_id bigserial PRIMARY KEY,
    owner_id bigint NOT NULL,
    account_key integer NOT NULL,
    UNIQUE (owner_id, account_key)
);
ALTER TABLE game.market_transactions ADD COLUMN IF NOT EXISTS buyer_account_id bigint REFERENCES game.wallet_accounts(account_id);
ALTER TABLE game.market_transactions ADD COLUMN IF NOT EXISTS seller_account_id bigint REFERENCES game.wallet_accounts(account_id);
ALTER TABLE game.market_transactions ADD COLUMN IF NOT EXISTS order_id bigint;
-- Legacy personal fills are identified only by actual character records, not inferred corporation usage.
-- 1000 is appConst.accountingKeyCash (consts::ACCOUNT_CASH).
INSERT INTO game.wallet_accounts (owner_id, account_key)
SELECT c.character_id, 1000 FROM game.characters c
WHERE EXISTS (SELECT 1 FROM game.market_transactions t WHERE t.buyer_id = c.character_id OR t.seller_id = c.character_id)
ON CONFLICT (owner_id, account_key) DO NOTHING;
UPDATE game.market_transactions t SET buyer_account_id = a.account_id
FROM game.wallet_accounts a JOIN game.characters c ON c.character_id = a.owner_id
WHERE t.buyer_account_id IS NULL AND t.buyer_id = c.character_id AND a.account_key = 1000;
UPDATE game.market_transactions t SET seller_account_id = a.account_id
FROM game.wallet_accounts a JOIN game.characters c ON c.character_id = a.owner_id
WHERE t.seller_account_id IS NULL AND t.seller_id = c.character_id AND a.account_key = 1000;
CREATE INDEX IF NOT EXISTS market_transactions_buyer_account_idx ON game.market_transactions (buyer_account_id, date, transaction_id);
CREATE INDEX IF NOT EXISTS market_transactions_seller_account_idx ON game.market_transactions (seller_account_id, date, transaction_id);

CREATE TABLE IF NOT EXISTS game.wallet_journal (
    journal_id bigserial PRIMARY KEY,
    account_id bigint NOT NULL REFERENCES game.wallet_accounts(account_id),
    actor_id bigint NOT NULL,
    entry_type_id integer NOT NULL,
    owner_id1 bigint NOT NULL,
    owner_id2 bigint NOT NULL,
    reference_id bigint NOT NULL,
    amount numeric(36,2) NOT NULL,
    -- A gross sale row is the before-tax balance; it can exceed the wallet cap by at most one tax amount.
    balance_after numeric(37,2) NOT NULL,
    created_at bigint NOT NULL,
    description text NOT NULL DEFAULT '',
    market_transaction_id bigint REFERENCES game.market_transactions(transaction_id),
    order_id bigint,
    side smallint NOT NULL
);
CREATE INDEX IF NOT EXISTS wallet_journal_account_date_idx ON game.wallet_journal (account_id, created_at DESC, journal_id DESC);
CREATE UNIQUE INDEX IF NOT EXISTS wallet_journal_fill_unique ON game.wallet_journal (market_transaction_id, account_id, entry_type_id, side) WHERE market_transaction_id IS NOT NULL;

-- Contracts (services/contracts.rs). type/status are appConst.conType*/conStatus*; an
-- auction's buyout is its collateral. Times are FILETIMEs (0: not yet).
CREATE TABLE IF NOT EXISTS game.contracts (
    contract_id      bigserial PRIMARY KEY,
    type             integer NOT NULL,
    issuer_id        bigint NOT NULL,
    issuer_corp_id   bigint NOT NULL,
    assignee_id      bigint NOT NULL DEFAULT 0,
    acceptor_id      bigint NOT NULL DEFAULT 0,
    start_station_id bigint NOT NULL,
    end_station_id   bigint NOT NULL DEFAULT 0,
    price            double precision NOT NULL DEFAULT 0,
    reward           double precision NOT NULL DEFAULT 0,
    collateral       double precision NOT NULL DEFAULT 0,
    title            text NOT NULL DEFAULT '',
    description      text NOT NULL DEFAULT '',
    volume           double precision NOT NULL DEFAULT 0,
    num_days         integer NOT NULL DEFAULT 0,
    status           integer NOT NULL DEFAULT 0,
    date_issued      bigint NOT NULL,
    date_expired     bigint NOT NULL,
    date_accepted    bigint NOT NULL DEFAULT 0,
    date_completed   bigint NOT NULL DEFAULT 0,
    -- Courier: the plastic wrap the acceptor carries.
    package_id       bigint
);
CREATE INDEX IF NOT EXISTS contracts_status_idx ON game.contracts (status, type);
CREATE INDEX IF NOT EXISTS contracts_issuer_idx ON game.contracts (issuer_id);
CREATE INDEX IF NOT EXISTS contracts_acceptor_idx ON game.contracts (acceptor_id);
CREATE INDEX IF NOT EXISTS contracts_assignee_idx ON game.contracts (assignee_id);

-- What a contract holds (in_crate: escrowed items, located at -contract_id) and what an
-- item exchange asks for in return (item_id NULL).
CREATE TABLE IF NOT EXISTS game.contract_items (
    contract_id bigint NOT NULL REFERENCES game.contracts,
    item_id     bigint,
    type_id     integer NOT NULL,
    quantity    bigint NOT NULL,
    in_crate    boolean NOT NULL
);
CREATE INDEX IF NOT EXISTS contract_items_contract_idx ON game.contract_items (contract_id);

-- Auction bids; the highest holds its ISK in escrow until outbid or settled.
CREATE TABLE IF NOT EXISTS game.contract_bids (
    bid_id      bigserial PRIMARY KEY,
    contract_id bigint NOT NULL REFERENCES game.contracts,
    bidder_id   bigint NOT NULL,
    amount      double precision NOT NULL,
    date        bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS contract_bids_contract_idx ON game.contract_bids (contract_id, amount DESC);

-- services/contracts: inventorycommon/const.py typePlasticWrap (3468);
-- inventorycommon/util.py:45-48 decodes negative quantity as hundredths of m³.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM game.items i JOIN game.contracts c ON c.package_id = i.item_id
        WHERE i.type_id = 3468 AND i.singleton AND i.quantity >= 0
          AND CASE WHEN c.volume = 'NaN'::double precision OR c.volume < 0
                         OR c.volume > 21474837.0 THEN true
                   ELSE ceil(c.volume * 100.0) > 2147483647 END
    ) THEN
        RAISE EXCEPTION 'invalid or out-of-range legacy plastic wrap contract volume';
    END IF;
    UPDATE game.items i
    SET quantity = -greatest(1, ceil(c.volume * 100.0)::integer)
    FROM game.contracts c
    WHERE c.package_id = i.item_id AND i.type_id = 3468 AND i.singleton AND i.quantity >= 0;

    UPDATE game.items i SET quantity = -1
    WHERE i.type_id = 3468 AND i.singleton AND i.quantity >= 0
      AND NOT EXISTS (SELECT 1 FROM game.contracts c WHERE c.package_id = i.item_id);
END $$;

-- Blueprint research state; a blueprint without a row is ME 0 / TE 0. runs is -1 for an
-- original. Copies are singleton items of quantity -2.
CREATE TABLE IF NOT EXISTS game.blueprints (
    item_id             bigint PRIMARY KEY,
    material_efficiency integer NOT NULL DEFAULT 0,
    time_efficiency     integer NOT NULL DEFAULT 0,
    runs                integer NOT NULL DEFAULT -1
);

-- Industry jobs (services/industry.rs); status is industry.STATUS_*.
CREATE TABLE IF NOT EXISTS game.industry_jobs (
    job_id            bigserial PRIMARY KEY,
    installer_id      bigint NOT NULL,
    owner_id          bigint NOT NULL,
    activity_id       integer NOT NULL,
    blueprint_id      bigint NOT NULL,
    blueprint_type_id integer NOT NULL,
    blueprint_copy    boolean NOT NULL,
    facility_id       bigint NOT NULL,
    solar_system_id   bigint NOT NULL,
    runs              integer NOT NULL,
    licensed_runs     integer NOT NULL DEFAULT 1,
    successful_runs   integer,
    product_type_id   integer,
    optional_type_id  integer,
    cost              double precision NOT NULL,
    probability       double precision NOT NULL DEFAULT 1,
    time_seconds      bigint NOT NULL,
    start_date        bigint NOT NULL,
    end_date          bigint NOT NULL,
    completed_date    bigint,
    completed_by      bigint,
    status            integer NOT NULL
);
CREATE INDEX IF NOT EXISTS industry_jobs_owner_idx ON game.industry_jobs (owner_id, status);
CREATE INDEX IF NOT EXISTS industry_jobs_blueprint_idx ON game.industry_jobs (blueprint_id) WHERE status = 1;
-- Industry custody (dao/industry.rs). Legacy jobs were personal; flagHangar=4 and accountingKeyCash=1000.
ALTER TABLE game.industry_jobs ADD COLUMN IF NOT EXISTS input_location_id bigint;
ALTER TABLE game.industry_jobs ADD COLUMN IF NOT EXISTS input_flag_id integer;
ALTER TABLE game.industry_jobs ADD COLUMN IF NOT EXISTS output_location_id bigint;
ALTER TABLE game.industry_jobs ADD COLUMN IF NOT EXISTS output_flag_id integer;
ALTER TABLE game.industry_jobs ADD COLUMN IF NOT EXISTS wallet_owner_id bigint;
ALTER TABLE game.industry_jobs ADD COLUMN IF NOT EXISTS wallet_account_key integer;
ALTER TABLE game.industry_jobs ADD COLUMN IF NOT EXISTS blueprint_location_id bigint;
ALTER TABLE game.industry_jobs ADD COLUMN IF NOT EXISTS blueprint_flag_id integer;
UPDATE game.industry_jobs SET
    input_location_id = COALESCE(input_location_id, facility_id), input_flag_id = COALESCE(input_flag_id, 4),
    output_location_id = COALESCE(output_location_id, facility_id), output_flag_id = COALESCE(output_flag_id, 4),
    wallet_owner_id = COALESCE(wallet_owner_id, owner_id), wallet_account_key = COALESCE(wallet_account_key, 1000),
    blueprint_location_id = COALESCE(blueprint_location_id, facility_id), blueprint_flag_id = COALESCE(blueprint_flag_id, 4)
WHERE owner_id = installer_id AND (input_location_id IS NULL OR input_flag_id IS NULL OR output_location_id IS NULL
    OR output_flag_id IS NULL OR wallet_owner_id IS NULL OR wallet_account_key IS NULL
    OR blueprint_location_id IS NULL OR blueprint_flag_id IS NULL);

-- Crimewatch (services/crimewatch.rs): -10 .. +10.
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS security_status double precision NOT NULL DEFAULT 0;

-- Player ships lost; blob is the killBlob the kill report parses (attackers, items).
CREATE TABLE IF NOT EXISTS game.killmails (
    kill_id           bigserial PRIMARY KEY,
    kill_time         bigint NOT NULL,
    solar_system_id   bigint NOT NULL,
    victim_id         bigint NOT NULL,
    victim_corp_id    bigint NOT NULL,
    victim_ship_type  integer NOT NULL,
    damage_taken      double precision NOT NULL,
    final_char_id     bigint,
    final_corp_id     bigint,
    final_ship_type   integer NOT NULL,
    final_weapon_type integer NOT NULL,
    final_damage      double precision NOT NULL,
    final_sec_status  double precision NOT NULL,
    isk_lost          double precision NOT NULL,
    attacker_ids      bigint[] NOT NULL,
    blob              text NOT NULL
);
CREATE INDEX IF NOT EXISTS killmails_victim_idx ON game.killmails (victim_id, kill_id);
CREATE INDEX IF NOT EXISTS killmails_attackers_idx ON game.killmails USING gin (attacker_ids);

-- EVE mail (services/mail.rs): each message once, a status row per sender and recipient
-- (appConst.mailStatusMask* bits, mailLabel* bits plus custom labels from bit 16).
CREATE TABLE IF NOT EXISTS game.mail_messages (
    message_id             bigserial PRIMARY KEY,
    sender_id              bigint NOT NULL,
    to_character_ids       text NOT NULL DEFAULT '',
    to_list_id             bigint,
    to_corp_or_alliance_id bigint,
    title                  text NOT NULL,
    body                   text NOT NULL,
    sent_date              bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS game.mail_status (
    message_id  bigint NOT NULL REFERENCES game.mail_messages,
    char_id     bigint NOT NULL,
    status_mask integer NOT NULL DEFAULT 0,
    label_mask  integer NOT NULL DEFAULT 0,
    PRIMARY KEY (char_id, message_id)
);
CREATE TABLE IF NOT EXISTS game.mail_labels (
    char_id  bigint NOT NULL,
    label_id integer NOT NULL,
    name     text NOT NULL,
    color    integer,
    PRIMARY KEY (char_id, label_id)
);

-- Mailing-list metadata, membership and entity access (dao/mailing_lists.rs).
CREATE SEQUENCE IF NOT EXISTS game.mailing_lists_list_id_seq
    START WITH 9900000000000 MAXVALUE 9949999999999 NO CYCLE;
CREATE TABLE IF NOT EXISTS game.mailing_lists (
    list_id bigint PRIMARY KEY DEFAULT nextval('game.mailing_lists_list_id_seq'),
    name_key text NOT NULL,
    display_name text NOT NULL,
    owner_id bigint NOT NULL REFERENCES game.characters(character_id),
    default_access smallint NOT NULL CHECK (default_access IN (0, 1)),
    default_member_access smallint NOT NULL CHECK (default_member_access IN (0, 1)),
    welcome_title text,
    welcome_body text,
    created_at bigint NOT NULL,
    deleted_at bigint,
    CHECK ((welcome_title IS NULL) = (welcome_body IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS mailing_lists_active_name_idx
    ON game.mailing_lists (name_key) WHERE deleted_at IS NULL;
CREATE TABLE IF NOT EXISTS game.mailing_list_members (
    list_id bigint NOT NULL REFERENCES game.mailing_lists(list_id),
    character_id bigint NOT NULL REFERENCES game.characters(character_id),
    member_access smallint NOT NULL CHECK (member_access IN (0, 1, 2)),
    joined_at bigint NOT NULL,
    PRIMARY KEY (list_id, character_id)
);
CREATE INDEX IF NOT EXISTS mailing_list_members_character_idx
    ON game.mailing_list_members (character_id, list_id);
-- Entity kinds: 1 character, 2 corporation, 3 alliance.
CREATE TABLE IF NOT EXISTS game.mailing_list_access (
    list_id bigint NOT NULL REFERENCES game.mailing_lists(list_id),
    entity_id bigint NOT NULL,
    entity_kind smallint NOT NULL CHECK (entity_kind IN (1, 2, 3)),
    access smallint NOT NULL CHECK (access IN (0, 1)),
    PRIMARY KEY (list_id, entity_id)
);

-- Notifications; data is the YAML mapping the client's formatter for type_id reads.
CREATE TABLE IF NOT EXISTS game.notifications (
    notification_id bigserial PRIMARY KEY,
    receiver_id     bigint NOT NULL,
    type_id         integer NOT NULL,
    group_id        integer NOT NULL,
    sender_id       bigint NOT NULL,
    created         bigint NOT NULL,
    processed       boolean NOT NULL DEFAULT false,
    data            text NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS notifications_receiver_idx ON game.notifications (receiver_id, notification_id);

-- NPC (agent, corporation, faction) standings toward characters, -10 .. +10.
CREATE TABLE IF NOT EXISTS game.standings (
    char_id  bigint NOT NULL,
    from_id  bigint NOT NULL,
    standing double precision NOT NULL,
    PRIMARY KEY (char_id, from_id)
);

-- Player corporations (services/corps.rs); IDs from 98,000,000 (idCheckers.IsPlayerCorporation).
CREATE TABLE IF NOT EXISTS game.corporations (
    corporation_id bigserial PRIMARY KEY,
    name           text NOT NULL,
    ticker         text NOT NULL,
    description    text NOT NULL DEFAULT '',
    url            text NOT NULL DEFAULT '',
    tax_rate       double precision NOT NULL DEFAULT 0,
    lp_tax_rate    double precision NOT NULL DEFAULT 0,
    ceo_id         bigint,
    creator_id     bigint NOT NULL,
    station_id     bigint NOT NULL,
    is_recruiting  boolean NOT NULL DEFAULT true,
    friendly_fire  boolean NOT NULL DEFAULT false,
    shape1 integer, shape2 integer, shape3 integer,
    color1 integer, color2 integer, color3 integer,
    typeface       integer,
    welcome_mail   text NOT NULL DEFAULT '',
    created        bigint NOT NULL,
    deleted        boolean NOT NULL DEFAULT false
);
CREATE UNIQUE INDEX IF NOT EXISTS corporations_name_idx ON game.corporations (lower(name)) WHERE NOT deleted;
CREATE UNIQUE INDEX IF NOT EXISTS corporations_ticker_idx ON game.corporations (upper(ticker)) WHERE NOT deleted;
SELECT setval('game.corporations_corporation_id_seq', GREATEST((SELECT last_value FROM game.corporations_corporation_id_seq), (SELECT max(corporation_id) FROM game.corporations), 98000000), true);
-- NULL marks a legacy corporation whose CEO's abilities have not yet been captured.
ALTER TABLE game.corporations ADD COLUMN IF NOT EXISTS member_limit bigint CHECK (member_limit >= 0);

-- dao/corporations: issued shares, immutable receipts and general vote snapshots.
ALTER TABLE game.corporations ADD COLUMN IF NOT EXISTS issued_shares bigint CHECK (issued_shares > 0);
CREATE SEQUENCE IF NOT EXISTS game.corp_governance_operation_seq;
CREATE TABLE IF NOT EXISTS game.corp_shares (
    corporation_id bigint NOT NULL REFERENCES game.corporations,
    shareholder_id bigint NOT NULL,
    shares bigint NOT NULL CHECK (shares >= 0),
    PRIMARY KEY (corporation_id, shareholder_id)
);
CREATE TABLE IF NOT EXISTS game.corp_share_transfers (
    operation_id bigint PRIMARY KEY,
    corporation_id bigint NOT NULL REFERENCES game.corporations,
    from_id bigint NOT NULL,
    to_id bigint NOT NULL,
    quantity bigint NOT NULL CHECK (quantity > 0),
    actor_id bigint NOT NULL,
    created bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS game.corp_dividends (
    operation_id bigint PRIMARY KEY,
    corporation_id bigint NOT NULL REFERENCES game.corporations,
    division integer NOT NULL,
    requested double precision NOT NULL,
    distributed numeric(36,2) NOT NULL DEFAULT 0,
    actor_id bigint NOT NULL,
    created bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS game.corp_dividend_recipients (
    operation_id bigint NOT NULL REFERENCES game.corp_dividends,
    holder_id bigint NOT NULL,
    shares bigint NOT NULL CHECK (shares > 0),
    amount numeric(36,2) NOT NULL CHECK (amount >= 0),
    PRIMARY KEY (operation_id, holder_id)
);
CREATE TABLE IF NOT EXISTS game.corp_vote_cases (
    id bigserial PRIMARY KEY,
    corporation_id bigint NOT NULL REFERENCES game.corporations,
    creator_id bigint NOT NULL,
    title text NOT NULL,
    description text NOT NULL,
    starts bigint NOT NULL,
    ends bigint NOT NULL,
    closed_at bigint
);
CREATE TABLE IF NOT EXISTS game.corp_vote_options (
    case_id bigint NOT NULL REFERENCES game.corp_vote_cases,
    option_id integer NOT NULL,
    text text NOT NULL,
    PRIMARY KEY (case_id, option_id)
);
CREATE TABLE IF NOT EXISTS game.corp_vote_snapshot (
    case_id bigint NOT NULL REFERENCES game.corp_vote_cases,
    holder_id bigint NOT NULL,
    shares bigint NOT NULL CHECK (shares > 0),
    PRIMARY KEY (case_id, holder_id)
);
CREATE TABLE IF NOT EXISTS game.corp_vote_ballots (
    case_id bigint NOT NULL,
    character_id bigint NOT NULL,
    option_id integer NOT NULL,
    created bigint NOT NULL,
    PRIMARY KEY (case_id, character_id),
    FOREIGN KEY (case_id, option_id) REFERENCES game.corp_vote_options
);
CREATE TABLE IF NOT EXISTS game.corp_vote_claims (
    case_id bigint NOT NULL,
    holder_id bigint NOT NULL,
    character_id bigint NOT NULL,
    PRIMARY KEY (case_id, holder_id),
    FOREIGN KEY (case_id, holder_id) REFERENCES game.corp_vote_snapshot,
    FOREIGN KEY (case_id, character_id) REFERENCES game.corp_vote_ballots
);
-- A member's corporation roles (appConst.corpRole* mask), the same at every location, the
-- roles they may grant, their titles (game.corp_titles bits) and free-text title.
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_roles bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_grantable_roles bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_title_mask bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_title text NOT NULL DEFAULT '';
-- Hangar division 1-7 then wallet division 1-7 names (NULL: the client's defaults).
ALTER TABLE game.corporations ADD COLUMN IF NOT EXISTS division_names text[];

-- A corporation's titles: title_id is the title's bit in a member's corp_title_mask, and its
-- roles add to the holder's.
CREATE TABLE IF NOT EXISTS game.corp_titles (
    corporation_id   bigint NOT NULL,
    title_id         bigint NOT NULL,
    name             text NOT NULL DEFAULT '',
    roles            bigint NOT NULL DEFAULT 0,
    grantable_roles  bigint NOT NULL DEFAULT 0,
    roles_hq         bigint NOT NULL DEFAULT 0,
    grantable_hq     bigint NOT NULL DEFAULT 0,
    roles_base       bigint NOT NULL DEFAULT 0,
    grantable_base   bigint NOT NULL DEFAULT 0,
    roles_other      bigint NOT NULL DEFAULT 0,
    grantable_other  bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (corporation_id, title_id)
);

-- Scoped corporation roles: the legacy folded local bits retain their historical
-- all-location meaning once; subsequent startups must never widen new grants.
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_roles_hq bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_roles_base bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_roles_other bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_grantable_hq bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_grantable_base bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_grantable_other bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_base_id bigint;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_block_roles boolean NOT NULL DEFAULT false;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_roles_removed_at bigint;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_division_id bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_squadron_id bigint NOT NULL DEFAULT 0;
-- Real observations only; old selection timestamps cannot distinguish logon from logoff.
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS observed_logon_at timestamptz;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS observed_logoff_at timestamptz;
CREATE TABLE IF NOT EXISTS game.corp_role_scope_migration (id boolean PRIMARY KEY DEFAULT true CHECK (id));

-- Durable, post-deployment corporation audit events; no historical backfill.
CREATE TABLE IF NOT EXISTS game.corp_member_events (
    event_id bigserial PRIMARY KEY, character_id bigint NOT NULL, issuer_id bigint NOT NULL,
    corporation_id bigint NOT NULL, event_type_id integer NOT NULL, event_time bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS corp_member_events_lookup_idx ON game.corp_member_events
    (corporation_id, character_id, event_time DESC, event_id DESC);
CREATE TABLE IF NOT EXISTS game.corp_role_history (
    history_id bigserial PRIMARY KEY, character_id bigint NOT NULL,
    corporation_id bigint NOT NULL, issuer_id bigint NOT NULL, change_time bigint NOT NULL,
    old_roles bigint NOT NULL, new_roles bigint NOT NULL, grantable boolean NOT NULL,
    scope smallint NOT NULL
);
CREATE INDEX IF NOT EXISTS corp_role_history_lookup_idx ON game.corp_role_history
    (corporation_id, character_id, change_time DESC, history_id DESC);
-- Durable director kick intent; eligibility is rechecked on every trigger.
CREATE TABLE IF NOT EXISTS game.corp_pending_kicks (
    corporation_id bigint NOT NULL,
    character_id bigint NOT NULL,
    issuer_id bigint NOT NULL,
    requested_at bigint NOT NULL,
    eligible_at bigint,
    reason smallint NOT NULL,
    PRIMARY KEY (corporation_id, character_id)
);
CREATE INDEX IF NOT EXISTS corp_pending_kicks_due_idx
    ON game.corp_pending_kicks (eligible_at) WHERE eligible_at IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS corp_pending_kicks_character_idx
    ON game.corp_pending_kicks (character_id);

-- Applications and invitations (status: appConst.crpApplication*).
CREATE TABLE IF NOT EXISTS game.corp_applications (
    application_id   bigserial PRIMARY KEY,
    corporation_id   bigint NOT NULL,
    character_id     bigint NOT NULL,
    application_text text NOT NULL,
    status           integer NOT NULL,
    created          bigint NOT NULL,
    custom_message   text NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS corp_applications_corp_idx ON game.corp_applications (corporation_id, status);
CREATE INDEX IF NOT EXISTS corp_applications_char_idx ON game.corp_applications (character_id, status);

-- Alliances (services/alliances.rs); IDs from 99,000,000 like the live server's.
CREATE TABLE IF NOT EXISTS game.alliances (
    alliance_id      bigserial PRIMARY KEY,
    name             text NOT NULL,
    short_name       text NOT NULL,
    description      text NOT NULL DEFAULT '',
    url              text NOT NULL DEFAULT '',
    executor_corp_id bigint,
    creator_corp_id  bigint NOT NULL,
    creator_char_id  bigint NOT NULL,
    dictatorial      boolean NOT NULL DEFAULT false,
    created          bigint NOT NULL,
    deleted          boolean NOT NULL DEFAULT false
);
CREATE UNIQUE INDEX IF NOT EXISTS alliances_name_idx ON game.alliances (lower(name)) WHERE NOT deleted;
CREATE UNIQUE INDEX IF NOT EXISTS alliances_short_name_idx ON game.alliances (upper(short_name)) WHERE NOT deleted;
SELECT setval('game.alliances_alliance_id_seq', GREATEST((SELECT last_value FROM game.alliances_alliance_id_seq), (SELECT max(alliance_id) FROM game.alliances), 99000000), true);
-- A corporation's alliance, since when, and the member corporation it backs as executor.
ALTER TABLE game.corporations ADD COLUMN IF NOT EXISTS alliance_id bigint;
ALTER TABLE game.corporations ADD COLUMN IF NOT EXISTS alliance_joined bigint;
ALTER TABLE game.corporations ADD COLUMN IF NOT EXISTS chosen_executor_id bigint;

-- Alliances a corporation has been in (alliance_id NULL: left), for its alliance history.
CREATE TABLE IF NOT EXISTS game.alliance_history (
    corporation_id bigint NOT NULL,
    alliance_id    bigint,
    start_date     bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS alliance_history_corp_idx ON game.alliance_history (corporation_id, start_date);

-- A corporation's application to an alliance (state: appConst.allianceApplication*).
CREATE TABLE IF NOT EXISTS game.alliance_applications (
    alliance_id      bigint NOT NULL,
    corporation_id   bigint NOT NULL,
    application_text text NOT NULL DEFAULT '',
    state            integer NOT NULL,
    created          bigint NOT NULL,
    PRIMARY KEY (alliance_id, corporation_id)
);

-- An alliance's declared relationships (appConst.allianceRelationship*) to other owners.
CREATE TABLE IF NOT EXISTS game.alliance_relationships (
    alliance_id  bigint NOT NULL,
    to_id        bigint NOT NULL,
    relationship integer NOT NULL,
    PRIMARY KEY (alliance_id, to_id)
);

-- Corporation offices (services/offices.rs): office_id is the office's item (typeOffice) at
-- the station, which holds the corporation hangar divisions' items (flagCorpSAG1-7).
CREATE TABLE IF NOT EXISTS game.offices (
    office_id      bigint PRIMARY KEY,
    corporation_id bigint NOT NULL,
    station_id     bigint NOT NULL,
    rented_until   bigint NOT NULL,
    UNIQUE (corporation_id, station_id)
);

-- Bills (services/bills.rs): bill_type is appConst.billType*, status appConst.bill*
-- (0 unpaid, 1 paid); external_id is what the bill is for (an office for rent).
CREATE TABLE IF NOT EXISTS game.bills (
    bill_id      bigserial PRIMARY KEY,
    bill_type    integer NOT NULL,
    debtor_id    bigint NOT NULL,
    creditor_id  bigint NOT NULL,
    amount       numeric(20, 2) NOT NULL,
    due          bigint NOT NULL,
    interest     double precision NOT NULL DEFAULT 0,
    external_id  bigint,
    external_id2 bigint,
    status       integer NOT NULL DEFAULT 0,
    created      bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS bills_debtor_idx ON game.bills (debtor_id, status);
CREATE INDEX IF NOT EXISTS bills_creditor_idx ON game.bills (creditor_id, status);
-- Which bill types an owner pays automatically, and from which wallet division.
CREATE TABLE IF NOT EXISTS game.bill_autopay (
    owner_id  bigint NOT NULL,
    bill_type integer NOT NULL,
    enabled   boolean NOT NULL,
    PRIMARY KEY (owner_id, bill_type)
);
ALTER TABLE game.corporations ADD COLUMN IF NOT EXISTS bill_division integer NOT NULL DEFAULT 1000;

-- Corporation wallet divisions (services/corpwallet.rs): account_key 1000-1006.
CREATE TABLE IF NOT EXISTS game.corp_wallets (
    corporation_id bigint NOT NULL,
    account_key    integer NOT NULL,
    balance        numeric(20, 2) NOT NULL DEFAULT 0,
    PRIMARY KEY (corporation_id, account_key)
);

-- services/slash.rs / dao/wallets: exact 36-digit corporation ISK storage.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
               WHERE table_schema = 'game' AND table_name = 'corp_wallets' AND column_name = 'balance'
                 AND (data_type <> 'numeric' OR numeric_precision IS DISTINCT FROM 36 OR numeric_scale IS DISTINCT FROM 2)) THEN
        ALTER TABLE game.corp_wallets ALTER COLUMN balance TYPE numeric(36, 2);
    END IF;
END $$;

-- Corporation and alliance bulletins, contacts and contact labels (services/orgs.rs);
-- owner_id is the corporation or alliance.
CREATE TABLE IF NOT EXISTS game.org_bulletins (
    bulletin_id  bigserial PRIMARY KEY,
    owner_id     bigint NOT NULL,
    title        text NOT NULL,
    body         text NOT NULL,
    edit_char_id bigint NOT NULL,
    edit_date    bigint NOT NULL,
    sort_order   integer NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS org_bulletins_owner_idx ON game.org_bulletins (owner_id);
CREATE TABLE IF NOT EXISTS game.org_contacts (
    owner_id        bigint NOT NULL,
    contact_id      bigint NOT NULL,
    relationship_id double precision NOT NULL,
    label_mask      bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (owner_id, contact_id)
);
-- label_id is the label's bit in a contact's label_mask.
CREATE TABLE IF NOT EXISTS game.org_labels (
    owner_id bigint NOT NULL,
    label_id bigint NOT NULL,
    name     text NOT NULL,
    color    integer,
    PRIMARY KEY (owner_id, label_id)
);

-- Corporations a character has joined, and when.
CREATE TABLE IF NOT EXISTS game.employment_history (
    char_id        bigint NOT NULL,
    corporation_id bigint NOT NULL,
    start_date     bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS employment_history_char_idx ON game.employment_history (char_id, start_date);

-- Ship insurance contracts (services/insurance.rs): payout fraction of the insured value.
CREATE TABLE IF NOT EXISTS game.insurance (
    ship_id    bigint PRIMARY KEY,
    owner_id   bigint NOT NULL,
    type_id    integer NOT NULL,
    fraction   double precision NOT NULL,
    start_date bigint NOT NULL,
    end_date   bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS insurance_owner_idx ON game.insurance (owner_id);

-- Jump clones (services/clones.rs): one per station per character.
CREATE TABLE IF NOT EXISTS game.jump_clones (
    clone_id    bigserial PRIMARY KEY,
    owner_id    bigint NOT NULL,
    location_id bigint NOT NULL,
    name        text NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS jump_clones_owner_idx ON game.jump_clones (owner_id);
-- Duplicate legacy clones fail the unique constraint rather than deleting player property.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.jump_clones'::regclass AND conname = 'jump_clones_owner_location_key') THEN
        DROP INDEX IF EXISTS game.jump_clones_owner_location_idx;
        ALTER TABLE game.jump_clones ADD CONSTRAINT jump_clones_owner_location_key UNIQUE (owner_id, location_id) DEFERRABLE INITIALLY IMMEDIATE;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.jump_clones'::regclass AND conname = 'jump_clones_clone_owner_key') THEN
        ALTER TABLE game.jump_clones ADD CONSTRAINT jump_clones_clone_owner_key UNIQUE (clone_id, owner_id);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.items'::regclass AND conname = 'items_jump_clone_owner_fkey') THEN
        ALTER TABLE game.items ADD CONSTRAINT items_jump_clone_owner_fkey FOREIGN KEY (jump_clone_id, owner_id)
            REFERENCES game.jump_clones (clone_id, owner_id) ON DELETE RESTRICT;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.items'::regclass AND conname = 'items_jump_clone_shape_check') THEN
        -- inventorycommon.const.flagImplant
        ALTER TABLE game.items ADD CONSTRAINT items_jump_clone_shape_check CHECK
            (jump_clone_id IS NULL OR (location_id = owner_id AND flag_id = 89 AND quantity = 1 AND singleton AND brain_slot IS NOT NULL));
    END IF;
END $$;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS last_clone_jump bigint;

-- Personal bookmark folders, subfolders and bookmarks (services/bookmarks.rs).
CREATE TABLE IF NOT EXISTS game.bookmark_folders (
    folder_id   bigserial PRIMARY KEY,
    owner_id    bigint NOT NULL,
    name        text NOT NULL,
    description text NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS bookmark_folders_owner_idx ON game.bookmark_folders (owner_id);
CREATE TABLE IF NOT EXISTS game.bookmark_subfolders (
    subfolder_id bigserial PRIMARY KEY,
    folder_id    bigint NOT NULL,
    name         text NOT NULL
);
CREATE TABLE IF NOT EXISTS game.bookmarks (
    bookmark_id  bigserial PRIMARY KEY,
    folder_id    bigint NOT NULL,
    subfolder_id bigint,
    creator_id   bigint NOT NULL,
    item_id      bigint,
    type_id      integer,
    location_id  bigint NOT NULL,  -- solar system
    x double precision NOT NULL, y double precision NOT NULL, z double precision NOT NULL,
    memo         text NOT NULL,
    note         text NOT NULL DEFAULT '',
    created      bigint NOT NULL,
    expiry       bigint
);
CREATE INDEX IF NOT EXISTS bookmarks_folder_idx ON game.bookmarks (folder_id);

-- Shared bookmark access groups and per-character folder subscriptions (dao/bookmarks).
CREATE TABLE IF NOT EXISTS game.access_groups (
    group_id bigserial PRIMARY KEY,
    creator_id bigint NOT NULL,
    name text NOT NULL,
    description text NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS game.access_group_members (
    group_id bigint NOT NULL REFERENCES game.access_groups(group_id) ON DELETE CASCADE,
    member_id bigint NOT NULL,
    membership_type integer NOT NULL CHECK (membership_type BETWEEN 0 AND 3),
    PRIMARY KEY (group_id, member_id)
);
CREATE TABLE IF NOT EXISTS game.access_group_logs (
    log_id bigserial PRIMARY KEY,
    group_id bigint NOT NULL,
    log_date bigint NOT NULL,
    character_id bigint NOT NULL,
    action_id integer NOT NULL,
    owner_id bigint,
    old_text text,
    new_text text,
    old_membership_type integer,
    new_membership_type integer
);
CREATE INDEX IF NOT EXISTS access_group_logs_group_idx ON game.access_group_logs (group_id, log_id);
ALTER TABLE game.bookmark_folders ADD COLUMN IF NOT EXISTS is_personal boolean NOT NULL DEFAULT true;
ALTER TABLE game.bookmark_folders ADD COLUMN IF NOT EXISTS admin_group_id bigint;
ALTER TABLE game.bookmark_folders ADD COLUMN IF NOT EXISTS manage_group_id bigint;
ALTER TABLE game.bookmark_folders ADD COLUMN IF NOT EXISTS use_group_id bigint;
ALTER TABLE game.bookmark_folders ADD COLUMN IF NOT EXISTS view_group_id bigint;
ALTER TABLE game.bookmark_subfolders ADD COLUMN IF NOT EXISTS creator_id bigint;
UPDATE game.bookmark_subfolders AS s SET creator_id = f.owner_id
    FROM game.bookmark_folders AS f WHERE s.folder_id = f.folder_id AND s.creator_id IS NULL;
CREATE TABLE IF NOT EXISTS game.bookmark_known_folders (
    character_id bigint NOT NULL,
    folder_id bigint NOT NULL REFERENCES game.bookmark_folders(folder_id) ON DELETE CASCADE,
    is_active boolean NOT NULL,
    PRIMARY KEY (character_id, folder_id)
);
CREATE TABLE IF NOT EXISTS game.bookmark_migrations (name text PRIMARY KEY);
WITH migration AS (
    INSERT INTO game.bookmark_migrations (name) VALUES ('personal_known_folders')
    ON CONFLICT DO NOTHING RETURNING name
)
INSERT INTO game.bookmark_known_folders (character_id, folder_id, is_active)
SELECT f.owner_id, f.folder_id, true FROM game.bookmark_folders AS f CROSS JOIN migration
WHERE f.is_personal
ON CONFLICT (character_id, folder_id) DO NOTHING;

-- Calendar events (services/calendar.rs): owned by a character or a player corporation.
CREATE TABLE IF NOT EXISTS game.calendar_events (
    event_id    bigserial PRIMARY KEY,
    owner_id    bigint NOT NULL,
    creator_id  bigint NOT NULL,
    event_date  bigint NOT NULL,  -- FILETIME
    duration    integer NOT NULL DEFAULT 0,
    title       text NOT NULL,
    description text NOT NULL DEFAULT '',
    importance  integer NOT NULL DEFAULT 0,
    modified    bigint NOT NULL,
    deleted     boolean NOT NULL DEFAULT false
);
CREATE INDEX IF NOT EXISTS calendar_events_owner_idx ON game.calendar_events (owner_id, event_date);
CREATE TABLE IF NOT EXISTS game.calendar_responses (
    event_id bigint NOT NULL,
    char_id  bigint NOT NULL,
    status   integer NOT NULL,  -- appConst.eventResponse*
    PRIMARY KEY (event_id, char_id)
);
CREATE INDEX IF NOT EXISTS calendar_responses_char_idx ON game.calendar_responses (char_id);

-- Loyalty points per owner (character or corporation) and issuing NPC corporation.
CREATE TABLE IF NOT EXISTS game.lp_balances (
    char_id bigint NOT NULL,
    corp_id bigint NOT NULL,
    amount  bigint NOT NULL,
    PRIMARY KEY (char_id, corp_id)
);

-- LP store catalogue (dao::lp_offers); requirements retain their JSON array order.
CREATE TABLE IF NOT EXISTS game.lp_offers (
    corporation_id bigint NOT NULL,
    offer_id bigint NOT NULL,
    type_id integer NOT NULL CHECK (type_id > 0),
    quantity integer NOT NULL CHECK (quantity > 0),
    lp_cost bigint NOT NULL CHECK (lp_cost >= 0),
    isk_cost bigint NOT NULL CHECK (isk_cost >= 0),
    ak_cost bigint NOT NULL CHECK (ak_cost >= 0),
    required_items jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(required_items) = 'array'),
    PRIMARY KEY (corporation_id, offer_id)
);

-- Personal skill plans (public gateway eve_public.character.skill.plan).
CREATE TABLE IF NOT EXISTS game.skill_plans (
    plan_id      bytea PRIMARY KEY,         -- 16-byte UUID
    character_id bigint NOT NULL REFERENCES game.characters,
    name         text NOT NULL DEFAULT '',
    description  text NOT NULL DEFAULT '',
    -- Requirements in plan order: skill_ids[i] to levels[i].
    skill_ids    integer[] NOT NULL DEFAULT '{}',
    levels       integer[] NOT NULL DEFAULT '{}',
    active       boolean NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS skill_plans_character_idx ON game.skill_plans (character_id);

-- The client's Python 2 UCS2 ABI counts UTF-16 units; supplementary code points use two.
CREATE OR REPLACE FUNCTION game.skill_plan_text_units(text) RETURNS integer
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
AS $$ SELECT char_length($1) + regexp_count($1 COLLATE "C", U&'[\+010000-\+10FFFF]') $$;

-- Personal milestone definitions, viewer snapshots and active tracking.
DO $$
DECLARE bad record;
BEGIN
    SELECT plan_id, character_id INTO bad FROM game.skill_plans
    WHERE octet_length(plan_id) <> 16 OR plan_id = decode(repeat('00', 16), 'hex')
       OR cardinality(skill_ids) <> cardinality(levels)
       OR cardinality(skill_ids) > 150
       OR game.skill_plan_text_units(name) > 50 OR game.skill_plan_text_units(description) > 1000
       OR coalesce(array_ndims(skill_ids), 1) <> 1 OR coalesce(array_ndims(levels), 1) <> 1
       OR CASE WHEN coalesce(array_ndims(skill_ids), 1) = 1 THEN array_position(skill_ids, NULL) IS NOT NULL ELSE false END
       OR CASE WHEN coalesce(array_ndims(levels), 1) = 1 THEN array_position(levels, NULL) IS NOT NULL ELSE false END
       OR EXISTS (SELECT 1 FROM unnest(skill_ids) AS s WHERE s <= 0)
       OR EXISTS (SELECT 1 FROM unnest(levels) AS l WHERE l < 0 OR l > 5)
    LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'invalid skill plan %, character %', encode(bad.plan_id, 'hex'), bad.character_id;
    END IF;
END $$;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.skill_plans'::regclass AND conname = 'skill_plans_shape_check') THEN
        ALTER TABLE game.skill_plans ADD CONSTRAINT skill_plans_shape_check CHECK (
            octet_length(plan_id) = 16 AND plan_id <> decode(repeat('00', 16), 'hex')
            AND cardinality(skill_ids) = cardinality(levels) AND cardinality(skill_ids) <= 150
            AND coalesce(array_ndims(skill_ids), 1) = 1 AND coalesce(array_ndims(levels), 1) = 1
            AND array_position(skill_ids, NULL) IS NULL AND array_position(levels, NULL) IS NULL
            AND 0 < ALL(skill_ids) AND 0 <= ALL(levels) AND 5 >= ALL(levels)
        );
    END IF;
END $$;
CREATE TABLE IF NOT EXISTS game.skill_plan_milestones (
    milestone_id bytea PRIMARY KEY CHECK (octet_length(milestone_id) = 16 AND milestone_id <> decode(repeat('00', 16), 'hex')),
    plan_id bytea NOT NULL REFERENCES game.skill_plans(plan_id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('type', 'skill')),
    type_id integer NOT NULL CHECK (type_id > 0),
    level integer,
    description text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((kind = 'type' AND level IS NULL) OR (kind = 'skill' AND level IS NOT NULL AND level BETWEEN 1 AND 5))
);
CREATE INDEX IF NOT EXISTS skill_plan_milestones_plan_idx ON game.skill_plan_milestones(plan_id);
CREATE UNIQUE INDEX IF NOT EXISTS skill_plan_milestones_target_idx ON game.skill_plan_milestones(plan_id, kind, type_id, coalesce(level, 0));
CREATE TABLE IF NOT EXISTS game.skill_plan_milestone_progress (
    character_id bigint NOT NULL REFERENCES game.characters(character_id) ON DELETE CASCADE,
    milestone_id bytea NOT NULL REFERENCES game.skill_plan_milestones(milestone_id) ON DELETE CASCADE,
    required_sp bigint NOT NULL CHECK (required_sp >= 0),
    trained_sp bigint NOT NULL CHECK (trained_sp >= 0),
    queued_sp bigint NOT NULL CHECK (queued_sp >= 0),
    completed boolean NOT NULL,
    evaluated_at bigint NOT NULL,
    PRIMARY KEY (character_id, milestone_id)
);
ALTER TABLE game.skill_plan_milestone_progress ADD COLUMN IF NOT EXISTS valid boolean NOT NULL DEFAULT true;
CREATE TABLE IF NOT EXISTS game.skill_plan_tracking (
    character_id bigint PRIMARY KEY REFERENCES game.characters(character_id) ON DELETE CASCADE,
    personal_plan_id bytea REFERENCES game.skill_plans(plan_id) ON DELETE CASCADE,
    certified_plan_id bigint CHECK (certified_plan_id BETWEEN 1 AND 4294967295),
    CHECK ((personal_plan_id IS NULL) <> (certified_plan_id IS NULL))
);
DO $$
DECLARE bad record;
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = 'game' AND table_name = 'skill_plans' AND column_name = 'active') THEN
        SELECT character_id, count(*) AS n INTO bad FROM game.skill_plans WHERE active GROUP BY character_id HAVING count(*) > 1 LIMIT 1;
        IF FOUND THEN RAISE EXCEPTION 'multiple active skill plans for character %', bad.character_id; END IF;
        SELECT p.character_id, p.plan_id INTO bad FROM game.skill_plans p
        JOIN game.skill_plan_tracking t ON t.character_id = p.character_id
        WHERE p.active AND (t.personal_plan_id IS DISTINCT FROM p.plan_id OR t.certified_plan_id IS NOT NULL) LIMIT 1;
        IF FOUND THEN RAISE EXCEPTION 'conflicting active skill plan for character %, plan %', bad.character_id, encode(bad.plan_id, 'hex'); END IF;
        INSERT INTO game.skill_plan_tracking(character_id, personal_plan_id)
            SELECT character_id, plan_id FROM game.skill_plans WHERE active ON CONFLICT (character_id) DO NOTHING;
        ALTER TABLE game.skill_plans DROP COLUMN active;
    END IF;
END $$;

-- Retail Serenity average market prices (crate::retail), loaded at startup.
CREATE TABLE IF NOT EXISTS game.retail_prices (
    type_id  integer PRIMARY KEY,
    average  double precision NOT NULL,
    adjusted double precision NOT NULL
);

-- New Eden Store: the account's PLEX vault (whole PLEX).
ALTER TABLE game.accounts ADD COLUMN IF NOT EXISTS plex_balance bigint NOT NULL DEFAULT 1000;

-- services/slash.rs / dao/accounts: exact 36-digit PLEX storage.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
               WHERE table_schema = 'game' AND table_name = 'accounts' AND column_name = 'plex_balance'
                 AND (data_type <> 'numeric' OR numeric_precision IS DISTINCT FROM 36 OR numeric_scale IS DISTINCT FROM 0)) THEN
        ALTER TABLE game.accounts ALTER COLUMN plex_balance TYPE numeric(36, 0);
    END IF;
END $$;

-- dao/accounts/plex: exact append-only account vault history and conversion references.
CREATE SEQUENCE IF NOT EXISTS game.plex_vault_reference_seq;
CREATE TABLE IF NOT EXISTS game.plex_vault_transactions (
    transaction_id bigserial PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES game.accounts,
    actor_char_id bigint,
    delta numeric(36,0) NOT NULL,
    balance_after numeric(36,0) NOT NULL,
    kind text NOT NULL,
    created bigint NOT NULL,
    item_id bigint,
    reference bigint
);
CREATE UNIQUE INDEX IF NOT EXISTS plex_vault_opening_user_idx ON game.plex_vault_transactions (user_id) WHERE kind = 'opening';
CREATE UNIQUE INDEX IF NOT EXISTS plex_vault_reference_idx ON game.plex_vault_transactions (reference) WHERE reference IS NOT NULL;
-- Market settlement identities owned by services/plex_vault.
ALTER TABLE game.plex_vault_transactions ADD COLUMN IF NOT EXISTS order_id bigint;
ALTER TABLE game.plex_vault_transactions ADD COLUMN IF NOT EXISTS trade_id bigint;
CREATE UNIQUE INDEX IF NOT EXISTS plex_vault_market_fill_idx ON game.plex_vault_transactions (order_id, trade_id, user_id) WHERE kind = 'market_fill';
CREATE UNIQUE INDEX IF NOT EXISTS plex_vault_market_refund_idx ON game.plex_vault_transactions (order_id) WHERE kind = 'market_refund';

-- Redeeming queue: items owed to an account (store purchases, login rewards),
-- turned into items when claimed onto a character.
CREATE TABLE IF NOT EXISTS game.redeem_tokens (
    token_id    bigserial PRIMARY KEY,
    user_id     bigint NOT NULL REFERENCES game.accounts,
    type_id     integer NOT NULL,
    quantity    bigint NOT NULL,
    label       text NOT NULL DEFAULT '',
    description text NOT NULL DEFAULT '',
    created     bigint NOT NULL  -- FILETIME
);
CREATE INDEX IF NOT EXISTS redeem_tokens_user_idx ON game.redeem_tokens (user_id);

-- Store purchases, for single-purchase and once-per-period offers.
CREATE TABLE IF NOT EXISTS game.store_purchases (
    user_id      bigint NOT NULL REFERENCES game.accounts,
    offer_id     integer NOT NULL,
    quantity     bigint NOT NULL,
    price        bigint NOT NULL,
    purchased_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS store_purchases_user_idx ON game.store_purchases (user_id, offer_id);

-- Seasonal login reward claims: one row per account per reward day.
CREATE TABLE IF NOT EXISTS game.login_reward_claims (
    user_id     bigint NOT NULL REFERENCES game.accounts,
    campaign_id integer NOT NULL,
    day         date NOT NULL,      -- reward date (after the daily turnover)
    clone_grade integer NOT NULL,
    PRIMARY KEY (user_id, campaign_id, day)
);

-- Seasonal Omega retrogrants: one audited grant per historical claim.
CREATE TABLE IF NOT EXISTS game.login_reward_retro_grants (
    user_id     bigint NOT NULL,
    campaign_id integer NOT NULL,
    day         date NOT NULL,
    type_id     integer NOT NULL,
    quantity    integer NOT NULL CHECK (quantity > 0),
    granted_at  bigint NOT NULL, -- FILETIME
    PRIMARY KEY (user_id, campaign_id, day),
    FOREIGN KEY (user_id, campaign_id, day)
        REFERENCES game.login_reward_claims (user_id, campaign_id, day)
);

-- Agent missions: one per character and agent (services/missions.rs). The site is a
-- deadspace point in system_id; its NPCs are spawned on first approach.
CREATE TABLE IF NOT EXISTS game.agent_missions (
    character_id bigint NOT NULL REFERENCES game.characters,
    agent_id     bigint NOT NULL,
    mission_id   integer NOT NULL,   -- sde_native.missions key (contentID)
    dungeon_id   integer NOT NULL,
    faction_id   bigint NOT NULL,
    state        integer NOT NULL,   -- appConst.agentMissionState*
    system_id    bigint NOT NULL,
    x            double precision NOT NULL,
    y            double precision NOT NULL,
    z            double precision NOT NULL,
    reward_isk   bigint NOT NULL,
    reward_lp    bigint NOT NULL,
    offered_at   bigint NOT NULL,    -- FILETIME
    accepted_at  bigint,
    PRIMARY KEY (character_id, agent_id)
);

-- Fitted modules' online state (dogma effectOnline), kept across sessions.
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS online boolean NOT NULL DEFAULT false;

-- Saved ship fittings (charFittingMgr): fit_* arrays are fitData (typeID, flag, qty) rows.
CREATE TABLE IF NOT EXISTS game.fittings (
    fitting_id   bigserial PRIMARY KEY,
    owner_id     bigint NOT NULL,
    name         text NOT NULL DEFAULT '',
    description  text NOT NULL DEFAULT '',
    ship_type_id integer NOT NULL,
    fit_types    integer[] NOT NULL DEFAULT '{}',
    fit_flags    integer[] NOT NULL DEFAULT '{}',
    fit_qty      bigint[] NOT NULL DEFAULT '{}'
);
-- dao/fittings: preserve legacy array bounds and elements when widening quantities.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
               WHERE table_schema = 'game' AND table_name = 'fittings' AND column_name = 'fit_qty'
                 AND data_type = 'ARRAY' AND udt_schema = 'pg_catalog' AND udt_name = '_int4') THEN
        ALTER TABLE game.fittings ALTER COLUMN fit_qty TYPE bigint[] USING fit_qty::bigint[];
    ELSIF NOT EXISTS (SELECT 1 FROM information_schema.columns
                      WHERE table_schema = 'game' AND table_name = 'fittings' AND column_name = 'fit_qty'
                        AND data_type = 'ARRAY' AND udt_schema = 'pg_catalog' AND udt_name = '_int8') THEN
        RAISE EXCEPTION 'unsupported game.fittings.fit_qty type';
    END IF;
END $$;
CREATE INDEX IF NOT EXISTS fittings_owner_idx ON game.fittings (owner_id);

-- Charges loaded in fitted modules. The client keys them as the sublocation
-- (shipID, flagID, typeID), not as inventory items (services/combat.rs).
CREATE TABLE IF NOT EXISTS game.module_charges (
    ship_id  bigint NOT NULL,
    flag_id  integer NOT NULL,
    type_id  integer NOT NULL,
    quantity bigint NOT NULL,
    PRIMARY KEY (ship_id, flag_id)
);

-- In-flight reload reservations (dao/items/pending_reloads).
CREATE TABLE IF NOT EXISTS game.pending_reloads (
    reload_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner_id  bigint NOT NULL,
    ship_id   bigint NOT NULL,
    module_id bigint NOT NULL,
    flag_id   integer NOT NULL,
    type_id   integer NOT NULL,
    quantity  bigint NOT NULL CHECK (quantity > 0)
);

CREATE INDEX IF NOT EXISTS pending_reloads_ship_id_idx ON game.pending_reloads (ship_id);
-- inventorycommon.const.flagCargo = 5; legacy reloads previously returned to cargo.
ALTER TABLE game.pending_reloads ADD COLUMN IF NOT EXISTS source_flag integer NOT NULL DEFAULT 5;
ALTER TABLE game.pending_reloads ADD COLUMN IF NOT EXISTS source_location_id bigint;
UPDATE game.pending_reloads SET source_location_id = ship_id WHERE source_location_id IS NULL;
ALTER TABLE game.pending_reloads ALTER COLUMN source_location_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS pending_reloads_source_idx ON game.pending_reloads (source_location_id, source_flag);

-- Weapon groups: each slave module fires with its master.
CREATE TABLE IF NOT EXISTS game.weapon_banks (
    ship_id   bigint NOT NULL,
    master_id bigint NOT NULL,
    slave_id  bigint PRIMARY KEY
);

-- Militias a player corporation's members may enlist in directly (fwCharacterEnlistmentMgr).
CREATE TABLE IF NOT EXISTS game.corp_enlistment_factions (
    corporation_id bigint NOT NULL,
    faction_id     bigint NOT NULL,
    PRIMARY KEY (corporation_id, faction_id)
);

-- Exploration (services/exploration.rs): cosmic anomalies and signatures per solar system,
-- wormhole connections (both ends share lifetime and mass), hackable site containers.
CREATE SEQUENCE IF NOT EXISTS game.exploration_site_seq START 9500000000000;
CREATE TABLE IF NOT EXISTS game.exploration_systems (
    system_id    bigint PRIMARY KEY,
    generated_at bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS game.exploration_sites (
    site_id         bigint PRIMARY KEY,
    system_id       bigint NOT NULL,
    target          text NOT NULL,
    kind            smallint NOT NULL,          -- 0 anomaly, 1 signature
    strength_attr   bigint NOT NULL,
    ease            double precision NOT NULL,
    dungeon_id      bigint,
    name_id         bigint,
    archetype       bigint,
    faction         bigint,
    difficulty      bigint NOT NULL,
    x               double precision NOT NULL,
    y               double precision NOT NULL,
    z               double precision NOT NULL,
    overlay_x       double precision NOT NULL,
    overlay_y       double precision NOT NULL,
    overlay_z       double precision NOT NULL,
    deviation       double precision NOT NULL,
    in_space        boolean NOT NULL DEFAULT false,
    containers      bigint[],                   -- hackable containers (game.items) once spawned
    hacked          bigint[] NOT NULL DEFAULT '{}',
    wh_type         integer,
    wh_dest_system  bigint,
    wh_dest_class   bigint,
    wh_other        bigint,
    wh_born         bigint,
    wh_life_minutes double precision,
    wh_mass_max     double precision,
    wh_mass_used    double precision,
    wh_jump_mass    double precision,
    wh_nebula       bigint
);
CREATE INDEX IF NOT EXISTS exploration_sites_system ON game.exploration_sites (system_id);
-- Signatures a character has scanned down to a warpable result.
CREATE TABLE IF NOT EXISTS game.exploration_scanned (
    character_id bigint NOT NULL,
    site_id      bigint NOT NULL,
    PRIMARY KEY (character_id, site_id)
);

-- Mission content beyond kill missions (services/missions.rs): talk-to-agent, courier,
-- trade and agent-interaction steps, and the epic arc a mission belongs to.
ALTER TABLE game.agent_missions ADD COLUMN IF NOT EXISTS kind text NOT NULL DEFAULT 'kill';
ALTER TABLE game.agent_missions ADD COLUMN IF NOT EXISTS arc_id bigint;
ALTER TABLE game.agent_missions ADD COLUMN IF NOT EXISTS dest_agent bigint;
ALTER TABLE game.agent_missions ADD COLUMN IF NOT EXISTS cargo_type integer;
ALTER TABLE game.agent_missions ADD COLUMN IF NOT EXISTS cargo_qty integer;
-- Epic arc progress per character: steps unlocked, accepted, completed or quit.
CREATE TABLE IF NOT EXISTS game.epic_arc_missions (
    character_id bigint NOT NULL,
    arc_id       bigint NOT NULL,
    mission_id   integer NOT NULL,
    state        text NOT NULL,      -- unlocked | accepted | completed | quit
    accepted_at  bigint,
    completed_at bigint,
    quit_at      bigint,
    PRIMARY KEY (character_id, arc_id, mission_id)
);

-- Character deletion (charUnboundMgr.PrepareCharacterForDelete): when the biomass queue
-- lets the character go (FILETIME); DeleteCharacter then marks it deleted.
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS delete_prepare_at bigint;

-- The AIR new player experience state per character (services/npe.rs):
-- storylines.common.airnpeconstants.AirNpeState (1 active, 2 completed, 3 skipped).
CREATE TABLE IF NOT EXISTS game.air_npe_state (
    character_id bigint PRIMARY KEY,
    state        smallint NOT NULL,
    changed_at   bigint NOT NULL
);

-- Mining (services/mining.rs): rocks of asteroid belts and of ore/ice/gas sites
-- (field_id: the belt's celestial ID or the exploration site ID), and belts' regrowth.
CREATE SEQUENCE IF NOT EXISTS game.asteroid_seq START 9700000000000;
CREATE TABLE IF NOT EXISTS game.asteroids (
    asteroid_id       bigint PRIMARY KEY DEFAULT nextval('game.asteroid_seq'),
    system_id         bigint NOT NULL,
    field_id          bigint NOT NULL,
    type_id           integer NOT NULL,
    quantity          bigint NOT NULL,
    original_quantity bigint NOT NULL,
    radius            real,
    x                 double precision NOT NULL,
    y                 double precision NOT NULL,
    z                 double precision NOT NULL
);
CREATE INDEX IF NOT EXISTS asteroids_system ON game.asteroids (system_id);
CREATE TABLE IF NOT EXISTS game.asteroid_fields (
    field_id    bigint PRIMARY KEY,
    system_id   bigint NOT NULL,
    grown_at    bigint NOT NULL,
    depleted_at bigint
);

-- Personal mining deliveries (dao/mining_ledger.rs); event_time is FILETIME.
CREATE TABLE IF NOT EXISTS game.character_mining_ledger (
    log_id bigserial PRIMARY KEY,
    character_id bigint NOT NULL,
    event_time bigint NOT NULL,
    solar_system_id bigint NOT NULL,
    type_id integer NOT NULL,
    quantity bigint NOT NULL CHECK (quantity > 0),
    quantity_wasted bigint NOT NULL CHECK (quantity_wasted >= 0)
);
CREATE INDEX IF NOT EXISTS character_mining_ledger_character_time
    ON game.character_mining_ledger (character_id, event_time DESC, log_id DESC);
CREATE INDEX IF NOT EXISTS character_mining_ledger_time
    ON game.character_mining_ledger (event_time);

-- Durable mining progress snapshots; legacy rows were already consumed.
ALTER TABLE game.character_mining_ledger ADD COLUMN IF NOT EXISTS progress_aggregated boolean NOT NULL DEFAULT true;
ALTER TABLE game.character_mining_ledger ADD COLUMN IF NOT EXISTS source_ship_id bigint;
ALTER TABLE game.character_mining_ledger ADD COLUMN IF NOT EXISTS source_ship_type_id integer;
ALTER TABLE game.character_mining_ledger ADD COLUMN IF NOT EXISTS corporation_id bigint;
ALTER TABLE game.character_mining_ledger ADD COLUMN IF NOT EXISTS alliance_id bigint;
ALTER TABLE game.character_mining_ledger ADD COLUMN IF NOT EXISTS faction_id bigint;
ALTER TABLE game.character_mining_ledger ADD COLUMN IF NOT EXISTS resource_kind text;
ALTER TABLE game.character_mining_ledger ADD COLUMN IF NOT EXISTS unit_volume_m3 double precision;
CREATE INDEX IF NOT EXISTS character_mining_ledger_pending
    ON game.character_mining_ledger (log_id) WHERE NOT progress_aggregated;

-- Durable scheduler (dao/jobs.rs); all timestamps are FILETIME ticks.
CREATE SEQUENCE IF NOT EXISTS game.jobs_lease_seq;
CREATE TABLE IF NOT EXISTS game.jobs (
    job_id text PRIMARY KEY,
    kind text NOT NULL,
    due_at bigint NOT NULL,
    payload text NOT NULL DEFAULT '{}',
    state text NOT NULL CHECK (state IN ('scheduled', 'running', 'dead')),
    attempt integer NOT NULL DEFAULT 0,
    lease_until bigint,
    lease_token bigint,
    last_error text,
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS jobs_due ON game.jobs (state, due_at);
-- Lua script-owned typed persistent values.
CREATE TABLE IF NOT EXISTS game.lua_store (
    script text NOT NULL,
    key text NOT NULL,
    value jsonb NOT NULL,
    PRIMARY KEY (script, key)
);
-- Immutable server-authored policy packages (dao/lua_content).
CREATE TABLE IF NOT EXISTS game.lua_content_versions (
    namespace text NOT NULL CHECK (namespace <> ''),
    version text NOT NULL CHECK (version <> ''),
    schema_version integer NOT NULL CHECK (schema_version > 0),
    compiler_identity text NOT NULL CHECK (compiler_identity <> ''),
    registration bytea NOT NULL,
    source bytea NOT NULL,
    data bytea NOT NULL,
    fingerprint bytea NOT NULL CHECK (octet_length(fingerprint) = 32),
    PRIMARY KEY (namespace, version)
);

-- Durable publication order for all retained Lua packages (dao/lua_content/activations).
CREATE TABLE IF NOT EXISTS game.lua_content_activations (
    ordinal bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    namespace text NOT NULL,
    version text NOT NULL,
    fingerprint bytea NOT NULL CHECK (octet_length(fingerprint) = 32),
    published_at bigint NOT NULL CHECK (published_at > 0),
    FOREIGN KEY (namespace, version) REFERENCES game.lua_content_versions(namespace, version) ON DELETE RESTRICT
);
CREATE INDEX IF NOT EXISTS lua_content_activations_namespace_ordinal_idx
    ON game.lua_content_activations (namespace, ordinal DESC);

-- Persistent adaptation chip ledger (dao/ship_adaption).
CREATE TABLE IF NOT EXISTS game.ship_adaption_characters (
    character_id bigint PRIMARY KEY REFERENCES game.characters(character_id),
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    content_namespace text NOT NULL CHECK (content_namespace = 'ship_adaption'),
    content_version text NOT NULL,
    FOREIGN KEY (content_namespace, content_version)
        REFERENCES game.lua_content_versions(namespace, version) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS game.ship_adaption_chips (
    character_id bigint NOT NULL REFERENCES game.ship_adaption_characters(character_id),
    type_id integer NOT NULL,
    balance bigint NOT NULL CHECK (balance >= 0 AND balance <= 4294967295),
    PRIMARY KEY (character_id, type_id)
);

-- Sovereignty Hub persistence (dao/sovereignty); callers lock policy before systems.
CREATE TABLE IF NOT EXISTS game.sov_systems (
    system_id bigint PRIMARY KEY,
    hub_item_id bigint UNIQUE,
    claim_corporation_id bigint,
    alliance_id bigint,
    claim_time bigint,
    ownership_epoch bigint NOT NULL DEFAULT 0,
    revision bigint NOT NULL DEFAULT 0,
    CONSTRAINT sov_systems_claim_group CHECK (
        (claim_corporation_id IS NULL AND alliance_id IS NULL AND claim_time IS NULL) OR
        (hub_item_id IS NOT NULL AND claim_corporation_id IS NOT NULL AND alliance_id IS NOT NULL AND claim_time IS NOT NULL)),
    CONSTRAINT sov_systems_epoch_revision CHECK (ownership_epoch >= 0 AND revision >= 0)
);
CREATE INDEX IF NOT EXISTS sov_systems_claim_corporation_idx
    ON game.sov_systems (claim_corporation_id, system_id) WHERE claim_corporation_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS game.sov_structures (
    item_id bigint PRIMARY KEY REFERENCES game.items(item_id) ON DELETE RESTRICT,
    system_id bigint NOT NULL UNIQUE REFERENCES game.sov_systems(system_id) ON DELETE RESTRICT,
    source_item_id bigint NOT NULL,
    deployer_character_id bigint NOT NULL,
    deployer_corporation_id bigint NOT NULL,
    deployed_at bigint NOT NULL,
    content_namespace text NOT NULL,
    content_version text NOT NULL,
    self_destruct_due_at bigint,
    self_destruct_actor bigint,
    CONSTRAINT sov_structures_destruct_pair CHECK ((self_destruct_due_at IS NULL) = (self_destruct_actor IS NULL)),
    UNIQUE (item_id, system_id),
    FOREIGN KEY (content_namespace, content_version) REFERENCES game.lua_content_versions(namespace, version) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS game.sov_capture_targets (
    target_item_id bigint PRIMARY KEY REFERENCES game.sov_structures(item_id) ON DELETE RESTRICT,
    system_id bigint NOT NULL REFERENCES game.sov_systems(system_id) ON DELETE RESTRICT,
    ownership_epoch bigint NOT NULL,
    controlling_team bigint,
    scoring_team bigint,
    defender_team bigint NOT NULL,
    score double precision NOT NULL,
    rate double precision NOT NULL,
    blocked boolean NOT NULL,
    score_sim_time bigint NOT NULL,
    checkpoint_wall_time bigint NOT NULL,
    revision bigint NOT NULL,
    capture_time_seconds double precision NOT NULL,
    defensive_regen_rate double precision NOT NULL,
    checkpoint_interval_ms bigint NOT NULL,
    content_namespace text NOT NULL,
    content_version text NOT NULL,
    CONSTRAINT sov_capture_revision CHECK (ownership_epoch >= 0 AND revision >= 0),
    CONSTRAINT sov_capture_score CHECK (score >= 0 AND score <= 1),
    CONSTRAINT sov_capture_rate CHECK (rate > '-Infinity'::float8 AND rate < 'Infinity'::float8 AND (NOT blocked OR rate = 0)),
    CONSTRAINT sov_capture_parameters CHECK (capture_time_seconds > 0 AND capture_time_seconds < 'Infinity'::float8 AND defensive_regen_rate >= 0 AND defensive_regen_rate < 'Infinity'::float8 AND checkpoint_interval_ms > 0),
    FOREIGN KEY (target_item_id, system_id) REFERENCES game.sov_structures(item_id, system_id) ON DELETE RESTRICT,
    FOREIGN KEY (content_namespace, content_version) REFERENCES game.lua_content_versions(namespace, version) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS game.sov_alliance_policy (
    alliance_id bigint PRIMARY KEY,
    current_capital_system bigint,
    last_capital_changed_at bigint,
    capital_lost_at bigint,
    capital_change_not_before bigint,
    revision bigint NOT NULL DEFAULT 0,
    CONSTRAINT sov_policy_revision CHECK (revision >= 0)
);
CREATE TABLE IF NOT EXISTS game.sov_history (
    history_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    system_id bigint NOT NULL,
    hub_item_id bigint NOT NULL,
    old_corporation_id bigint,
    new_corporation_id bigint,
    old_alliance_id bigint,
    new_alliance_id bigint,
    old_epoch bigint NOT NULL,
    new_epoch bigint NOT NULL,
    reason text NOT NULL,
    at bigint NOT NULL,
    actor_id bigint,
    operation_key text NOT NULL UNIQUE,
    content_namespace text NOT NULL,
    content_version text NOT NULL,
    CONSTRAINT sov_history_epochs CHECK (old_epoch >= 0 AND new_epoch >= 0),
    FOREIGN KEY (content_namespace, content_version) REFERENCES game.lua_content_versions(namespace, version) ON DELETE RESTRICT
);
CREATE INDEX IF NOT EXISTS sov_history_time_idx ON game.sov_history (at DESC, history_id DESC);
CREATE TABLE IF NOT EXISTS game.sov_link_sources (
    character_id bigint PRIMARY KEY REFERENCES game.characters(character_id) ON DELETE RESTRICT,
    revision bigint NOT NULL DEFAULT 0,
    CONSTRAINT sov_link_sources_revision CHECK (revision >= 0)
);
CREATE TABLE IF NOT EXISTS game.sov_link_source_changes (
    character_id bigint NOT NULL REFERENCES game.sov_link_sources(character_id) ON DELETE RESTRICT,
    revision bigint NOT NULL,
    effective_at bigint NOT NULL,
    PRIMARY KEY (character_id, revision),
    CONSTRAINT sov_link_source_changes_revision CHECK (revision > 0)
);
-- Direct station trades (services/trade): container IDs share the item sequence.
CREATE TABLE IF NOT EXISTS game.trades (
    trade_id bigint PRIMARY KEY DEFAULT nextval('game.items_item_id_seq'),
    station_id bigint NOT NULL,
    char_a bigint NOT NULL,
    char_b bigint NOT NULL,
    money_a numeric NOT NULL DEFAULT 0,
    money_b numeric NOT NULL DEFAULT 0,
    accepted_a boolean NOT NULL DEFAULT false,
    accepted_b boolean NOT NULL DEFAULT false,
    state text NOT NULL CHECK (state IN ('open', 'done', 'cancelled')),
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS game.trade_items (
    item_id bigint PRIMARY KEY,
    trade_id bigint NOT NULL REFERENCES game.trades(trade_id),
    owner_id bigint NOT NULL,
    origin_location bigint NOT NULL,
    origin_flag integer NOT NULL
);
CREATE INDEX IF NOT EXISTS trade_items_trade_id ON game.trade_items (trade_id);
-- Align item IDs with existing trade container IDs.
SELECT setval('game.items_item_id_seq', GREATEST(
    (SELECT last_value FROM game.items_item_id_seq),
    (SELECT max(item_id) FROM game.items),
    (SELECT max(trade_id) FROM game.trades),
    1000000000000
), true);

-- Player bounties and kill rights (dao/bounties.rs, dao/kill_rights.rs).
CREATE TABLE IF NOT EXISTS game.bounties (
    target_id bigint PRIMARY KEY,
    kind smallint NOT NULL,
    amount numeric(36,2) NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS game.bounty_contributions (
    contribution_id bigserial PRIMARY KEY,
    target_id bigint NOT NULL,
    placer_id bigint NOT NULL,
    amount numeric(36,2) NOT NULL,
    placed_at bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS game.bounty_payouts (
    killmail_id bigint PRIMARY KEY,
    hunter_id bigint NOT NULL,
    amount numeric(36,2) NOT NULL,
    paid_at bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS game.kill_rights (
    kill_right_id bigserial PRIMARY KEY,
    from_id bigint NOT NULL,
    to_id bigint NOT NULL,
    created_at bigint NOT NULL,
    expiry bigint NOT NULL,
    price numeric(36,2),
    restricted_to bigint,
    activated_until bigint,
    activated_by bigint,
    used_at bigint
);
CREATE UNIQUE INDEX IF NOT EXISTS kill_rights_unused_pair ON game.kill_rights (from_id, to_id) WHERE used_at IS NULL;

-- Corporate contract ownership and captured custody (dao/contracts).
-- Defaults: 1000 is the client's master wallet key (appConst accountingKeyCash, ACCOUNT_CASH in consts.rs); 4 is the station hangar flag (inventorycommon const flagHangar, FLAG_HANGAR).
ALTER TABLE game.contracts ADD COLUMN IF NOT EXISTS for_corp boolean NOT NULL DEFAULT false;
ALTER TABLE game.contracts ADD COLUMN IF NOT EXISTS issuer_wallet_key integer NOT NULL DEFAULT 1000;
ALTER TABLE game.contracts ADD COLUMN IF NOT EXISTS acceptor_wallet_key integer NOT NULL DEFAULT 1000;
ALTER TABLE game.contracts ADD COLUMN IF NOT EXISTS issuer_hangar_flag integer NOT NULL DEFAULT 4;
ALTER TABLE game.contracts ADD COLUMN IF NOT EXISTS issuer_source_location bigint;
UPDATE game.contracts SET issuer_source_location = start_station_id WHERE issuer_source_location IS NULL;
ALTER TABLE game.contracts ALTER COLUMN issuer_source_location SET NOT NULL;
-- dao/contracts: paid listing deposit and creation-time auction tax snapshot.
ALTER TABLE game.contracts ADD COLUMN IF NOT EXISTS paid_deposit double precision NOT NULL DEFAULT 0.0;
ALTER TABLE game.contracts ADD COLUMN IF NOT EXISTS auction_tax_rate double precision NOT NULL DEFAULT 0.0;
ALTER TABLE game.contracts ADD COLUMN IF NOT EXISTS auction_tax_maximum double precision NOT NULL DEFAULT 0.0;
ALTER TABLE game.contract_bids ADD COLUMN IF NOT EXISTS wallet_key integer NOT NULL DEFAULT 1000;

-- dao/wars and dao/killmails/wars: durable mutual wars and immutable kill classification.
ALTER TABLE game.killmails ADD COLUMN IF NOT EXISTS victim_alliance_id bigint;
ALTER TABLE game.killmails ADD COLUMN IF NOT EXISTS final_alliance_id bigint;

CREATE TABLE IF NOT EXISTS game.wars (
    war_id bigserial PRIMARY KEY,
    declared_by bigint NOT NULL,
    against bigint NOT NULL,
    declared_at bigint NOT NULL,
    starts_at bigint NOT NULL,
    state text NOT NULL CHECK (state IN ('pending', 'active', 'cooldown', 'ended')),
    retracted_at bigint,
    retracted_by bigint,
    finishes_at bigint,
    CHECK (declared_by <> against),
    CHECK (starts_at > declared_at),
    CHECK ((state IN ('pending', 'active') AND retracted_at IS NULL AND retracted_by IS NULL AND finishes_at IS NULL)
        OR (state IN ('cooldown', 'ended') AND retracted_at IS NOT NULL AND retracted_by IS NOT NULL
            AND finishes_at IS NOT NULL AND retracted_by IN (declared_by, against) AND finishes_at > retracted_at))
);
CREATE UNIQUE INDEX IF NOT EXISTS wars_ongoing_pair_idx
    ON game.wars (LEAST(declared_by, against), GREATEST(declared_by, against))
    WHERE state <> 'ended';
CREATE INDEX IF NOT EXISTS wars_declared_by_idx ON game.wars (declared_by, war_id);
CREATE INDEX IF NOT EXISTS wars_against_idx ON game.wars (against, war_id);

CREATE TABLE IF NOT EXISTS game.war_invite_settings (
    owner_id bigint PRIMARY KEY,
    blocked boolean NOT NULL DEFAULT false
);
CREATE TABLE IF NOT EXISTS game.war_invites (
    from_owner bigint NOT NULL,
    to_owner bigint NOT NULL,
    sent_at bigint NOT NULL,
    expires_at bigint NOT NULL,
    PRIMARY KEY (from_owner, to_owner),
    CHECK (from_owner <> to_owner AND expires_at > sent_at)
);
CREATE INDEX IF NOT EXISTS war_invites_to_owner_idx ON game.war_invites (to_owner);

CREATE TABLE IF NOT EXISTS game.killmail_wars (
    kill_id bigint NOT NULL REFERENCES game.killmails (kill_id),
    war_id bigint NOT NULL REFERENCES game.wars (war_id),
    attacker_side bigint NOT NULL,
    defender_side bigint NOT NULL,
    PRIMARY KEY (kill_id, war_id),
    CHECK (attacker_side <> defender_side)
);
CREATE INDEX IF NOT EXISTS killmail_wars_war_idx ON game.killmail_wars (war_id, kill_id DESC);

-- Client alert and crash telemetry (dao/client_errors.rs).
CREATE TABLE IF NOT EXISTS game.client_errors (
    error_id bigserial PRIMARY KEY,
    stack_hash bigint NOT NULL UNIQUE,
    stack_key text NOT NULL,
    mode text NOT NULL,
    log_mode char(1) NOT NULL CHECK (log_mode IN ('B', 'D')),
    first_user_id bigint,
    first_char_id bigint,
    traceback text NOT NULL,
    first_seen timestamptz NOT NULL DEFAULT now(),
    last_seen timestamptz NOT NULL DEFAULT now(),
    occurrences bigint NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS game.client_error_reports (
    report_id bigserial PRIMARY KEY,
    error_id bigint NOT NULL REFERENCES game.client_errors,
    user_id bigint,
    char_id bigint,
    count bigint NOT NULL,
    reported_at timestamptz NOT NULL,
    arguments text
);
CREATE TABLE IF NOT EXISTS game.client_crashes (
    crash_id bigserial PRIMARY KEY,
    user_id bigint,
    char_id bigint,
    received_at timestamptz NOT NULL DEFAULT now(),
    crash text NOT NULL
);

-- Durable progress journal and consumer receipts (dao/progress).
CREATE TABLE IF NOT EXISTS game.progress_events (
    event_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_namespace text NOT NULL,
    source_key text NOT NULL,
    schema_version integer NOT NULL,
    kind text NOT NULL,
    occurred_at bigint NOT NULL,
    recorded_at bigint NOT NULL,
    payload jsonb NOT NULL,
    context jsonb NOT NULL,
    causation text,
    origin text NOT NULL,
    ordinal bigint NOT NULL,
    subject_character_id bigint,
    UNIQUE (source_namespace, source_key)
);
CREATE TABLE IF NOT EXISTS game.progress_subscriptions (
    consumer_key text PRIMARY KEY,
    generation bigint NOT NULL,
    activated_at bigint NOT NULL,
    version integer NOT NULL,
    enabled boolean NOT NULL
);
CREATE TABLE IF NOT EXISTS game.progress_subscription_kinds (
    consumer_key text NOT NULL REFERENCES game.progress_subscriptions,
    kind text NOT NULL,
    PRIMARY KEY (consumer_key, kind)
);
CREATE TABLE IF NOT EXISTS game.progress_deliveries (
    consumer_key text NOT NULL REFERENCES game.progress_subscriptions,
    event_id uuid NOT NULL REFERENCES game.progress_events,
    state text NOT NULL CHECK (state IN ('pending', 'applied')),
    applied_at bigint,
    next_attempt_at bigint NOT NULL,
    attempts integer NOT NULL DEFAULT 0,
    last_error text,
    subject_character_id bigint,
    PRIMARY KEY (consumer_key, event_id)
);
CREATE INDEX IF NOT EXISTS progress_deliveries_subject_pending
    ON game.progress_deliveries (consumer_key, subject_character_id, state, next_attempt_at, event_id);
CREATE INDEX IF NOT EXISTS progress_deliveries_due
    ON game.progress_deliveries (next_attempt_at, consumer_key, event_id) WHERE state = 'pending';
-- End of durable progress journal and consumer receipts (dao/progress).

-- Durable loss identity (dao/combat.rs).
ALTER TABLE game.killmails ADD COLUMN IF NOT EXISTS victim_item_id BIGINT;
CREATE UNIQUE INDEX IF NOT EXISTS killmails_victim_item_id_unique ON game.killmails(victim_item_id) WHERE victim_item_id IS NOT NULL;

-- Frozen daily-goal assignments and settlement receipts (dao/daily_goals).
CREATE TABLE IF NOT EXISTS game.daily_goal_periods (
    character_id bigint NOT NULL REFERENCES game.characters,
    kind text NOT NULL CHECK (kind IN ('day', 'month')),
    period_start bigint NOT NULL,
    period_end bigint NOT NULL CHECK (period_end > period_start),
    assigned_at bigint NOT NULL,
    generation bigint NOT NULL CHECK (generation >= 0),
    namespace text NOT NULL,
    version text NOT NULL,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    PRIMARY KEY (character_id, kind, period_start),
    UNIQUE (character_id, kind, period_start, period_end, namespace, version),
    FOREIGN KEY (namespace, version) REFERENCES game.lua_content_versions(namespace, version)
);
CREATE TABLE IF NOT EXISTS game.daily_goal_instances (
    goal_id uuid PRIMARY KEY,
    character_id bigint NOT NULL,
    kind text NOT NULL,
    period_start bigint NOT NULL,
    period_end bigint NOT NULL CHECK (period_end > period_start),
    slot_key text NOT NULL CHECK (slot_key <> ''),
    generation bigint NOT NULL CHECK (generation >= 0),
    namespace text NOT NULL,
    version text NOT NULL,
    category text NOT NULL CHECK (category IN ('ordinary', 'bonus', 'month')),
    definition jsonb NOT NULL,
    target numeric(20,0) NOT NULL CHECK (target BETWEEN 1 AND 18446744073709551615),
    progress numeric(20,0) NOT NULL DEFAULT 0 CHECK (progress BETWEEN 0 AND target),
    -- IEEE binary64's minimum quantum is 2^-1074; fractional coefficients are strictly below 2^-exponent.
    remainder_coefficient text NOT NULL DEFAULT '0' CHECK (length(remainder_coefficient) BETWEEN 1 AND 651 AND remainder_coefficient ~ '^(0|[1-9][0-9]*[13579]|[13579])$'),
    remainder_exponent smallint NOT NULL DEFAULT 0,
    completed_at bigint,
    earned_omega boolean NOT NULL DEFAULT false,
    paid boolean NOT NULL DEFAULT false,
    voided_at bigint,
    has_unclaimed boolean NOT NULL DEFAULT false,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    FOREIGN KEY (character_id, kind, period_start, period_end, namespace, version) REFERENCES game.daily_goal_periods(character_id, kind, period_start, period_end, namespace, version),
    FOREIGN KEY (namespace, version) REFERENCES game.lua_content_versions(namespace, version),
    UNIQUE (character_id, kind, period_start, slot_key, generation),
    CHECK ((remainder_coefficient = '0' AND remainder_exponent = 0) OR
           (remainder_coefficient <> '0' AND remainder_exponent BETWEEN -1074 AND -1
            AND remainder_coefficient::numeric < power(2::numeric, -remainder_exponent))),
    CHECK (progress <> target OR remainder_coefficient = '0'),
    CHECK ((completed_at IS NOT NULL) = (progress = target)),
    CHECK (completed_at IS NOT NULL OR (NOT earned_omega AND NOT paid AND NOT has_unclaimed))
);
CREATE INDEX IF NOT EXISTS daily_goal_instances_history ON game.daily_goal_instances
    (character_id, completed_at DESC, goal_id DESC) WHERE has_unclaimed AND completed_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS daily_goal_instances_period ON game.daily_goal_instances
    (character_id, kind, period_start, category, goal_id);
CREATE TABLE IF NOT EXISTS game.daily_goal_rewards (
    goal_id uuid NOT NULL REFERENCES game.daily_goal_instances,
    reward_index integer NOT NULL CHECK (reward_index >= 0),
    asset_id uuid NOT NULL UNIQUE,
    unit text NOT NULL CHECK (unit IN ('isk', 'lp', 'sp', 'item', 'plex')),
    amount_isk numeric(36,2),
    amount_units numeric,
    issuer_id bigint,
    type_id integer,
    omega_required boolean NOT NULL,
    earned boolean NOT NULL DEFAULT false,
    claimed_at bigint,
    location_id bigint,
    item_id bigint,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    PRIMARY KEY (goal_id, reward_index),
    CHECK (amount_isk IS NULL OR amount_isk > 0),
    CHECK (amount_units IS NULL OR (amount_units BETWEEN 1 AND 18446744073709551615 AND amount_units = trunc(amount_units))),
    CHECK (unit <> 'lp' OR amount_units <= 9223372036854775807),
    CHECK (unit <> 'sp' OR amount_units <= 4294967295),
    CHECK (unit <> 'item' OR amount_units <= 2147483647),
    -- Public PLEX balance is signed 64-bit hundredths.
    CHECK (unit <> 'plex' OR amount_units <= 92233720368547758),
    CHECK ((unit = 'isk' AND amount_isk IS NOT NULL AND amount_units IS NULL AND issuer_id IS NULL AND type_id IS NULL)
        OR (unit = 'lp' AND amount_isk IS NULL AND amount_units IS NOT NULL AND issuer_id IS NOT NULL AND type_id IS NULL)
        OR (unit IN ('sp', 'plex') AND amount_isk IS NULL AND amount_units IS NOT NULL AND issuer_id IS NULL AND type_id IS NULL)
        OR (unit = 'item' AND amount_isk IS NULL AND amount_units IS NOT NULL AND issuer_id IS NULL AND type_id IS NOT NULL)),
    CHECK (claimed_at IS NULL OR earned),
    CHECK ((location_id IS NULL AND item_id IS NULL) OR (unit = 'item' AND claimed_at IS NOT NULL AND location_id IS NOT NULL AND item_id IS NOT NULL))
);
ALTER TABLE game.daily_goal_rewards ADD COLUMN IF NOT EXISTS plex_reference bigint;
CREATE UNIQUE INDEX IF NOT EXISTS daily_goal_rewards_plex_reference_idx ON game.daily_goal_rewards (plex_reference) WHERE plex_reference IS NOT NULL;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.daily_goal_rewards'::regclass AND conname = 'daily_goal_rewards_plex_reference_shape') THEN
        ALTER TABLE game.daily_goal_rewards ADD CONSTRAINT daily_goal_rewards_plex_reference_shape
            CHECK (plex_reference IS NULL OR (plex_reference > 0 AND unit = 'plex' AND claimed_at IS NOT NULL));
    END IF;
END $$;
-- Daily rewards use protobuf Currency int64 units and two-decimal ISK.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.daily_goal_rewards'::regclass AND conname = 'daily_goal_rewards_isk_wire_bound') THEN
        ALTER TABLE game.daily_goal_rewards ADD CONSTRAINT daily_goal_rewards_isk_wire_bound
            CHECK (amount_isk IS NULL OR amount_isk <= 9223372036854775807.99);
    END IF;
END $$;
CREATE TABLE IF NOT EXISTS game.daily_goal_payments (
    goal_id uuid PRIMARY KEY REFERENCES game.daily_goal_instances,
    character_id bigint NOT NULL REFERENCES game.characters,
    day_start bigint NOT NULL,
    ordinal smallint NOT NULL CHECK (ordinal IN (1, 2)),
    issuer_id bigint NOT NULL,
    amount bigint NOT NULL CHECK (amount > 0),
    paid_at bigint NOT NULL,
    UNIQUE (character_id, day_start, ordinal)
);
CREATE TABLE IF NOT EXISTS game.daily_goal_admin_audit (
    audit_id uuid PRIMARY KEY,
    actor_id bigint NOT NULL REFERENCES game.characters,
    subject_id bigint NOT NULL REFERENCES game.characters,
    goal_id uuid NOT NULL REFERENCES game.daily_goal_instances,
    operation text NOT NULL,
    payload jsonb NOT NULL,
    before_state jsonb NOT NULL,
    after_state jsonb NOT NULL,
    recorded_at bigint NOT NULL
);

ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS escrow numeric(36,2) NOT NULL DEFAULT 0;

-- dao/market: reject unrepresentable legacy amounts before numeric typmods can round them. The check and conversion run only while a column is not yet numeric(36,2).
DO $$
DECLARE
    col record;
    invalid boolean;
BEGIN
    FOR col IN SELECT * FROM (VALUES
        ('market_orders', 'price'), ('market_orders', 'escrow'),
        ('market_order_history', 'price'), ('market_order_history', 'escrow'),
        ('market_transactions', 'price')
    ) AS columns(table_name, column_name)
    LOOP
        IF EXISTS (SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'game' AND table_name = col.table_name
                     AND column_name = col.column_name
                     AND (data_type <> 'numeric' OR numeric_precision IS DISTINCT FROM 36 OR numeric_scale IS DISTINCT FROM 2)) THEN
            EXECUTE format(
                'SELECT EXISTS (SELECT 1 FROM game.%I WHERE %I::text IN (''NaN'', ''Infinity'', ''-Infinity'')
                  OR CASE WHEN %I::text NOT IN (''NaN'', ''Infinity'', ''-Infinity'') THEN
                    %I::text::numeric < 0 OR %I::text::numeric >= 1e34::numeric
                    OR %I::text::numeric <> trunc(%I::text::numeric, 2)
                  ELSE false END)',
                col.table_name, col.column_name, col.column_name, col.column_name,
                col.column_name, col.column_name, col.column_name) INTO invalid;
            IF invalid THEN
                RAISE EXCEPTION 'invalid legacy market amount %.%', col.table_name, col.column_name;
            END IF;
            EXECUTE format('ALTER TABLE game.%I ALTER COLUMN %I TYPE numeric(36,2) USING %I::text::numeric',
                           col.table_name, col.column_name, col.column_name);
        END IF;
    END LOOP;
END $$;

ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS market_scope smallint NOT NULL DEFAULT 0;
ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS vault_user_id bigint;
ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS plex_escrow bigint NOT NULL DEFAULT 0;
ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS created_at bigint;
ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS expires_at bigint;
ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS market_scope smallint NOT NULL DEFAULT 0;
ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS vault_user_id bigint;
ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS plex_escrow bigint NOT NULL DEFAULT 0;
ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS created_at bigint;
ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS expires_at bigint;
ALTER TABLE game.market_order_history ADD COLUMN IF NOT EXISTS close_reason text;
ALTER TABLE game.market_transactions ADD COLUMN IF NOT EXISTS market_scope smallint NOT NULL DEFAULT 0;
ALTER TABLE game.market_transactions ADD COLUMN IF NOT EXISTS buy_order_id bigint;
ALTER TABLE game.market_transactions ADD COLUMN IF NOT EXISTS sell_order_id bigint;
ALTER TABLE game.market_transactions ADD COLUMN IF NOT EXISTS buyer_vault_user_id bigint;
ALTER TABLE game.market_transactions ADD COLUMN IF NOT EXISTS seller_vault_user_id bigint;
-- dao/items, market, contracts, store and fitting: widen existing quantities once.
DO $$
DECLARE
    col record;
    tab text;
    quantity_column text;
    quantity_type text;
BEGIN
    FOR col IN SELECT * FROM (VALUES
        ('items', 'quantity'), ('market_transactions', 'quantity'),
        ('contract_items', 'quantity'), ('store_purchases', 'quantity'),
        ('redeem_tokens', 'quantity'), ('module_charges', 'quantity'),
        ('pending_reloads', 'quantity')
    ) AS columns(table_name, column_name)
    LOOP
        IF EXISTS (SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'game' AND table_name = col.table_name
                     AND column_name = col.column_name AND data_type = 'integer') THEN
            EXECUTE format('ALTER TABLE game.%I ALTER COLUMN %I TYPE bigint', col.table_name, col.column_name);
        ELSIF NOT EXISTS (SELECT 1 FROM information_schema.columns
                          WHERE table_schema = 'game' AND table_name = col.table_name
                            AND column_name = col.column_name AND data_type = 'bigint') THEN
            RAISE EXCEPTION 'unsupported quantity column game.%.%', col.table_name, col.column_name;
        END IF;
    END LOOP;
    FOREACH tab IN ARRAY ARRAY['market_orders', 'market_order_history'] LOOP
        FOREACH quantity_column IN ARRAY ARRAY['vol_entered', 'vol_remaining', 'min_volume', 'plex_escrow'] LOOP
            SELECT data_type INTO quantity_type FROM information_schema.columns
            WHERE table_schema = 'game' AND table_name = tab AND column_name = quantity_column;
            IF quantity_type IS NULL OR quantity_type NOT IN ('integer', 'bigint') THEN
                RAISE EXCEPTION 'unsupported quantity column game.%.%', tab, quantity_column;
            END IF;
        END LOOP;
        IF EXISTS (SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'game' AND table_name = tab
                     AND column_name IN ('vol_entered', 'vol_remaining', 'min_volume', 'plex_escrow')
                     AND data_type = 'integer') THEN
            EXECUTE format('ALTER TABLE game.%I ALTER COLUMN vol_entered TYPE bigint, ALTER COLUMN vol_remaining TYPE bigint, ALTER COLUMN min_volume TYPE bigint, ALTER COLUMN plex_escrow TYPE bigint', tab);
        END IF;
    END LOOP;
END $$;

CREATE TABLE IF NOT EXISTS game.market_book_versions (
    market_scope smallint NOT NULL,
    type_id integer NOT NULL,
    region_key bigint NOT NULL,
    revision bigint NOT NULL,
    changed_at bigint NOT NULL,
    PRIMARY KEY (market_scope, type_id, region_key)
);
CREATE TABLE IF NOT EXISTS game.market_legacy_npc_backup (
    order_id bigint PRIMARY KEY,
    row_data jsonb NOT NULL
);

-- Completion marker written by dao/market/migration after the data cutover.
CREATE TABLE IF NOT EXISTS game.market_cutover_complete (
    singleton boolean PRIMARY KEY CHECK (singleton)
);
CREATE INDEX IF NOT EXISTS market_orders_scope_book_idx ON game.market_orders (market_scope, type_id, region_id, bid, price, order_id);
CREATE INDEX IF NOT EXISTS market_orders_scope_expiry_idx ON game.market_orders (market_scope, type_id, expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS market_transactions_scope_history_idx ON game.market_transactions (market_scope, type_id, date);
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.market_orders'::regclass AND conname = 'market_orders_scope_check') THEN
        ALTER TABLE game.market_orders ADD CONSTRAINT market_orders_scope_check CHECK (
            (market_scope = 0 AND vault_user_id IS NULL AND plex_escrow = 0)
            OR (market_scope = 1 AND char_id IS NOT NULL AND created_at IS NOT NULL AND expires_at IS NOT NULL AND expires_at > created_at
                AND vol_entered > 0 AND vol_remaining > 0 AND vol_remaining <= vol_entered
                AND min_volume > 0 AND min_volume <= vol_entered AND price > 0
                AND ((corporation_id IS NULL AND vault_user_id IS NOT NULL)
                     OR (corporation_id IS NOT NULL AND vault_user_id IS NULL))
                AND ((bid AND plex_escrow = 0 AND escrow = price * vol_remaining)
                     OR (NOT bid AND plex_escrow = vol_remaining AND escrow = 0)))
        );
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.market_order_history'::regclass AND conname = 'market_order_history_scope_check') THEN
        ALTER TABLE game.market_order_history ADD CONSTRAINT market_order_history_scope_check CHECK (market_scope IN (0, 1));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'game.market_transactions'::regclass AND conname = 'market_transactions_scope_check') THEN
        ALTER TABLE game.market_transactions ADD CONSTRAINT market_transactions_scope_check CHECK (market_scope IN (0, 1));
    END IF;
END $$;
