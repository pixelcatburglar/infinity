//! Database regressions for market book snapshots, guards and price history.
use super::*;
use crate::{dao::market::{listings::{self, Area}, orders::{self, CloseReason, MarketScope, Order}, trading::{self, NewTransaction}, versions}, isk::IskAmount, services::tests::{client, fixture}};

fn money(value: &str) -> IskAmount { IskAmount::parse_decimal(value).unwrap() }

async fn order(db: &Db, person: Fixture, at: Station, ty: i32, bid: bool, price: &str, issued: i64, expires: Option<i64>) -> Order {
    Order { id: orders::next_order_id(db).await.unwrap(), type_id: ty, char_id: Some(person.character), at,
        corporation_id: None, wallet_key: 1000, source_location_id: None, source_flag_id: None,
        price: money(price), vol_entered: 10, vol_remaining: 10, min_volume: 1, bid, range: -1,
        duration: 30, issued: Some(issued), escrow: if ty == TYPE_PLEX && bid { money(price).checked_mul(10).unwrap() } else { IskAmount::ZERO }, scope: MarketScope::for_type(ty),
        vault_user_id: if ty == TYPE_PLEX { Some(person.account) } else { None }, plex_escrow: if ty == TYPE_PLEX && !bid { 10 } else { 0 },
        created_at: Some(issued), expires_at: expires }
}

