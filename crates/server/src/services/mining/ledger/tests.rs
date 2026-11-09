use super::*;
use infinity_core::{consts::{FILETIME_DAY, FILETIME_SECOND, FILETIME_UNIX_EPOCH}};
use crate::{dao::mining_ledger::{self, Log}};

#[test]
fn wire_rows_round_trip_with_exact_fields() {
    let event_time = FILETIME_UNIX_EPOCH + 20_000 * FILETIME_DAY + 123;
    let quantity = i64::from(i32::MAX) + 37;
    let waste = quantity + 5;
    let log = Log { log_id: 7, character_id: 8, event_time, solar_system_id: 9, type_id: 10, quantity, quantity_wasted: waste };
    let expected = Value::instance("utillib.KeyVal", [
        ("eventDate", Value::Int(event_time)),
        ("solarsystemID", Value::Int(9)),
        ("typeID", Value::Int(10)),
        ("quantity", Value::Int(quantity)),
        ("quantityWasted", Value::Int(waste)),
    ]);
    let rows = Value::list([row(log)]);
    assert_eq!(marshal::loads(&marshal::dumps(&rows)).unwrap(), Value::list([expected]));
    let empty = Value::list([]);
    assert_eq!(marshal::loads(&marshal::dumps(&empty)).unwrap(), empty);
}

#[test]
fn request_accepts_only_authenticated_zero_argument_calls() {
    assert_eq!(validate_request(Some(42), &[], &Value::None).unwrap(), 42);
    assert_eq!(validate_request(Some(42), &[], &Value::Dict(vec![])).unwrap(), 42);
    for character in [None, Some(0), Some(-1)] {
        assert!(validate_request(character, &[], &Value::None).is_err());
    }
    assert!(validate_request(Some(42), &[Value::Int(43)], &Value::None).is_err());
    assert!(validate_request(Some(42), &[], &Value::dict([("characterID", Value::Int(43))])).is_err());
    assert!(validate_request(Some(42), &[], &Value::Int(43)).is_err());
}

