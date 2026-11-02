-- Mutable game state (accounts, characters, items...). Idempotent; applied by
-- the server at startup. Static data lives in schema `sde` (sde_typed.sql).

CREATE SCHEMA IF NOT EXISTS game;
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
SELECT setval('game.accounts_user_id_seq', GREATEST((SELECT max(user_id) FROM game.accounts), 10000000), true);

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
    quantity    integer NOT NULL DEFAULT 1,
    singleton   boolean NOT NULL DEFAULT false,
    custom_info text,
    name        text
);
CREATE INDEX IF NOT EXISTS items_location_idx ON game.items (location_id, flag_id);
CREATE INDEX IF NOT EXISTS items_owner_idx ON game.items (owner_id);
-- dao/items/brain: durable augmentation metadata on installed item rows.
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS brain_slot integer;
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS brain_activated_at bigint;
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS brain_expires_at bigint;
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS brain_effect_ids integer[];
CREATE UNIQUE INDEX IF NOT EXISTS items_brain_slot_idx ON game.items (owner_id, flag_id, brain_slot)
    WHERE location_id = owner_id AND brain_slot IS NOT NULL;
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
SELECT setval('game.items_item_id_seq', GREATEST((SELECT max(item_id) FROM game.items), 1000000000000), true);

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
SELECT setval('game.characters_character_id_seq', GREATEST((SELECT max(character_id) FROM game.characters), 90000000), true);

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
    vol_entered      integer NOT NULL,
    vol_remaining    integer NOT NULL,
    min_volume       integer NOT NULL DEFAULT 1,
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
    vol_entered   integer NOT NULL,
    vol_remaining integer NOT NULL,
    min_volume    integer NOT NULL,
    bid           boolean NOT NULL,
    range         integer NOT NULL,
    duration      integer NOT NULL,
    issued        bigint NOT NULL,  -- FILETIME
    state         integer NOT NULL,
    closed        bigint NOT NULL   -- FILETIME
);
CREATE INDEX IF NOT EXISTS market_order_history_char_idx ON game.market_order_history (char_id, closed);

-- Every fill. An NPC side is the station's owner. Read back as each side's wallet
-- transactions and as the regional price history.
CREATE TABLE IF NOT EXISTS game.market_transactions (
    transaction_id bigserial PRIMARY KEY,
    type_id        integer NOT NULL,
    quantity       integer NOT NULL,
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
    quantity    integer NOT NULL,
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
SELECT setval('game.corporations_corporation_id_seq', GREATEST((SELECT max(corporation_id) FROM game.corporations), 98000000), true);

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
SELECT setval('game.alliances_alliance_id_seq', GREATEST((SELECT max(alliance_id) FROM game.alliances), 99000000), true);
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
-- services/clones.rs: retain the oldest clone per station; duplicate names are discarded.
-- Implants are not modelled, so there is no implant data to transfer.
DELETE FROM game.jump_clones AS duplicate
USING game.jump_clones AS retained
WHERE duplicate.owner_id = retained.owner_id
  AND duplicate.location_id = retained.location_id
  AND duplicate.clone_id > retained.clone_id;
CREATE UNIQUE INDEX IF NOT EXISTS jump_clones_owner_location_idx ON game.jump_clones (owner_id, location_id);
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

-- Redeeming queue: items owed to an account (store purchases, login rewards),
-- turned into items when claimed onto a character.
CREATE TABLE IF NOT EXISTS game.redeem_tokens (
    token_id    bigserial PRIMARY KEY,
    user_id     bigint NOT NULL REFERENCES game.accounts,
    type_id     integer NOT NULL,
    quantity    integer NOT NULL,
    label       text NOT NULL DEFAULT '',
    description text NOT NULL DEFAULT '',
    created     bigint NOT NULL  -- FILETIME
);
CREATE INDEX IF NOT EXISTS redeem_tokens_user_idx ON game.redeem_tokens (user_id);

-- Store purchases, for single-purchase and once-per-period offers.
CREATE TABLE IF NOT EXISTS game.store_purchases (
    user_id      bigint NOT NULL REFERENCES game.accounts,
    offer_id     integer NOT NULL,
    quantity     integer NOT NULL,
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
    fit_qty      integer[] NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS fittings_owner_idx ON game.fittings (owner_id);

-- Charges loaded in fitted modules. The client keys them as the sublocation
-- (shipID, flagID, typeID), not as inventory items (services/combat.rs).
CREATE TABLE IF NOT EXISTS game.module_charges (
    ship_id  bigint NOT NULL,
    flag_id  integer NOT NULL,
    type_id  integer NOT NULL,
    quantity integer NOT NULL,
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
    quantity  integer NOT NULL CHECK (quantity > 0)
);

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
-- Preserve synthetic container IDs across the earlier item-sequence reset.
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