async fn snap(db: &Db, ty: i32, region: i64, now: i64) -> versions::BookSnapshot {
    versions::snapshot(db, MarketScope::for_type(ty), ty, region, now, FILETIME_DAY).await.unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn global_snapshot_priority_expiry_and_reconnect() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, b) = isolated_stations(&db, &probe.sde).await;
    let person = fixture::create(&db).await.unwrap();
    let work = db.clone();
    let result = tokio::spawn(async move {
        let now = filetime_now();
        let deadline = now + FILETIME_DAY;
        let mut ids = Vec::new();
        for (at, bid, price, issued) in [(a, false, "20", now-2), (b, false, "10", now-1), (a, false, "10", now-1), (a, true, "20", now-2), (b, true, "30", now-1), (a, true, "30", now-1)] {
            let o = order(&work, person, at, TYPE_PLEX, bid, price, issued, Some(deadline)).await;
            orders::insert(&work, &o).await.unwrap();
            ids.push(o.id);
        }
        let first = snap(&work, TYPE_PLEX, a.region, now).await;
        let second = snap(&work, TYPE_PLEX, b.region, now).await;
        let rows: Vec<_> = first.rows.iter().map(|o| o.id).collect();
        assert_eq!(rows, vec![ids[4], ids[5], ids[3], ids[1], ids[2], ids[0]]);
        assert_eq!(rows, second.rows.iter().map(|o| o.id).collect::<Vec<_>>());
        assert_eq!((first.revision, first.changed_at, first.next_visible_expiry), (second.revision, second.changed_at, Some(deadline)));
        let restarted = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let restored = snap(&restarted, TYPE_PLEX, b.region, now).await;
        assert_eq!((restored.revision, restored.changed_at, restored.next_visible_expiry), (first.revision, first.changed_at, first.next_visible_expiry));
        assert_eq!(restored.rows.iter().map(|o| o.id).collect::<Vec<_>>(), rows);
        let expired = snap(&work, TYPE_PLEX, a.region, deadline).await;
        assert!(expired.rows.is_empty());
        assert_eq!(expired.revision, first.revision);
        assert_eq!(expired.latest_crossed_expiry, Some(deadline));
        assert_eq!(expired.next_visible_expiry, None);
        assert_eq!(orders::expired(&work, FILETIME_DAY, deadline).await.unwrap().iter().filter(|o| ids.contains(&o.id)).count(), ids.len());
        let asks = orders::asks(&work, now, FILETIME_DAY, TYPE_PLEX, b.region, money("100")).await.unwrap();
        assert_eq!(asks.iter().map(|o| o.id).collect::<Vec<_>>(), vec![ids[1], ids[2], ids[0]]);
        let bids = orders::bids(&work, now, FILETIME_DAY, TYPE_PLEX, a.region, money("1")).await.unwrap();
        assert_eq!(bids.iter().map(|o| o.id).collect::<Vec<_>>(), vec![ids[4], ids[5], ids[3]]);
        let best = listings::best_asks(&work, now, FILETIME_DAY, Area::Region, b.region, Some(TYPE_PLEX)).await.unwrap();
        assert_eq!(best.len(), 1);
        assert_eq!(best[0].station_id, b.id);
    }).await;
    cleanup(&db, &[person]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn best_ask_snapshot_bounds_rows_and_preserves_book_metadata() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, b) = isolated_stations(&db, &probe.sde).await;
    let person = fixture::create(&db).await.unwrap();
    let work = db.clone();
    let result = tokio::spawn(async move {
        let now = filetime_now();
        let ask_deadline = now + FILETIME_DAY;
        let bid_deadline = now + 2 * FILETIME_DAY;
        let mut ids = Vec::new();
        for (at, bid, price, issued, expires) in [
            (a, false, "20", now-3, ask_deadline),
            (b, false, "10", now-1, ask_deadline),
            (a, false, "10", now-2, ask_deadline),
            (b, false, "10", now-2, ask_deadline),
            (a, true, "30", now-1, bid_deadline),
            (b, true, "40", now-1, bid_deadline),
            (a, false, "1", now-3, now-1),
        ] {
            let o = order(&work, person, at, TYPE_PLEX, bid, price, issued, Some(expires)).await;
            orders::insert(&work, &o).await.unwrap();
            ids.push(o.id);
        }
        for (at, expected, future, crossed) in [
            (now, Some(ids[2]), Some(ask_deadline), Some(now-1)),
            (ask_deadline, None, Some(bid_deadline), Some(ask_deadline)),
            (bid_deadline, None, None, Some(bid_deadline)),
        ] {
            let full = snap(&work, TYPE_PLEX, a.region, at).await;
            let best = versions::best_ask_snapshot(&work, MarketScope::for_type(TYPE_PLEX), TYPE_PLEX, b.region, at, FILETIME_DAY).await.unwrap();
            assert_eq!(best.rows.iter().map(|o| o.id).collect::<Vec<_>>(), expected.into_iter().collect::<Vec<_>>());
            assert_eq!((best.revision, best.changed_at, best.next_visible_expiry, best.latest_crossed_expiry),
                       (full.revision, full.changed_at, full.next_visible_expiry, full.latest_crossed_expiry));
            assert_eq!((best.next_visible_expiry, best.latest_crossed_expiry), (future, crossed));
        }
    }).await;
    cleanup(&db, &[person]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn mutation_revisions_and_checked_storage_boundaries() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, _) = isolated_stations(&db, &probe.sde).await;
    let person = fixture::create(&db).await.unwrap();
    let work = db.clone();
    let result = tokio::spawn(async move {
        let now = filetime_now();
        let mut o = order(&work, person, a, TYPE_PLEX, false, "10", now, Some(now + FILETIME_DAY)).await;
        let start = snap(&work, TYPE_PLEX, a.region, now).await.revision;
        for field in 0..2 {
            for value in [i64::from(i32::MIN)-1, i64::from(i32::MAX)+1] {
                let mut invalid = o.clone();
                if field == 0 { invalid.range = value; } else { invalid.duration = value; }
                assert!(orders::insert(&work, &invalid).await.is_err());
                assert!(trading::archive_immediate(&work, &invalid, 0, CloseReason::Cancelled, now).await.is_err());
                assert!(orders::my_order(&work, person.character, invalid.id).await.unwrap().is_none());
                let n: i64 = work.query_one("SELECT count(*) AS n FROM game.market_order_history WHERE order_id=$1", &[&invalid.id]).await.unwrap().get("n");
                assert_eq!(n, 0);
            }
        }
        assert_eq!(snap(&work, TYPE_PLEX, a.region, now).await.revision, start);
        orders::insert(&work, &o).await.unwrap();
        assert_eq!(snap(&work, TYPE_PLEX, a.region, now).await.revision, start+1);
        assert_eq!(trading::reprice(&work, &o, o.price, o.escrow, now).await.unwrap(), 0);
        assert_eq!(trading::consume(&work, &o, 10, o.escrow, 10).await.unwrap(), 0);
        assert_eq!(snap(&work, TYPE_PLEX, a.region, now).await.revision, start+1);
        assert_eq!(trading::consume(&work, &o, 9, o.escrow, 9).await.unwrap(), 1);
        assert_eq!(trading::consume(&work, &o, 8, o.escrow, 8).await.unwrap(), 0);
        assert_eq!(snap(&work, TYPE_PLEX, a.region, now).await.revision, start+2);
        o.vol_remaining = 9;
        o.plex_escrow = 9;
        assert_eq!(trading::reprice(&work, &o, money("11"), o.escrow, now).await.unwrap(), 1);
        assert_eq!(snap(&work, TYPE_PLEX, a.region, now).await.revision, start+3);
        o.price = money("11");
        assert_eq!(trading::close(&work, &o, 9, o.escrow, 9, 0, CloseReason::Cancelled, now).await.unwrap(), 1);
        assert_eq!(trading::close(&work, &o, 9, o.escrow, 9, 0, CloseReason::Cancelled, now).await.unwrap(), 0);
        assert_eq!(snap(&work, TYPE_PLEX, a.region, now).await.revision, start+4);
    }).await;
    cleanup(&db, &[person]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn global_history_uses_weighted_numeric_utc_days() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, b) = isolated_stations(&db, &probe.sde).await;
    let person = fixture::create(&db).await.unwrap();
    let work = db.clone();
    let result = tokio::spawn(async move {
        let day = filetime_now() / FILETIME_DAY * FILETIME_DAY;
        let max = "9999999999999999999999999999999999.99";
        for (station, date, quantity, price) in [(a, day - 3*FILETIME_DAY, 1, "10"), (b, day - 3*FILETIME_DAY + 1, 3, "30"), (b, day, 2, max)] {
            trading::record(&work, &NewTransaction { type_id: TYPE_PLEX, quantity, price: money(price), buyer: person.character, seller: person.character, station: station.id, region: station.region, date, buyer_account_id: None, seller_account_id: None, order_id: None, scope: MarketScope::GlobalPlex, buy_order_id: None, sell_order_id: None, buyer_vault_user_id: None, seller_vault_user_id: None }).await.unwrap();
        }
        let rows = listings::price_history(&work, a.region, &[TYPE_PLEX], FILETIME_DAY, day-3*FILETIME_DAY).await.unwrap();
        let older = rows.iter().find(|r| r.day == day-3*FILETIME_DAY).unwrap();
        assert_eq!((older.volume, older.count), (4, 2));
        assert_eq!(money(&older.average), money("25"));
        assert_eq!((older.low, older.high), (money("10"), money("30")));
        let newest = rows.iter().find(|r| r.day == day).unwrap();
        assert_eq!((newest.volume, newest.count, newest.low, newest.high), (2, 1, money(max), money(max)));
        assert_eq!(money(&newest.average), money(max));
        assert!(!rows.iter().any(|r| r.day == day-2*FILETIME_DAY || r.day == day-FILETIME_DAY));
    }).await;
    cleanup(&db, &[person]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn regional_scope_and_legacy_deadline() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, b) = isolated_stations(&db, &probe.sde).await;
    let person = fixture::create(&db).await.unwrap();
    let work = db.clone();
    let result = tokio::spawn(async move {
        let now = filetime_now();
        // An otherwise unused test type avoids interference with seeded NPC orders.
        let ty = i32::MAX;
        let local = order(&work, person, a, ty, false, "10", now, Some(now + FILETIME_DAY)).await;
        let remote = order(&work, person, b, ty, false, "9", now, Some(now + FILETIME_DAY)).await;
        let legacy = order(&work, person, a, ty, false, "8", now - 31*FILETIME_DAY, None).await;
        for o in [&local, &remote, &legacy] { orders::insert(&work, o).await.unwrap(); }
        let here = snap(&work, ty, a.region, now).await;
        let there = snap(&work, ty, b.region, now).await;
        assert_eq!(here.rows.iter().map(|o| o.id).collect::<Vec<_>>(), vec![local.id]);
        assert_eq!(there.rows.iter().map(|o| o.id).collect::<Vec<_>>(), vec![remote.id]);
        assert_eq!(here.latest_crossed_expiry, Some(now-FILETIME_DAY));
        assert!(orders::expired(&work, FILETIME_DAY, now).await.unwrap().iter().any(|o| o.id == legacy.id));
        let listed = listings::orders(&work, now, FILETIME_DAY, ty, Area::Region, a.region, None).await.unwrap();
        assert_eq!(listed.iter().map(|o| o.order_id).collect::<Vec<_>>(), vec![local.id]);
        let best = listings::best_asks(&work, now, FILETIME_DAY, Area::Region, a.region, Some(ty)).await.unwrap();
        assert_eq!(best.len(), 1);
        assert_eq!(best[0].station_id, a.id);
        for field in 0..2 {
            for value in [i64::from(i32::MIN), i64::from(i32::MAX)] {
                let mut boundary = order(&work, person, a, ty, false, "10", now, Some(now + FILETIME_DAY)).await;
                if field == 0 { boundary.range = value; } else { boundary.duration = value; }
                assert_eq!(orders::insert(&work, &boundary).await.unwrap(), boundary.id);
                let stored = orders::my_order(&work, person.character, boundary.id).await.unwrap().unwrap();
                assert_eq!(if field == 0 { stored.range } else { stored.duration }, value);
                boundary.id = orders::next_order_id(&work).await.unwrap();
                assert_eq!(trading::archive_immediate(&work, &boundary, 0, CloseReason::Cancelled, now).await.unwrap(), boundary.id);
                let row = work.query_one("SELECT range::bigint AS range, duration::bigint AS duration FROM game.market_order_history WHERE order_id=$1", &[&boundary.id]).await.unwrap();
                assert_eq!(row.get::<_, i64>(if field == 0 { "range" } else { "duration" }), value);
            }
        }
    }).await;
    cleanup(&db, &[person]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn market_rpc_global_books_quotes_and_history_arity() {
    use crate::services::{self, Call};
    use marshal::Value;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, b) = isolated_stations(&db, &probe.sde).await;
    let person = fixture::create(&db).await.unwrap();
    let work = db.clone();
    let result = tokio::spawn(async move {
        let now = filetime_now();
        let o = order(&work, person, b, TYPE_PLEX, false, "10", now, Some(now + FILETIME_DAY)).await;
        orders::insert(&work, &o).await.unwrap();
        let (mut c, _peer) = client(work.clone(), person.character).await;
        c.session.apply(vec![("regionid", Value::Int(a.region)), ("stationid", Value::Int(a.id)), ("solarsystemid2", Value::Int(a.system))]);
        let kwargs = Value::Dict(vec![]);
        let empty = [];
        let plex_type = [Value::Int(i64::from(TYPE_PLEX))];
        fn call<'a>(method: &'a str, args: &'a [Value], kwargs: &'a Value) -> Call<'a> {
            Call { service: "marketProxy", method, args, kwargs, bound: false, bind_params: None }
        }
        let dedicated = services::dispatch(&mut c, &call("GetPlexOrders", &empty, &kwargs)).await.unwrap();
        let generic = services::dispatch(&mut c, &call("GetOrders", &plex_type, &kwargs)).await.unwrap();
        let Value::Instance(_, dedicated_state) = dedicated else { panic!("expected cached PLEX book") };
        let Value::Instance(_, generic_state) = generic else { panic!("expected cached generic book") };
        let d = dedicated_state.items().unwrap();
        let g = generic_state.items().unwrap();
        assert_eq!(d[1], g[1]);
        assert_eq!(d[2], g[2]);
        let repeated = services::dispatch(&mut c, &call("GetPlexOrders", &empty, &kwargs)).await.unwrap();
        let Value::Instance(_, repeated_state) = repeated else { panic!("expected cached PLEX book") };
        assert_eq!(*dedicated_state, *repeated_state);
        let best = services::dispatch(&mut c, &call("GetPlexBest", &empty, &kwargs)).await.unwrap();
        let Value::Instance(_, state) = best else { panic!("expected cached best") };
        assert_eq!(state.items().unwrap()[2], d[2]);
        for method in ["GetStationAsks", "GetSystemAsks", "GetRegionBest"] {
            let quote = services::dispatch(&mut c, &call(method, &empty, &kwargs)).await.unwrap();
            let Value::Dict(entries) = quote else { panic!("expected quote dictionary") };
            assert!(entries.iter().any(|(key, _)| *key == Value::Int(i64::from(TYPE_PLEX))));
        }
        let history = services::dispatch(&mut c, &call("GetPlexHistory", &empty, &kwargs)).await.unwrap();
        let Value::Dict(entries) = history else { panic!("expected history dictionary") };
        assert!(entries.iter().any(|(key, _)| *key == Value::Int(i64::from(TYPE_PLEX))));
        for method in ["GetPlexOldPriceHistory", "GetPlexNewPriceHistory"] {
            services::dispatch(&mut c, &call(method, &empty, &kwargs)).await.unwrap();
        }
        let absent = [Value::list([Value::Int(i64::from(i32::MAX))])];
        let history = services::dispatch(&mut c, &call("GetHistoryForManyTypeIDs", &absent, &kwargs)).await.unwrap();
        let Value::Dict(entries) = history else { panic!("expected history dictionary") };
        assert!(entries.iter().any(|(key, _)| *key == Value::Int(i64::from(i32::MAX))));
        work.execute("UPDATE game.market_orders SET created_at=$2, expires_at=$3 WHERE order_id=$1", &[&o.id, &(now-FILETIME_DAY), &(now-1)]).await.unwrap();
        let after = services::dispatch(&mut c, &call("GetPlexOrders", &empty, &kwargs)).await.unwrap();
        let Value::Instance(_, after_state) = after else { panic!("expected cached PLEX book") };
        assert_ne!(after_state.items().unwrap()[2], d[2]);
        for method in ["GetPlexOrders", "GetPlexBest", "GetPlexHistory", "GetPlexOldPriceHistory", "GetPlexNewPriceHistory"] {
            assert!(services::dispatch(&mut c, &call(method, &[Value::None], &kwargs)).await.is_err());
        }
        assert!(services::dispatch(&mut c, &call("GetOrders", &empty, &kwargs)).await.is_err());
        assert!(services::dispatch(&mut c, &call("GetOrders", &[Value::None], &kwargs)).await.is_err());
        assert!(services::dispatch(&mut c, &call("GetOrders", &[Value::Int(i64::from(TYPE_PLEX)), Value::None], &kwargs)).await.is_err());
    }).await;
    cleanup(&db, &[person]).await;
    result.unwrap();
}
