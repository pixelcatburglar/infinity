use super::{events, map, retention};
use crate::{dao::map_telemetry::{History, Visit}, services::{Call, CallError}};
use marshal::Value;

fn call<'a>(method: &'a str, args: &'a [Value], kwargs: &'a Value) -> Call<'a> {
    Call { service: "map", method, args, kwargs, bound: false, bind_params: None }
}
fn wire(value: Value) -> Value { marshal::loads(&marshal::dumps(&value)).unwrap() }

#[test]
fn protocol_shapes_and_validation() {
    let none = Value::None;
    let empty = Value::Dict(vec![]);
    let kwargs = Value::dict([("extra", Value::Int(1))]);
    for stat in [1, 3, 5] {
        for hours in [1, 24] {
            let args = [Value::Int(stat), Value::Int(hours)];
            assert!(map::history_request(&call("GetHistory", &args, &none)).is_ok());
            assert!(map::history_request(&call("GetHistory", &args, &empty)).is_ok());
            assert!(map::history_request(&call("GetHistory", &args, &kwargs)).is_err());
        }
    }
    for args in [vec![], vec![Value::Int(1)], vec![Value::Int(2), Value::Int(1)], vec![Value::Int(1), Value::Int(2)], vec![Value::Bool(true), Value::Int(1)], vec![Value::Int(1), Value::Int(1), Value::Int(1)]] {
        assert!(map::history_request(&call("GetHistory", &args, &none)).is_err());
    }
    assert!(map::history_request(&call("GetHistory", &[Value::Int(5), Value::Int(1)], &none)).unwrap().0.is_none());
    assert!(map::visits_request(&call("GetSolarSystemVisits", &[], &empty)).is_ok());
    assert!(map::visits_request(&call("GetSolarSystemVisits", &[Value::Int(1)], &none)).is_err());
    assert!(map::visits_request(&call("GetSolarSystemVisits", &[], &kwargs)).is_err());
    for character in [None, Some(0), Some(-1)] {
        assert!(matches!(map::selected(character), Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CHARACTER_NOT_FOUND));
    }
    assert_eq!(map::selected(Some(7)).unwrap(), 7);
    let rows = vec![History { solar_system_id: 42, value1: 5, value2: 2, value3: 1 }];
    let history = wire(map::history_value(rows));
    let Value::Instance(_, fields) = history else { panic!("expected rowset") };
    assert_eq!(fields.get("header"), Some(&Value::list([Value::str("solarSystemID"), Value::str("value1"), Value::str("value2"), Value::str("value3")])));
    assert_eq!(fields.get("lines"), Some(&Value::list([Value::list([Value::Int(42), Value::Int(5), Value::Int(2), Value::Int(1)])])));
    let Value::Instance(_, fields) = wire(map::history_value(vec![])) else { panic!("expected empty rowset") };
    assert_eq!(fields.get("lines"), Some(&Value::list([])));
    let visits = wire(map::visits_value(vec![Visit { last_date_time: 99, solar_system_id: 42, visits: 3 }]));
    let Value::List(visits) = visits else { panic!("expected visits list") };
    let Value::Instance(_, fields) = &visits[0] else { panic!("expected KeyVal") };
    assert_eq!(fields.get("lastDateTime"), Some(&Value::Int(99)));
    assert_eq!(fields.get("solarSystemID"), Some(&Value::Int(42)));
    assert_eq!(fields.get("visits"), Some(&Value::Int(3)));
    assert_eq!(wire(map::visits_value(vec![])), Value::list([]));
    assert_eq!(retention::cutoff(i64::MIN, 24), i64::MIN);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database and external flock"]
async fn map_dispatch_events_and_reconnect() {
    use crate::{dao::map_telemetry as dao, db::Db, services::{self, tests}};
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&cfg).await.unwrap();
    let id = dao::next_occurrence(&db).await.unwrap();
    let character = 4_000_000_000_000_000 + id;
    let system = -id;
    let now = crate::client::filetime_now();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        assert!(dao::append(tx, dao::EventSource::JumpOccurrence, id, system, now, dao::EventKind::Jump).await?);
        dao::record_visit(tx, character, system, now).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async { Ok(()) }).await.unwrap();
    events::npc_lost(&db, system, now).await;
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        events::player_lost(tx, -id, system, false, now).await?;
        events::player_lost(tx, -id - 1, system, true, now).await?;
        events::unpiloted_lost(tx, -id, system, now).await?;
        Ok(())
    }), |(), ()| async { Ok::<(), CallError>(()) }).await.unwrap();
    let (mut client, _peer) = tests::client(db.clone(), character).await;
    let none = Value::None;
    for (stat, expected) in [(1, vec![1, 0, 0]), (3, vec![3, 1, 1])] {
        let args = [Value::Int(stat), Value::Int(1)];
        let value = wire(services::dispatch(&mut client, &call("GetHistory", &args, &none)).await.unwrap());
        let Value::Instance(_, fields) = value else { panic!("expected rowset") };
        let lines = fields.get("lines").unwrap().items().unwrap();
        let row = lines.iter().find(|line| line.items().unwrap()[0] == Value::Int(system)).unwrap().items().unwrap();
        assert_eq!(&row[1..], expected.into_iter().map(Value::Int).collect::<Vec<_>>());
    }
    let args = [Value::Int(5), Value::Int(24)];
    assert_eq!(wire(services::dispatch(&mut client, &call("GetHistory", &args, &none)).await.unwrap()), wire(map::history_value(vec![])));
    assert_eq!(wire(services::dispatch(&mut client, &call("GetSolarSystemVisits", &[], &none)).await.unwrap()), wire(map::visits_value(vec![Visit { last_date_time: now, solar_system_id: system, visits: 1 }])));
    let system_id = client.sde.map.systems().next().expect("SDE system").solar_system_id;
    let location = [Value::Int(system_id)];
    let value = wire(services::dispatch(&mut client, &call("GetCurrentSovData", &location, &none)).await.unwrap());
    assert_eq!(crate::services::value_field(&value, "header"), Some(&Value::list([Value::str("locationID"), Value::str("corporationID"), Value::str("allianceID"), Value::str("claimTime")])));
    let expected = crate::services::sovereignty::queries::read(&client.db, &client.sde,
        crate::services::sovereignty::queries::Query::CurrentSovData { location_id: system_id }).await.unwrap();
    assert_eq!(value, wire(expected));
    let history = services::dispatch(&mut client, &call("GetRecentSovActivity", &[], &none)).await.unwrap();
    let expected = crate::services::sovereignty::queries::read(&client.db, &client.sde,
        crate::services::sovereignty::queries::Query::RecentSovActivity).await.unwrap();
    assert_eq!(wire(history), wire(expected));
    assert!(map::dispatch(&mut client, &call("GetCurrentSovData", &[], &none)).await.unwrap().is_err());
    assert!(super::dispatch(&mut client, &call("GetBeaconCount", &[], &none)).await.is_none());
    assert_eq!(services::dispatch(&mut client, &call("GetBeaconCount", &[], &none)).await.unwrap(), Value::Dict(vec![]));
    client.session.apply(vec![("charid", Value::None)]);
    assert!(matches!(services::dispatch(&mut client, &call("GetHistory", &args, &none)).await, Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CHARACTER_NOT_FOUND));
    let reopened = Db::connect(&cfg).await.unwrap();
    assert_eq!(dao::visits(&reopened, character).await.unwrap()[0].visits, 1);
    db.execute("DELETE FROM game.map_events WHERE solar_system_id = $1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.map_visits WHERE character_id = $1", &[&character]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database and external flock"]
async fn retention_handler_survives_reconnect_and_retry() {
    use infinity_core::consts::FILETIME_HOUR;
    use crate::{dao::{jobs, map_telemetry as dao}, db::Db, scheduler, services::{self, tests}};

    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&cfg).await.unwrap();
    let expired_id = dao::next_occurrence(&db).await.unwrap();
    let retained_id = dao::next_occurrence(&db).await.unwrap();
    let system = -expired_id;
    let character = 4_000_000_000_000_000 + expired_id;
    let now = crate::client::filetime_now();
    let cutoff = retention::cutoff(now, crate::config::game().map.history_retention_hours);
    let expired = cutoff - FILETIME_HOUR;
    let retained = cutoff + FILETIME_HOUR;
    for (id, time) in [(expired_id, expired), (retained_id, retained)] {
        db.transaction(|tx, _: &mut ()| Box::pin(async move {
            if dao::append(tx, dao::EventSource::JumpOccurrence, id, system, time, dao::EventKind::Jump).await? {
                dao::record_visit(tx, character, system, time).await?;
            } else {
                panic!("fresh occurrence must be accepted");
            }
            Ok(())
        }), |(), ()| async { Ok::<(), CallError>(()) }).await.unwrap();
    }

    let sentinel = scheduler::Job { id: format!("map-retention-test-{expired_id}"), kind: "test:unrelated".into(), due_at: expired, payload: "{}".into() };
    scheduler::schedule(&db, &sentinel).await.unwrap();
    let (client, _peer) = tests::client(db.clone(), character).await;
    let world = client.world.clone();
    let reopened = Db::connect(&cfg).await.unwrap();
    let visits = vec![Visit { last_date_time: retained, solar_system_id: system, visits: 2 }];
    assert_eq!(dao::visits(&reopened, character).await.unwrap(), visits);
    let before = dao::history(&reopened, dao::HistoryStat::Jumps, expired, retained).await.unwrap();
    assert!(before.contains(&History { solar_system_id: system, value1: 2, value2: 0, value3: 0 }));

    assert!(scheduler::PERIODIC_KINDS.contains(&scheduler::MAP_HISTORY));
    let registrations = services::scheduler_registrations();
    let registration = registrations.iter().find(|entry| entry.kind == scheduler::MAP_HISTORY).unwrap();
    assert_eq!(registration.interval_secs, Some(crate::config::game().map.maintenance_secs as u64));
    let job = scheduler::Job { id: scheduler::MAP_HISTORY.into(), kind: scheduler::MAP_HISTORY.into(), due_at: now, payload: "{}".into() };
    for _ in 0..2 {
        retention::handle(reopened.clone(), world.clone(), job.clone()).await.unwrap();
        let history = dao::history(&reopened, dao::HistoryStat::Jumps, expired, retained).await.unwrap();
        assert!(history.contains(&History { solar_system_id: system, value1: 1, value2: 0, value3: 0 }));
        assert!(dao::history(&reopened, dao::HistoryStat::Jumps, expired, expired).await.unwrap().iter().all(|row| row.solar_system_id != system));
        assert_eq!(dao::visits(&reopened, character).await.unwrap(), visits);
        let unchanged = jobs::lock_job(&reopened, &sentinel.id).await.unwrap().unwrap();
        assert_eq!((unchanged.kind, unchanged.due_at, unchanged.payload), (sentinel.kind.clone(), sentinel.due_at, sentinel.payload.clone()));
    }
    reopened.execute("DELETE FROM game.map_events WHERE solar_system_id = $1", &[&system]).await.unwrap();
    reopened.execute("DELETE FROM game.map_visits WHERE character_id = $1", &[&character]).await.unwrap();
    scheduler::cancel(&reopened, &sentinel.id).await.unwrap();
}
