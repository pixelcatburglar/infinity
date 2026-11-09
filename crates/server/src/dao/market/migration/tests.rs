//! Migration tests use an initialized, dedicated database and roll back every fixture.
use super::*;
use tokio_postgres::{Client, NoTls, Transaction};

const SCHEMA: &str = include_str!("../../../../sql/game_schema.sql");

async fn connect() -> Client {
    let config = crate::config::tests::test_database();
    let (client, connection) = tokio_postgres::connect(&config.url, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    client
}

struct Fixture {
    user: i64,
    character: i64,
    station: i64,
    system: i64,
    constellation: i64,
    region: i64,
}

async fn fixture(tx: &Transaction<'_>) -> Fixture {
    // Isolate the cutover from pre-existing books, while leaving all data intact on rollback.
    tx.batch_execute("DELETE FROM game.market_cutover_complete; DELETE FROM game.market_orders; DELETE FROM game.market_book_versions; DELETE FROM game.market_legacy_npc_backup").await.unwrap();
    let station = tx.query_one("SELECT station_id::bigint AS station, solar_system_id::bigint AS system, constellation_id::bigint AS constellation, region_id::bigint AS region FROM sde.sta_stations WHERE solar_system_id IS NOT NULL AND constellation_id IS NOT NULL AND region_id IS NOT NULL ORDER BY station_id LIMIT 1", &[]).await.unwrap();
    let station_id: i64 = station.get("station");
    let user = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role,plex_balance) VALUES ('market-migration-' || nextval('game.accounts_user_id_seq')::text,$1,0,123) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,balance) VALUES ($1,'market-migration-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,456.78) RETURNING character_id", &[&user, &station_id]).await.unwrap().get("character_id");
    Fixture { user, character, station: station_id, system: station.get("system"), constellation: station.get("constellation"), region: station.get("region") }
}