#[test]
fn retention_cutoff_is_a_rolling_filetime_window() {
    crate::tests::init_config();
    let now = FILETIME_UNIX_EPOCH + 30_000 * FILETIME_DAY + 123;
    let days = crate::config::game().mining.ledger_retention_days as i64;
    let cutoff = super::super::retention::cutoff(now);
    assert_eq!(now - cutoff, days * FILETIME_DAY);
    assert_eq!(super::super::retention::cutoff(now + 1), cutoff + 1);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn dao_isolation_order_bounds_and_strict_retention() {
    crate::tests::init_config();
    let mut fixture = crate::services::drones::tests::fixture::Fixture::new().await;
    let (b, b_ship) = fixture.add_pilot().await;
    let a = fixture.owner;
    let a_ship = fixture.ship;
    let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut client, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let cutoff = 1_000_i64;
    let large = i64::from(i32::MAX) + 100;
    for (owner, time, qty) in [(a, cutoff - 1, 1), (a, cutoff, large), (a, cutoff, large + 1), (a, cutoff + 1, 3), (b, cutoff - 1, 4), (b, cutoff, 5), (b, cutoff + 1, 6)] {
        let ship_id = if owner == a { a_ship } else { b_ship };
        mining_ledger::append(&tx, &mining_ledger::DeliveryFact { character_id: owner, ship_id, solar_system_id: 77, type_id: 88, quantity: qty, quantity_wasted: large, event_time: time, unit_volume_m3: 1.0, resource_kind: crate::lua::events::resources::MiningResourceKind::Ore }).await.unwrap();
    }
    let snapshot = tx.query_one("SELECT source_ship_id, source_ship_type_id, corporation_id, faction_id, resource_kind, unit_volume_m3, progress_aggregated FROM game.character_mining_ledger WHERE character_id = $1 ORDER BY log_id LIMIT 1", &[&a]).await.unwrap();
    assert_eq!(snapshot.get::<_, Option<i64>>("source_ship_id"), Some(a_ship));
    assert_eq!(snapshot.get::<_, Option<i64>>("corporation_id"), Some(1));
    assert_eq!(snapshot.get::<_, Option<i64>>("faction_id"), None);
    assert_eq!(snapshot.get::<_, Option<String>>("resource_kind").as_deref(), Some("ore"));
    assert_eq!(snapshot.get::<_, Option<f64>>("unit_volume_m3"), Some(1.0));
    assert!(!snapshot.get::<_, bool>("progress_aggregated"));
    assert!(snapshot.get::<_, Option<i32>>("source_ship_type_id").is_some());
    tx.execute("UPDATE game.characters SET corporation_id = 2 WHERE character_id = $1", &[&a]).await.unwrap();
    assert_eq!(tx.query_one("SELECT corporation_id FROM game.character_mining_ledger WHERE character_id = $1 ORDER BY log_id LIMIT 1", &[&a]).await.unwrap().get::<_, Option<i64>>("corporation_id"), Some(1));
    tx.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = $1", &[&a]).await.unwrap();
    let malformed: i64 = tx.query_one("INSERT INTO game.character_mining_ledger (character_id, event_time, solar_system_id, type_id, quantity, quantity_wasted, progress_aggregated) VALUES ($1, $2, 77, 88, 1, 0, false) RETURNING log_id", &[&a, &cutoff]).await.unwrap().get("log_id");
    assert!(mining_ledger::progress::lock_pending(&tx, 10000).await.is_err());
    assert!(!tx.query_one("SELECT progress_aggregated FROM game.character_mining_ledger WHERE log_id = $1", &[&malformed]).await.unwrap().get::<_, bool>("progress_aggregated"));
    tx.execute("DELETE FROM game.character_mining_ledger WHERE log_id = $1", &[&malformed]).await.unwrap();
    let missing = mining_ledger::DeliveryFact { character_id: a, ship_id: -1, solar_system_id: 77, type_id: 88, quantity: 1, quantity_wasted: 0, event_time: cutoff, unit_volume_m3: 1.0, resource_kind: crate::lua::events::resources::MiningResourceKind::Ore };
    assert!(mining_ledger::append(&tx, &missing).await.is_err());
    let legacy: i64 = tx.query_one("INSERT INTO game.character_mining_ledger (character_id, event_time, solar_system_id, type_id, quantity, quantity_wasted) VALUES ($1, $2, 77, 88, 1, 0) RETURNING log_id", &[&a, &(cutoff - 1)]).await.unwrap().get("log_id");
    assert!(tx.query_one("SELECT progress_aggregated FROM game.character_mining_ledger WHERE log_id = $1", &[&legacy]).await.unwrap().get::<_, bool>("progress_aggregated"));
    mining_ledger::delete_before(&tx, cutoff).await.unwrap();
    assert_eq!(mining_ledger::list(&tx, a, i64::MIN, i64::MAX).await.unwrap().len(), 4);
    let rows = mining_ledger::list(&tx, a, cutoff, cutoff + 1).await.unwrap();
    assert_eq!(rows.iter().map(|r| (r.event_time, r.quantity, r.quantity_wasted)).collect::<Vec<_>>(), vec![(cutoff + 1, 3, large), (cutoff, large + 1, large), (cutoff, large, large)]);
    assert!(rows[1].log_id > rows[2].log_id);
    assert_eq!(rows, mining_ledger::list(&tx, a, cutoff, cutoff + 1).await.unwrap());
    assert_eq!(mining_ledger::list(&tx, b, cutoff, cutoff).await.unwrap().iter().map(|r| r.quantity).collect::<Vec<_>>(), vec![5]);
    let ids: Vec<i64> = mining_ledger::list(&tx, a, i64::MIN, i64::MAX).await.unwrap().into_iter().chain(mining_ledger::list(&tx, b, i64::MIN, i64::MAX).await.unwrap()).map(|row| row.log_id).collect();
    assert_eq!(mining_ledger::progress::mark_aggregated(&tx, &ids).await.unwrap(), ids.len() as u64);
    // The deletion is global; the transaction restores any pre-existing older history too.
    mining_ledger::delete_before(&tx, cutoff).await.unwrap();
    assert_eq!(mining_ledger::list(&tx, a, i64::MIN, i64::MAX).await.unwrap(), rows);
    assert_eq!(mining_ledger::list(&tx, b, i64::MIN, i64::MAX).await.unwrap().iter().map(|r| r.quantity).collect::<Vec<_>>(), vec![6, 5]);
    assert_eq!(mining_ledger::delete_before(&tx, cutoff).await.unwrap(), 0);
    tx.rollback().await.unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn pending_locks_skip_and_late_lower_id_is_not_lost() {
    let fixture = crate::services::drones::tests::fixture::Fixture::new().await;
    let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut first, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let (mut second, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let (mut third, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let low = first.transaction().await.unwrap();
    let fact = mining_ledger::DeliveryFact { character_id: fixture.owner, ship_id: fixture.ship, solar_system_id: fixture.system, type_id: 88, quantity: 1, quantity_wasted: 0, event_time: 1000, unit_volume_m3: 1.0, resource_kind: crate::lua::events::resources::MiningResourceKind::Ore };
    mining_ledger::append(&low, &fact).await.unwrap();
    let low_id = mining_ledger::list(&low, fixture.owner, i64::MIN, i64::MAX).await.unwrap()[0].log_id;
    let high = second.transaction().await.unwrap();
    mining_ledger::append(&high, &fact).await.unwrap();
    let high_id = mining_ledger::list(&high, fixture.owner, i64::MIN, i64::MAX).await.unwrap()[0].log_id;
    assert!(low_id < high_id);
    high.commit().await.unwrap();
    let locked = second.transaction().await.unwrap();
    let rows = mining_ledger::progress::lock_pending(&locked, 10000).await.unwrap();
    assert!(rows.iter().any(|row| row.log_id == high_id));
    let skipped = third.transaction().await.unwrap();
    let rows = mining_ledger::progress::lock_pending(&skipped, 10000).await.unwrap();
    assert!(!rows.iter().any(|row| row.log_id == high_id));
    skipped.rollback().await.unwrap();
    assert_eq!(mining_ledger::progress::mark_aggregated(&locked, &[high_id]).await.unwrap(), 1);
    locked.commit().await.unwrap();
    low.commit().await.unwrap();
    let later = second.transaction().await.unwrap();
    let rows = mining_ledger::progress::lock_pending(&later, 10000).await.unwrap();
    assert!(rows.iter().any(|row| row.log_id == low_id));
    assert!(!rows.iter().any(|row| row.log_id == high_id));
    later.rollback().await.unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn unbound_rpc_returns_only_retained_owner_logs() -> anyhow::Result<()> {
    use anyhow::{anyhow, ensure, Context};
    use crate::services::drones::tests::fixture::Fixture;

    let mut f = Fixture::new().await;
    let result: anyhow::Result<()> = async {
        let kwargs = Value::None;
        let call = Call { service: "characterMiningLedger", method: "GetCharacterLogs", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let empty = crate::services::dispatch(&mut f.client, &call).await
            .map_err(|e| anyhow!("empty ledger RPC: {e:?}"))?;
        ensure!(empty == Value::list([]), "new owner ledger was not empty: {empty:?}");

        let (other, other_ship) = f.add_pilot().await;
        let type_id = f.world.ball(f.system, f.ship).await.context("fixture ship missing")?.type_id;
        let now = crate::client::filetime_now();
        let expired = super::super::retention::cutoff(now) - FILETIME_DAY;
        let retained = now - FILETIME_SECOND;
        ensure!(retained > super::super::retention::cutoff(now), "retention window too short for test");
        for (owner, ship, quantity, waste, time) in [(f.owner, f.ship, 1, 2, expired), (f.owner, f.ship, 11, 3, retained), (f.owner, f.ship, 22, 4, retained), (other, other_ship, 33, 5, retained)] {
            mining_ledger::append(&f.db, &mining_ledger::DeliveryFact { character_id: owner, ship_id: ship, solar_system_id: f.system, type_id, quantity, quantity_wasted: waste, event_time: time, unit_volume_m3: 1.0, resource_kind: crate::lua::events::resources::MiningResourceKind::Ore }).await?;
        }

        let expected = Value::list([(22, 4), (11, 3)].map(|(quantity, waste)| Value::instance("utillib.KeyVal", [
            ("eventDate", Value::Int(retained)),
            ("solarsystemID", Value::Int(f.system)),
            ("typeID", Value::Int(i64::from(type_id))),
            ("quantity", Value::Int(quantity)),
            ("quantityWasted", Value::Int(waste)),
        ])));
        let stored = mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await?;
        ensure!(stored.len() == 3 && stored[2].event_time == expired, "expired fact missing from storage: {stored:?}");
        for _ in 0..2 {
            let actual = crate::services::dispatch(&mut f.client, &call).await
                .map_err(|e| anyhow!("ledger RPC: {e:?}"))?;
            let actual = marshal::loads(&marshal::dumps(&actual)).unwrap();
            ensure!(actual == expected, "unexpected ledger response: {actual:?}");
        }
        ensure!(mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await? == stored, "ledger reads changed stored facts");
        let observers = Call { service: "corpMiningLedger", method: "GetObserversWithMiningEvents", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        ensure!(crate::services::mining::dispatch(&mut f.client, &observers).await.is_none(), "corporate observer service was claimed");
        Ok(())
    }.await;
    f.cleanup().await;
    result
}
