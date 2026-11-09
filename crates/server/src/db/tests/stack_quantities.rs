//! Schema replay coverage for inventory quantity widening.

use super::*;

const COLUMNS: &[(&str, &str)] = &[
    ("items", "quantity"), ("market_orders", "vol_entered"),
    ("market_orders", "vol_remaining"), ("market_orders", "min_volume"),
    ("market_orders", "plex_escrow"), ("market_order_history", "vol_entered"),
    ("market_order_history", "vol_remaining"), ("market_order_history", "min_volume"),
    ("market_order_history", "plex_escrow"), ("market_transactions", "quantity"),
    ("contract_items", "quantity"), ("store_purchases", "quantity"),
    ("redeem_tokens", "quantity"), ("module_charges", "quantity"),
    ("pending_reloads", "quantity"),
];

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database; run this test alone"]
async fn quantity_schema_replays_fresh_and_legacy() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let client = db.pool.get().await.unwrap();
    for legacy in [0, 1, 2] {
        client.batch_execute("BEGIN; DROP SCHEMA game CASCADE;").await.unwrap();
        client.batch_execute(GAME_SCHEMA).await.unwrap();
        if legacy != 0 {
            client.batch_execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES (1, 1, 1, 1, -2); INSERT INTO game.fittings (owner_id, ship_type_id, fit_types, fit_flags, fit_qty) VALUES (1, 1, ARRAY[1], ARRAY[2,3], '[0:2]={0,-2,2147483647}'), (3, 1, '{}', '{}', '[2:3][4:5]={{1,NULL},{-1,0}}'); INSERT INTO game.fittings (owner_id, ship_type_id) VALUES (2, 1); INSERT INTO game.market_cutover_complete (singleton) VALUES (true);").await.unwrap();
            for &(table, column) in COLUMNS {
                client.batch_execute(&format!("ALTER TABLE game.{table} ALTER COLUMN {column} TYPE integer USING {column}::integer")).await.unwrap();
            }
            client.batch_execute("ALTER TABLE game.fittings ALTER COLUMN fit_qty TYPE integer[] USING fit_qty::integer[];").await.unwrap();
            if legacy == 1 {
                client.batch_execute("INSERT INTO game.market_orders (type_id, char_id, station_id, solar_system_id, constellation_id, region_id, price, vol_entered, vol_remaining, min_volume, bid, duration, market_scope, vault_user_id, plex_escrow, created_at, expires_at) VALUES (1, 1, 1, 1, 1, 1, 1, 10, 7, 1, false, 1, 1, 1, 7, 1, 2); INSERT INTO game.market_order_history (order_id, type_id, char_id, station_id, region_id, price, vol_entered, vol_remaining, min_volume, bid, range, duration, issued, state, closed, market_scope, vault_user_id, plex_escrow) VALUES (1, 1, 1, 1, 1, 1, 10, 7, 1, false, -1, 1, 1, 1, 1, 1, 1, 7);").await.unwrap();
            }
            if legacy == 2 {
                client.batch_execute("ALTER TABLE game.market_orders DROP CONSTRAINT market_orders_scope_check, DROP COLUMN plex_escrow; ALTER TABLE game.market_order_history DROP CONSTRAINT market_order_history_scope_check, DROP COLUMN plex_escrow;").await.unwrap();
            }
        } else {
            client.batch_execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES (1, 1, 1, 1, 9007199254740993);").await.unwrap();
        }
        for _ in 0..2 { client.batch_execute(GAME_SCHEMA).await.unwrap(); }
        for &(table, column) in COLUMNS {
            let row = client.query_one("SELECT udt_name, is_nullable FROM information_schema.columns WHERE table_schema = 'game' AND table_name = $1 AND column_name = $2", &[&table, &column]).await.unwrap();
            assert_eq!(row.get::<_, String>("udt_name"), "int8", "{table}.{column}");
            assert_eq!(row.get::<_, String>("is_nullable"), "NO");
        }
        for (column, expected) in [("fit_qty", "_int8"), ("fit_types", "_int4"), ("fit_flags", "_int4")] {
            let row = client.query_one("SELECT udt_name, is_nullable, column_default FROM information_schema.columns WHERE table_schema = 'game' AND table_name = 'fittings' AND column_name = $1", &[&column]).await.unwrap();
            assert_eq!(row.get::<_, String>("udt_name"), expected);
            assert_eq!(row.get::<_, String>("is_nullable"), "NO");
            assert!(row.get::<_, Option<String>>("column_default").unwrap().contains("{}"));
        }
        for table in ["market_orders", "market_order_history"] {
            let constraint = format!("{table}_scope_check");
            assert!(client.query_opt("SELECT 1 FROM pg_constraint WHERE conrelid = format('game.%I', $1::text)::regclass AND conname = $2", &[&table, &constraint]).await.unwrap().is_some());
            let row = client.query_one("SELECT column_default FROM information_schema.columns WHERE table_schema = 'game' AND table_name = $1 AND column_name = 'plex_escrow'", &[&table]).await.unwrap();
            assert_eq!(row.get::<_, Option<String>>("column_default").as_deref(), Some("0"));
        }
        if legacy == 1 {
            for table in ["market_orders", "market_order_history"] {
                let row = client.query_one(&format!("SELECT market_scope, plex_escrow, vol_remaining FROM game.{table}"), &[]).await.unwrap();
                assert_eq!(row.get::<_, i16>("market_scope"), 1);
                assert_eq!(row.get::<_, i64>("plex_escrow"), 7);
                assert_eq!(row.get::<_, i64>("vol_remaining"), 7);
            }
        }
        if legacy != 0 {
            let row = client.query_one("SELECT quantity FROM game.items WHERE owner_id = 1", &[]).await.unwrap();
            assert_eq!(row.get::<_, i64>("quantity"), -2);
            let row = client.query_one("SELECT fit_qty::text AS qty, fit_types::text AS types, fit_flags::text AS flags FROM game.fittings WHERE owner_id = 1", &[]).await.unwrap();
            assert_eq!(row.get::<_, String>("qty"), "[0:2]={0,-2,2147483647}");
            assert_eq!(row.get::<_, String>("types"), "{1}");
            assert_eq!(row.get::<_, String>("flags"), "{2,3}");
            let row = client.query_one("SELECT cardinality(fit_qty) AS n FROM game.fittings WHERE owner_id = 2", &[]).await.unwrap();
            assert_eq!(row.get::<_, i32>("n"), 0);
            let row = client.query_one("SELECT fit_qty::text AS qty FROM game.fittings WHERE owner_id = 3", &[]).await.unwrap();
            assert_eq!(row.get::<_, String>("qty"), "[2:3][4:5]={{1,NULL},{-1,0}}");
            assert!(client.query_opt("SELECT singleton FROM game.market_cutover_complete WHERE singleton", &[]).await.unwrap().is_some());
        } else {
            let row = client.query_one("SELECT quantity FROM game.items WHERE owner_id = 1", &[]).await.unwrap();
            assert_eq!(row.get::<_, i64>("quantity"), 9007199254740993);
        }
        client.batch_execute("ROLLBACK").await.unwrap();
    }
    for (alter, column) in [
        ("ALTER TABLE game.items ALTER COLUMN quantity TYPE numeric", "game.items.quantity"),
        ("ALTER TABLE game.market_orders ALTER COLUMN vol_entered TYPE integer, ALTER COLUMN min_volume TYPE numeric", "game.market_orders.min_volume"),
    ] {
        client.batch_execute("BEGIN; DROP SCHEMA game CASCADE;").await.unwrap();
        client.batch_execute(GAME_SCHEMA).await.unwrap();
        client.batch_execute(alter).await.unwrap();
        client.batch_execute("SAVEPOINT before_replay").await.unwrap();
        let error = client.batch_execute(GAME_SCHEMA).await.unwrap_err();
        assert!(error.as_db_error().unwrap().message().contains(column));
        client.batch_execute("ROLLBACK TO SAVEPOINT before_replay; ROLLBACK").await.unwrap();
    }
}
