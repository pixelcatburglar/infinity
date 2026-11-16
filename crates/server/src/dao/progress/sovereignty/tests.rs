//! Real PostgreSQL replay ordering, exclusion and pagination checks.
use super::*;
use crate::dao::progress::JournalEvent;
use crate::lua::events::{combat::{BinaryAmount, CombatEntity}, resources::{FactContext, FactOrigin, MiningSlice}, GameEvent};
use crate::progress::codec;

fn context(system: i64) -> FactContext {
    FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(system), station_id: None, origin: FactOrigin::Natural }
}
fn journal(event: &GameEvent, key: &str, at: i64, origin: &str, context: &FactContext) -> JournalEvent {
    JournalEvent { source_namespace: "business".into(), source_key: key.into(), schema_version: codec::SCHEMA_VERSION,
        kind: event.kind().into(), occurred_at: at, recorded_at: at + 1, payload: codec::encode(event).unwrap(),
        context: serde_json::to_string(context).unwrap(), origin: origin.into(), ordinal: 0, subject_character_id: None }
}
fn npc(system: i64, at: i64) -> GameEvent {
    GameEvent::NpcDestroyed { death_id: "d".into(), spawn_generation: 1,
        npc: CombatEntity { item_id: 1, type_id: 1, incarnation: [1; 16], character_id: None, corporation_id: None, alliance_id: None, faction_id: None },
        system_id: system, killer_id: None, killer: None, context: context(system), occurred_at: at,
        base_bounty: BinaryAmount { coefficient: "0".into(), exponent: 0 }, static_version: None }
}
fn ore(system: i64, slices: &[(i64, i64)]) -> GameEvent {
    GameEvent::OreMined { char_id: 1, ore_type: 2, context: context(system),
        slices: slices.iter().map(|&(log_id, occurred_at)| MiningSlice { log_id, occurred_at, quantity: 1, volume_m3: log_id as f64 }).collect() }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn replay_pages_interleaved_slices_with_named_pending() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let consumer = format!("sov-replay-{}", crate::handshake::random_u64());
    let system = (crate::handshake::random_u64() >> 10) as i64 + 1;
    db.execute("INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ($1,1,1,1,true)", &[&consumer]).await.unwrap();
    db.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) SELECT $1, unnest($2::text[])", &[&consumer, &vec!["NpcDestroyed".to_owned(), "OreMined".to_owned()]]).await.unwrap();
    let m1: Vec<(i64, i64)> = (0..150_i64).map(|i| (i + 1, 100 + 2 * i)).collect();
    let m2: Vec<(i64, i64)> = (0..140_i64).map(|i| (200 + i, 101 + 2 * i)).collect();
    let mut events = vec![
        journal(&ore(system, &m1), "OreMined:1", 100, "natural", &context(system)),
        journal(&ore(system, &m2), "OreMined:2", 101, "natural", &context(system)),
        journal(&npc(system, 5000), "NpcDestroyed:1", 5000, "natural", &context(system)),
        journal(&npc(system, 6000), "NpcDestroyed:gm", 6000, "gm", &context(system)),
        journal(&ore(system, &[(400, 7000)]), "OreMined:pending", 7000, "natural", &context(system)),
        journal(&npc(system + 1, 5000), "NpcDestroyed:other", 5000, "natural", &context(system + 1)),
        journal(&npc(i64::MAX, 8000), "NpcDestroyed:max", 8000, "natural", &context(i64::MAX)),
        JournalEvent { source_namespace: "business".into(), source_key: "NpcDestroyed:overflow".into(), schema_version: codec::SCHEMA_VERSION,
            kind: "NpcDestroyed".into(), occurred_at: 9000, recorded_at: 9001, payload: "{\"NpcDestroyed\":{\"system_id\":9223372036854775808}}".into(),
            context: "{}".into(), origin: "natural".into(), ordinal: 0, subject_character_id: None },
        JournalEvent { source_namespace: "business".into(), source_key: "NpcDestroyed:malformed".into(), schema_version: codec::SCHEMA_VERSION,
            kind: "NpcDestroyed".into(), occurred_at: 9001, recorded_at: 9002, payload: "{\"NpcDestroyed\":{\"system_id\":\"e2\"}}".into(),
            context: "{}".into(), origin: "natural".into(), ordinal: 0, subject_character_id: None },
    ];
    for event in &mut events {
        event.source_namespace = consumer.clone();
    }
    let inserted = crate::dao::progress::insert_batch(&db, &events, &[consumer.as_str(), consumer.as_str()], &["NpcDestroyed", "OreMined"]).await.unwrap();
    let pending_id = inserted.iter().find(|(key, _, _)| key == "OreMined:pending").unwrap().1.clone();
    let event_ids: Vec<String> = inserted.iter().map(|row| row.1.clone()).collect();
    db.execute("UPDATE game.progress_deliveries SET state='applied' WHERE consumer_key=$1 AND event_id <> $2::text::uuid", &[&consumer, &pending_id]).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let first = replay_page(&db, &consumer, system, None, 0, i64::MAX, None, 256).await.unwrap();
        assert_eq!(first.len(), 256);
        let keys: Vec<_> = first.iter().map(|s| (s.order.occurred_at, s.order.source_namespace.clone(), s.order.source_key.clone(), s.order.ordinal, s.order.slice_id)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert!(first.iter().any(|s| s.order.source_key == "OreMined:1") && first.iter().any(|s| s.order.source_key == "OreMined:2"));
        let second = replay_page(&db, &consumer, system, None, 0, i64::MAX, Some(&first.last().unwrap().order), 256).await.unwrap();
        assert_eq!(second.len(), 35);
        assert!(second.iter().all(|s| s.order.source_key != "OreMined:pending"));
        let named = replay_page(&db, &consumer, system, Some(pending_id.as_str()), 0, i64::MAX, None, 512).await.unwrap();
        assert_eq!(named.len(), 292);
        assert!(named.iter().any(|s| s.order.slice_id == 400 && matches!(s.increment, Increment::MinedM3(volume) if volume == 400.0)));
        let death = named.iter().find(|s| s.order.source_key == "NpcDestroyed:1").unwrap();
        assert_eq!((death.order.slice_id, death.order.occurred_at, death.system_id), (0, 5000, system));
        assert!(matches!(death.increment, Increment::NpcDeath));
        assert!(replay_page(&db, &consumer, system, None, 6000, i64::MAX, None, 256).await.unwrap().is_empty());
        let edge = replay_page(&db, &consumer, i64::MAX, None, 0, i64::MAX, None, 16).await.unwrap();
        assert_eq!(edge.len(), 1);
        assert_eq!(edge[0].system_id, i64::MAX);
    }).await;
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id = ANY(SELECT unnest($1::text[])::uuid)", &[&event_ids]).await.unwrap();
    db.execute("DELETE FROM game.progress_subscription_kinds WHERE consumer_key=$1", &[&consumer]).await.unwrap();
    db.execute("DELETE FROM game.progress_subscriptions WHERE consumer_key=$1", &[&consumer]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE event_id = ANY(SELECT unnest($1::text[])::uuid)", &[&event_ids]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
