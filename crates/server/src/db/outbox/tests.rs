use super::*;

/// Inspect uncommitted entries without publishing them.
pub(crate) fn split<E>(outbox: Outbox<E>) -> (Vec<Effect<E>>, Vec<GameEvent>) {
    let mut effects = Vec::new();
    let mut events = Vec::new();
    for entry in outbox.entries {
        match entry {
            Entry::Notification(n) => effects.push(Effect::Notification(n)),
            Entry::Operation(o) => effects.push(Effect::Operation(o)),
            Entry::Event { event, .. } => events.push(event),
        }
    }
    (effects, events)
}

#[test]
fn routed_child_preserves_operations_events_and_order() {
    let mut parent = Outbox::<i32>::default();
    parent.operation(1);
    let mut child = Outbox::<i32>::default();
    child.notify_client("OnItemChange", vec![]);
    child.event(GameEvent::ItemsMoved { char_id: 2, item_id: 3, from_location_id: 4, to_location_id: 5, quantity: 1 });
    child.operation(6);
    parent.append_routed(child, |notification| match notification {
        Notification::DirectClient { event, args } => Notification::Pilot { char_id: 2, event, args },
        other => other,
    });
    assert!(matches!(&parent.entries[0], Entry::Operation(1)));
    assert!(matches!(&parent.entries[1], Entry::Notification(Notification::Pilot { char_id: 2, .. })));
    assert!(matches!(&parent.entries[2], Entry::Event { event: GameEvent::ItemsMoved { item_id: 3, .. }, .. }));
    assert!(matches!(&parent.entries[3], Entry::Operation(6)));
}

/// Read a character's committed journal events in source order.
pub(crate) async fn events(db: &crate::db::Db, kind: &str, character: i64) -> Vec<GameEvent> {
    db.query("SELECT schema_version,kind,payload::text AS payload FROM game.progress_events WHERE kind=$1 AND subject_character_id=$2 ORDER BY recorded_at,ordinal", &[&kind, &character]).await.unwrap()
        .iter().map(|row| crate::progress::codec::decode(row.get("schema_version"), row.get::<_, String>("kind").as_str(), row.get::<_, String>("payload").as_str()).unwrap()).collect()
}

pub(crate) async fn wallet_events(db: &crate::db::Db, owner: i64) -> Vec<GameEvent> {
    db.query("SELECT schema_version,kind,payload::text AS payload FROM game.progress_events WHERE kind='WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint=$1 ORDER BY recorded_at,ordinal", &[&owner]).await.unwrap()
        .iter().map(|row| crate::progress::codec::decode(row.get("schema_version"), row.get::<_, String>("kind").as_str(), row.get::<_, String>("payload").as_str()).unwrap()).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn failed_notification_does_not_skip_later_entries() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db, sde, 0);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut outbox = Outbox::default();
    outbox.notify_client("missing-client", vec![]);
    outbox.notify_sender(sender, "later", vec![]);
    dispatch_world((), outbox, &world).unwrap();
    assert_eq!(receiver.try_recv().unwrap().0, "later");
}

#[test]
fn resource_subject_and_earliest_occurrence_are_historical() {
    use crate::lua::events::resources::{FactContext, FactOrigin, MiningSlice};
    let context = FactContext { corporation_id: Some(8), alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Gm };
    let event = GameEvent::GasHarvested { char_id: 4, gas_type: 5, context, slices: vec![
        MiningSlice { log_id: 1, occurred_at: 30, quantity: 1, volume_m3: 1.0 },
        MiningSlice { log_id: 2, occurred_at: 20, quantity: 1, volume_m3: 1.0 },
    ] };
    assert_eq!(subject(&event), Some(4));
    let (time, context) = event.resource_metadata().unwrap();
    assert_eq!(time, 20);
    assert_eq!(context.origin.as_str(), "gm");
    assert_eq!(serde_json::to_value(context).unwrap()["corporation_id"], 8);
}

#[test]
fn sovereignty_publication_keeps_committed_source_generation() {
    let mut outbox = Outbox::<Infallible>::default();
    outbox.sovereignty(crate::services::sovereignty::CommittedPublication {
        sources: vec![crate::dao::sovereignty::sources::SourceChange {
            character_id: 42, revision: 3, effective_at: 123,
        }],
        ..Default::default()
    });
    let (effects, events) = split(outbox);
    assert!(events.is_empty());
    assert!(matches!(&effects[..], [Effect::Notification(Notification::Sovereignty(publication))]
        if publication.sources.len() == 1 && publication.sources[0].revision == 3
            && publication.sources[0].effective_at == 123));
}

fn npc_death(killer_id: Option<i64>) -> GameEvent {
    use crate::lua::events::{combat::{BinaryAmount, CombatEntity}, resources::{FactContext, FactOrigin}};
    GameEvent::NpcDestroyed {
        death_id: "death".into(), spawn_generation: 1,
        npc: CombatEntity { item_id: 1, type_id: 1, incarnation: [1; 16], character_id: None, corporation_id: None, alliance_id: None, faction_id: None },
        system_id: 1, killer_id, killer: killer_id.map(|id| CombatEntity { item_id: id, type_id: 1, incarnation: [1; 16], character_id: Some(id), corporation_id: None, alliance_id: None, faction_id: None }),
        context: FactContext { corporation_id: Some(8), alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(1), station_id: None, origin: FactOrigin::Gm },
        occurred_at: 20, base_bounty: BinaryAmount { coefficient: "0".into(), exponent: 0 }, static_version: Some("test".into()),
    }
}

