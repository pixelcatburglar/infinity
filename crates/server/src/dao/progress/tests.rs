use super::*;
use crate::lua::events::{combat::{BinaryAmount, CombatEntity}, resources::{FactContext, FactOrigin}};

fn npc_event() -> crate::lua::events::GameEvent {
    crate::lua::events::GameEvent::NpcDestroyed {
        death_id: "death".into(), spawn_generation: 1,
        npc: CombatEntity { item_id: 1, type_id: 1, incarnation: [1; 16], character_id: None, corporation_id: None, alliance_id: None, faction_id: None },
        system_id: 1, killer_id: None, killer: None,
        context: FactContext { corporation_id: Some(8), alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(1), station_id: None, origin: FactOrigin::Gm },
        occurred_at: 20, base_bounty: BinaryAmount { coefficient: "0".into(), exponent: 0 }, static_version: Some("test".into()),
    }
}

fn journal() -> JournalEvent {
    let event = npc_event();
    JournalEvent { source_namespace: "journal-test".into(), source_key: "death:1".into(), schema_version: codec::SCHEMA_VERSION,
        kind: event.kind().into(), occurred_at: 20, recorded_at: 30, payload: codec::encode(&event).unwrap(),
        context: "{\"a\":1,\"b\":2}".into(), origin: "gm".into(), ordinal: 7, subject_character_id: None }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn canonical_replay_collision_and_source_readback() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    tx.execute("INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ('journal-test-consumer',1,1,1,true)", &[]).await.unwrap();
    tx.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) VALUES ('journal-test-consumer','NpcDestroyed')", &[]).await.unwrap();
    let original = journal();
    let first = insert_batch(&tx, &[original.clone()]).await.unwrap();
    assert!(first[0].2);
    let mut replay = original.clone();
    replay.recorded_at += 1;
    replay.context = "{\"b\":2,\"a\":1}".into();
    let second = insert_batch(&tx, &[replay]).await.unwrap();
    assert_eq!(first[0].1, second[0].1);
    assert!(!second[0].2);
    let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.progress_deliveries WHERE consumer_key='journal-test-consumer'", &[]).await.unwrap().get("count");
    assert_eq!(count, 1);
    let read = source(&tx, &original.source_namespace, &original.source_key).await.unwrap().unwrap();
    assert_eq!(codec::encode(&read.event).unwrap(), original.payload);
    assert_eq!(read.recorded_at, original.recorded_at);
    assert!(source(&tx, &original.source_namespace, "missing").await.unwrap().is_none());

    for field in 0..8 {
        let mut changed = original.clone();
        match field {
            0 => changed.schema_version += 1,
            1 => changed.kind = "ShipLost".into(),
            2 => changed.occurred_at += 1,
            3 => changed.payload = "{}".into(),
            4 => changed.context = "{}".into(),
            5 => changed.origin = "server".into(),
            6 => changed.ordinal += 1,
            7 => changed.subject_character_id = Some(1),
            _ => unreachable!(),
        }
        let savepoint = tx.transaction().await.unwrap();
        let mut fresh = original.clone();
        fresh.source_key = format!("fresh:{field}");
        assert!(matches!(insert_batch(&savepoint, &[fresh.clone(), changed]).await, Err(Error::Progress(_))));
        savepoint.rollback().await.unwrap();
        assert!(source(&tx, &fresh.source_namespace, &fresh.source_key).await.unwrap().is_none());
    }
    let mut legacy = original.clone();
    legacy.source_key = "legacy".into();
    legacy.kind = "ShipLost".into();
    insert_batch(&tx, &[legacy.clone()]).await.unwrap();
    legacy.occurred_at += 1;
    legacy.ordinal += 1;
    assert!(!insert_batch(&tx, &[legacy]).await.unwrap()[0].2);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn delivery_state_and_bounded_character_snapshot() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let consumer = "journal-snapshot-test";
    tx.execute("INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ($1,1,1,1,true)", &[&consumer]).await.unwrap();
    tx.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) VALUES ($1,'NpcDestroyed')", &[&consumer]).await.unwrap();
    let mut entries = Vec::new();
    for index in 0..259 {
        let mut entry = journal();
        entry.source_namespace = format!("snapshot-test-{}", crate::handshake::random_u64());
        entry.source_key = index.to_string();
        entry.subject_character_id = Some(if index < 3 { 1 } else { 2 });
        entry.recorded_at = if index == 0 { 30 } else if index < 3 { 10 } else { 1 };
        entries.push(entry);
    }
    let inserted = insert_batch(&tx, &entries).await.unwrap();
    let key = DeliveryKey { consumer_key: consumer.into(), event_id: inserted[0].1.clone() };
    match delivery(&tx, &key).await.unwrap() {
        DeliveryState::Pending(event) => {
            assert_eq!(event.event_id, key.event_id);
            assert_eq!(event.schema_version, entries[0].schema_version);
            assert_eq!(event.source_key, entries[0].source_key);
        }
        _ => panic!("expected pending delivery"),
    }
    let all = pending_for_character(&tx, consumer, 1, 4).await.unwrap();
    assert_eq!(all.len(), 3);
    let mut expected: Vec<_> = inserted[..3].iter().enumerate()
        .map(|(index, row)| (entries[index].recorded_at, row.1.clone())).collect();
    expected.sort();
    assert_eq!(all.iter().map(|key| &key.event_id).collect::<Vec<_>>(),
        expected.iter().map(|(_, id)| id).collect::<Vec<_>>());
    let capped = pending_for_character(&tx, consumer, 1, 2).await.unwrap();
    assert_eq!(capped, all[..2]);
    mark_applied(&tx, &key).await.unwrap();
    assert!(matches!(delivery(&tx, &key).await.unwrap(), DeliveryState::Applied));
    assert_eq!(pending_for_character(&tx, consumer, 1, 4).await.unwrap().len(), 2);
    let missing = DeliveryKey { consumer_key: consumer.into(), event_id: "00000000-0000-0000-0000-000000000000".into() };
    assert!(matches!(delivery(&tx, &missing).await.unwrap(), DeliveryState::Missing));
    assert!(lock_pending(&tx, &missing).await.is_err());
    tx.rollback().await.unwrap();
}