async fn order(tx: &Transaction<'_>, f: &Fixture, character: Option<i64>, bid: bool, escrow: &str, corporation: Option<i64>, source: Option<i64>, flag: Option<i32>) -> i64 {
    tx.query_one("INSERT INTO game.market_orders (type_id,char_id,station_id,solar_system_id,constellation_id,region_id,price,vol_entered,vol_remaining,min_volume,bid,duration,issued,escrow,corporation_id,source_location_id,source_flag_id) VALUES ($1,$2,$3,$4,$5,$6,12.50,10,4,1,$7,30,100000000000000000,$8::text::numeric,$9,$10,$11) RETURNING order_id", &[&TYPE_PLEX, &character, &f.station, &f.system, &f.constellation, &f.region, &bid, &escrow, &corporation, &source, &flag]).await.unwrap().get("order_id")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with initialized SDE; run serially"]
async fn legacy_personal_and_corporate_principal_is_conserved() {
    let mut client = connect().await;
    let tx = client.transaction().await.unwrap();
    let f = fixture(&tx).await;
    let corp: i64 = tx.query_one("INSERT INTO game.corporations (name,ticker,creator_id,station_id,created) VALUES ('market-migration-' || nextval('game.corporations_corporation_id_seq')::text, 'T' || nextval('game.corporations_corporation_id_seq')::text,$1,$2,1) RETURNING corporation_id", &[&f.character, &f.station]).await.unwrap().get("corporation_id");
    let buy = order(&tx, &f, Some(f.character), true, "50.00", None, None, None).await;
    let sell = order(&tx, &f, Some(f.character), false, "0", None, None, None).await;
    let source = 987654321_i64;
    let flag = FLAG_CORP_SAG1 as i32;
    let corporate = order(&tx, &f, Some(f.character), false, "0", Some(corp), Some(source), Some(flag)).await;
    let before: String = tx.query_one("SELECT (c.balance::text || ':' || a.plex_balance::text) AS totals FROM game.characters c JOIN game.accounts a ON a.user_id=c.user_id WHERE c.character_id=$1", &[&f.character]).await.unwrap().get("totals");
    migrate(&tx).await.unwrap();
    for (id, bid, owner, plex) in [(buy, true, Some(f.user), 0_i64), (sell, false, Some(f.user), 4), (corporate, false, None, 4)] {
        let r = tx.query_one("SELECT market_scope,vault_user_id,plex_escrow,vol_entered,vol_remaining,price::text AS price,escrow::text AS escrow,station_id,solar_system_id,constellation_id,region_id,created_at,expires_at,corporation_id,source_location_id,source_flag_id FROM game.market_orders WHERE order_id=$1", &[&id]).await.unwrap();
        assert_eq!(r.get::<_, i16>("market_scope"), 1);
        assert_eq!(r.get::<_, Option<i64>>("vault_user_id"), owner);
        assert_eq!(r.get::<_, i64>("plex_escrow"), plex);
        assert_eq!((r.get::<_, i64>("vol_entered"), r.get::<_, i64>("vol_remaining")), (10, 4));
        assert_eq!(r.get::<_, String>("price"), "12.50");
        assert_eq!(r.get::<_, String>("escrow"), if bid { "50.00" } else { "0.00" });
        assert_eq!((r.get::<_, i64>("station_id"), r.get::<_, i64>("solar_system_id"), r.get::<_, i64>("constellation_id"), r.get::<_, i64>("region_id")), (f.station, f.system, f.constellation, f.region));
        assert_eq!(r.get::<_, Option<i64>>("created_at"), Some(100000000000000000));
        assert_eq!(r.get::<_, Option<i64>>("expires_at"), Some(100000000000000000 + 30 * FILETIME_DAY));
        if id == corporate {
            assert_eq!(r.get::<_, Option<i64>>("corporation_id"), Some(corp));
            assert_eq!(r.get::<_, Option<i64>>("source_location_id"), Some(source));
            assert_eq!(r.get::<_, Option<i32>>("source_flag_id"), Some(flag));
        }
    }
    let after: String = tx.query_one("SELECT (c.balance::text || ':' || a.plex_balance::text) AS totals FROM game.characters c JOIN game.accounts a ON a.user_id=c.user_id WHERE c.character_id=$1", &[&f.character]).await.unwrap().get("totals");
    assert_eq!(before, after);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with initialized SDE; run serially"]
async fn invalid_legacy_rows_refuse_cutover_and_rollback() {
    let mut client = connect().await;
    let mut tx = client.transaction().await.unwrap();
    let f = fixture(&tx).await;
    let id = order(&tx, &f, Some(f.character), true, "50.00", None, None, None).await;
    for mutation in [
        "UPDATE game.market_orders SET escrow=49 WHERE order_id=$1",
        "UPDATE game.market_orders SET vol_remaining=11 WHERE order_id=$1",
        "UPDATE game.market_orders SET char_id=-1 WHERE order_id=$1",
        "UPDATE game.market_orders SET station_id=-1 WHERE order_id=$1",
        "UPDATE game.market_orders SET solar_system_id=-1 WHERE order_id=$1",
        "UPDATE game.market_orders SET corporation_id=-1 WHERE order_id=$1",
    ] {
        let save = tx.savepoint("invalid_order").await.unwrap();
        save.execute(mutation, &[&id]).await.unwrap();
        let before: String = save.query_one("SELECT to_jsonb(o)::text AS row FROM game.market_orders o WHERE order_id=$1", &[&id]).await.unwrap().get("row");
        assert!(migrate(&save).await.is_err(), "accepted {mutation}");
        assert_eq!(save.query_one("SELECT to_jsonb(o)::text AS row FROM game.market_orders o WHERE order_id=$1", &[&id]).await.unwrap().get::<_, String>("row"), before);
        assert_eq!(save.query_one("SELECT count(*) AS n FROM game.market_cutover_complete", &[]).await.unwrap().get::<_, i64>("n"), 0);
        save.rollback().await.unwrap();
    }
    let corp: i64 = tx.query_one("INSERT INTO game.corporations (name,ticker,creator_id,station_id,created) VALUES ('market-invalid-' || nextval('game.corporations_corporation_id_seq')::text, 'I' || nextval('game.corporations_corporation_id_seq')::text,$1,$2,1) RETURNING corporation_id", &[&f.character, &f.station]).await.unwrap().get("corporation_id");
    let sell = order(&tx, &f, Some(f.character), false, "0", Some(corp), Some(123), Some(FLAG_CORP_SAG1)).await;
    let save = tx.savepoint("invalid_source").await.unwrap();
    save.execute("UPDATE game.market_orders SET source_flag_id=0 WHERE order_id=$1", &[&sell]).await.unwrap();
    let before: String = save.query_one("SELECT to_jsonb(o)::text AS row FROM game.market_orders o WHERE order_id=$1", &[&sell]).await.unwrap().get("row");
    assert!(migrate(&save).await.is_err());
    assert_eq!(save.query_one("SELECT to_jsonb(o)::text AS row FROM game.market_orders o WHERE order_id=$1", &[&sell]).await.unwrap().get::<_, String>("row"), before);
    save.rollback().await.unwrap();
    assert_eq!(tx.query_one("SELECT escrow::text AS escrow FROM game.market_orders WHERE order_id=$1", &[&id]).await.unwrap().get::<_, String>("escrow"), "50.00");
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with initialized SDE; run serially"]
async fn npc_backup_marker_validation_and_repeat_are_stable() {
    let mut client = connect().await;
    let mut tx = client.transaction().await.unwrap();
    let f = fixture(&tx).await;
    let id = order(&tx, &f, None, false, "0", None, None, None).await;
    let error = validate(&tx).await.unwrap_err().to_string();
    assert!(error.contains("schema_on_start") && error.contains("game_schema.sql"), "{error}");
    {
        let save = tx.savepoint("missing_marker_table").await.unwrap();
        save.batch_execute("DROP TABLE game.market_cutover_complete").await.unwrap();
        let error = validate(&save).await.unwrap_err().to_string();
        assert!(error.contains("schema_on_start") && error.contains("game_schema.sql"), "{error}");
        save.rollback().await.unwrap();
    }
    migrate(&tx).await.unwrap();
    validate(&tx).await.unwrap();
    let backup: String = tx.query_one("SELECT row_data::text AS row FROM game.market_legacy_npc_backup WHERE order_id=$1", &[&id]).await.unwrap().get("row");
    assert!(backup.contains(&id.to_string()));
    assert!(tx.query_opt("SELECT order_id FROM game.market_orders WHERE order_id=$1", &[&id]).await.unwrap().is_none());
    let revisions: String = tx.query_one("SELECT COALESCE(jsonb_agg(to_jsonb(v) ORDER BY market_scope,type_id,region_key)::text,'[]') AS rows FROM game.market_book_versions v", &[]).await.unwrap().get("rows");
    // A newly inserted legacy row proves the marker short-circuits, rather than merely making an empty migration harmless.
    let later = order(&tx, &f, None, false, "0", None, None, None).await;
    migrate(&tx).await.unwrap();
    assert!(tx.query_opt("SELECT order_id FROM game.market_orders WHERE order_id=$1", &[&later]).await.unwrap().is_some());
    assert_eq!(tx.query_one("SELECT count(*) AS n FROM game.market_cutover_complete", &[]).await.unwrap().get::<_, i64>("n"), 1);
    assert_eq!(tx.query_one("SELECT count(*) AS n FROM game.market_legacy_npc_backup", &[]).await.unwrap().get::<_, i64>("n"), 1);
    assert_eq!(tx.query_one("SELECT row_data::text AS row FROM game.market_legacy_npc_backup WHERE order_id=$1", &[&id]).await.unwrap().get::<_, String>("row"), backup);
    assert_eq!(tx.query_one("SELECT COALESCE(jsonb_agg(to_jsonb(v) ORDER BY market_scope,type_id,region_key)::text,'[]') AS rows FROM game.market_book_versions v", &[]).await.unwrap().get::<_, String>("rows"), revisions);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with initialized SDE; run serially"]
async fn schema_preflight_rejects_bad_money_without_partial_conversion() {
    let mut client = connect().await;
    let mut tx = client.transaction().await.unwrap();
    let f = fixture(&tx).await;
    let id = order(&tx, &f, None, false, "0", None, None, None).await;
    for value in ["NaN", "Infinity", "-Infinity", "12.345"] {
        let save = tx.savepoint("bad_money").await.unwrap();
        save.batch_execute("ALTER TABLE game.market_orders ALTER COLUMN price TYPE double precision USING price::double precision").await.unwrap();
        save.execute("UPDATE game.market_orders SET price=$2::text::double precision WHERE order_id=$1", &[&id, &value]).await.unwrap();
        assert!(save.batch_execute(SCHEMA).await.is_err(), "accepted {value}");
        save.rollback().await.unwrap();
        let row = tx.query_one("SELECT price::text AS price FROM game.market_orders WHERE order_id=$1", &[&id]).await.unwrap();
        assert_eq!(row.get::<_, String>("price"), "12.50");
        let ty: String = tx.query_one("SELECT format_type(atttypid,atttypmod) AS ty FROM pg_attribute WHERE attrelid='game.market_orders'::regclass AND attname='price'", &[]).await.unwrap().get("ty");
        assert_eq!(ty, "numeric(36,2)");
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with initialized SDE; run serially"]
async fn fresh_and_repeated_schema_and_cutover_are_safe() {
    let mut client = connect().await;
    let tx = client.transaction().await.unwrap();
    fixture(&tx).await;
    tx.batch_execute(SCHEMA).await.unwrap();
    migrate(&tx).await.unwrap();
    validate(&tx).await.unwrap();
    let before: String = tx.query_one("SELECT COALESCE(jsonb_agg(to_jsonb(v) ORDER BY market_scope,type_id,region_key)::text,'[]') AS rows FROM game.market_book_versions v", &[]).await.unwrap().get("rows");
    tx.batch_execute(SCHEMA).await.unwrap();
    migrate(&tx).await.unwrap();
    validate(&tx).await.unwrap();
    assert_eq!(tx.query_one("SELECT count(*) AS n FROM game.market_cutover_complete", &[]).await.unwrap().get::<_, i64>("n"), 1);
    assert_eq!(tx.query_one("SELECT COALESCE(jsonb_agg(to_jsonb(v) ORDER BY market_scope,type_id,region_key)::text,'[]') AS rows FROM game.market_book_versions v", &[]).await.unwrap().get::<_, String>("rows"), before);
    tx.rollback().await.unwrap();
}