#[test]
fn combat_subject_and_metadata_are_frozen() {
    let event = npc_death(None);
    assert_eq!(subject(&event), None);
    assert_eq!(subject(&npc_death(Some(4))), Some(4));
    let (time, context) = crate::lua::events::combat::metadata(&event).unwrap();
    assert_eq!(time, 20);
    assert_eq!(context.origin.as_str(), "gm");
    assert_eq!(context.corporation_id, Some(8));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn canonical_source_survives_position_changes_and_missing_source_is_refused() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let mut missing: Outbox = Outbox::default();
    missing.event(npc_death(None));
    assert!(matches!(crate::progress::prepare_batch(&mut missing, &tx, 1).await, Err(PrepareError::Progress(_))));
    let mut numeric: Outbox = Outbox::default();
    numeric.event_from(SourceKey::new("death", 1), npc_death(None));
    assert!(matches!(crate::progress::prepare_batch(&mut numeric, &tx, 1).await, Err(PrepareError::Progress(_))));
    let mut first: Outbox = Outbox::default();
    first.event_from(SourceKey::canonical("death", "runtime/system/batch/bucket/subslice", 42), npc_death(None));
    let inserted = crate::progress::prepare_batch(&mut first, &tx, 1).await.unwrap();
    assert!(inserted.contains("death:runtime/system/batch/bucket/subslice"));
    let envelope = crate::dao::progress::source(&tx, "business", "death:runtime/system/batch/bucket/subslice").await.unwrap().unwrap();
    assert_eq!(envelope.ordinal, 42);
    assert_eq!(envelope.occurred_at, 20);
    assert_eq!(envelope.origin, "gm");
    assert_eq!(envelope.subject_character_id, None);
    let mut replay: Outbox = Outbox::default();
    replay.event(GameEvent::CharacterSelected { user_id: 1, char_id: 1 });
    replay.event_from(SourceKey::canonical("death", "runtime/system/batch/bucket/subslice", 42), npc_death(None));
    let inserted = crate::progress::prepare_batch(&mut replay, &tx, 2).await.unwrap();
    assert!(!inserted.contains("death:runtime/system/batch/bucket/subslice"));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn daily_canonical_source_replay_conflict_and_rollback() {
    use crate::lua::events::{goals::{CompletionReason, GoalCategory, GoalPeriod}, resources::{FactContext, FactOrigin}};
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let id = crate::uuid_text::format(&crate::handshake::random_u64().to_be_bytes().repeat(2).try_into().unwrap());
    let event = GameEvent::DailyGoalCompleted { char_id: 1, goal_id: id.clone(), category: GoalCategory::Ordinary, period: GoalPeriod::Day, period_start: 0, period_end: 10, paid: false, reason: CompletionReason::Qa, occurred_at: 11, context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Qa } };
    let mut missing: Outbox = Outbox::default(); missing.event(event.clone());
    assert!(crate::progress::prepare_batch(&mut missing, &tx, 1).await.is_err());
    let key = format!("DailyGoalCompleted:{id}");
    let mut first: Outbox = Outbox::default(); first.event_from(SourceKey::canonical("DailyGoalCompleted", &id, 0), event.clone());
    assert!(crate::progress::prepare_batch(&mut first, &tx, 1).await.unwrap().contains(&key));
    let stored = crate::dao::progress::source(&tx, "business", &key).await.unwrap().unwrap();
    assert_eq!((stored.occurred_at, stored.origin.as_str(), stored.subject_character_id, stored.ordinal), (11, "qa", Some(1), 0));
    let mut replay: Outbox = Outbox::default(); replay.event_from(SourceKey::canonical("DailyGoalCompleted", &id, 0), event.clone());
    assert!(!crate::progress::prepare_batch(&mut replay, &tx, 2).await.unwrap().contains(&key));
    let mut changed = event.clone();
    if let GameEvent::DailyGoalCompleted { occurred_at, .. } = &mut changed { *occurred_at = 12; }
    let mut mismatch: Outbox = Outbox::default(); mismatch.event_from(SourceKey::canonical("DailyGoalCompleted", &id, 0), changed);
    assert!(crate::progress::prepare_batch(&mut mismatch, &tx, 3).await.is_err());
    let mut conflict: Outbox = Outbox::default(); conflict.event_from(SourceKey::canonical("DailyGoalCompleted", &id, 0), GameEvent::CharacterSelected { user_id: 1, char_id: 1 });
    assert!(crate::progress::prepare_batch(&mut conflict, &tx, 3).await.is_err());
    let other_id = crate::uuid_text::format(&crate::handshake::random_u64().to_be_bytes().repeat(2).try_into().unwrap());
    let mut unrelated: Outbox = Outbox::default();
    unrelated.event_from(SourceKey::canonical("DailyGoalCompleted", &other_id, 0), GameEvent::CharacterSelected { user_id: 1, char_id: 1 });
    crate::progress::prepare_batch(&mut unrelated, &tx, 4).await.unwrap();
    let mut incoming = event.clone();
    if let GameEvent::DailyGoalCompleted { goal_id, .. } = &mut incoming { *goal_id = other_id.clone(); }
    let mut incoming_canonical: Outbox = Outbox::default();
    incoming_canonical.event_from(SourceKey::canonical("DailyGoalCompleted", &other_id, 0), incoming);
    assert!(crate::progress::prepare_batch(&mut incoming_canonical, &tx, 5).await.is_err());
    tx.rollback().await.unwrap();
    let tx = client.transaction().await.unwrap();
    assert!(crate::dao::progress::source(&tx, "business", &key).await.unwrap().is_none());
    tx.rollback().await.unwrap();
}
