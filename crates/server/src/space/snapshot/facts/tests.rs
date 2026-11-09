use super::*;
use crate::lua::events::combat::CombatEntity;
use crate::lua::events::resources::{FactContext, FactOrigin};
use crate::space::combat_facts::{CombatFacts, CombatSource};

pub(crate) fn facts_test_init(system: &mut System, key_capacity: usize, handle_capacity: usize) {
    assert!(system.combat_facts.is_none());
    system.combat_facts = Some(CombatFacts::new(crate::handshake::random_u64(), system.id, key_capacity, handle_capacity));
}

fn entity(id: i64) -> CombatEntity {
    CombatEntity { item_id: id, type_id: 1, incarnation: [1; 16], character_id: Some(id), corporation_id: None, alliance_id: None, faction_id: None }
}
fn source(sequence: u64) -> CombatSource {
    let mut activation = [0; 16]; activation[15] = 1;
    CombatSource { actual_actor_character: Some(1), entity: entity(1),
        context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: Some(1), ship_type_id: Some(1), system_id: Some(1), station_id: None, origin: FactOrigin::Natural },
        source_is_npc: false, controller_incarnation: Some([1; 16]),
        module_id: Some(2), effect_id: Some(3), weapon_type_id: Some(4), activation, sequence }
}
fn hit(facts: &mut CombatFacts, sequence: u64, at: i64) {
    let target = entity(2);
    let source = source(sequence);
    facts.reserve_attempt(&source).unwrap();
    let mut reservation = facts.reserve_damage(source, &target, at).unwrap();
    facts.commit_damage(&mut reservation, &target, [1.0, 0.0, 0.0], false).unwrap();
    facts.release(reservation);
    facts.finish_attempt(&source);
}

#[test]
fn empty_capture_has_no_frame() {
    crate::tests::init_config();
    let mut system = System::default();
    assert!(capture_fact_checkpoint(&mut system).unwrap().is_none());
}

#[test]
fn sealed_frame_ack_keeps_later_mutations_and_existing_cadence() {
    crate::tests::init_config();
    let mut system = System::default();
    let mut facts = CombatFacts::new(7, 1, 32, 4);
    let mut activation = [0; 16]; activation[15] = 1;
    facts.register_source(activation, 1, 0).unwrap();
    let at = system.combat_fact_time();
    hit(&mut facts, 0, at);
    system.combat_facts = Some(facts);
    assert!(!system.is_cold(false));
    system.tick(1);
    let due = system.snapshot_due;
    assert_eq!(due, 1 + crate::config::game().space.position_snapshot_secs * crate::space::MILLISECONDS_PER_SECOND);
    let frame = capture_fact_checkpoint(&mut system).unwrap().unwrap();
    hit(system.combat_facts.as_mut().unwrap(), 1, at);
    frame.acknowledge(&mut system, &HashSet::new());
    assert!(system.combat_facts.as_ref().unwrap().is_dirty());
    assert_eq!(system.snapshot_due, due);
}


#[test]
fn full_handle_drain_preserves_low_level_reserved_rows() {
    crate::tests::init_config();
    let mut system = System::default();
    let mut facts = CombatFacts::new(7, 1, 32, 1);
    let mut activation = [0; 16]; activation[15] = 1;
    facts.register_source(activation, 1, 0).unwrap();
    let at = system.combat_fact_time();
    hit(&mut facts, 0, at);
    // Low-level capture preflight only: production reservations are committed and
    // released within one actor turn, before any checkpoint mailbox can run.
    let target = entity(3);
    let source = source(1);
    facts.reserve_attempt(&source).unwrap();
    let mut reserved = facts.reserve_damage(source, &target, at).unwrap();
    facts.commit_damage(&mut reserved, &target, [1.0, 0.0, 0.0], false).unwrap();
    system.combat_facts = Some(facts);
    let first = capture_fact_checkpoint(&mut system).unwrap().unwrap();
    system.combat_facts.as_mut().unwrap().release(reserved);
    system.combat_facts.as_mut().unwrap().finish_attempt(&source);
    let retry = capture_fact_checkpoint(&mut system).unwrap().unwrap();
    assert!(Arc::ptr_eq(&first.batches[0], &retry.batches[0]));
    assert!(!system.is_cold(false));
    retry.acknowledge(&mut system, &HashSet::new());
    assert_eq!(system.combat_facts.as_ref().unwrap().batch_count(), 0);
    assert!(system.combat_facts.as_ref().unwrap().is_dirty());
    assert!(capture_fact_checkpoint(&mut system).unwrap().is_some());
}

#[test]
fn invalid_capture_refuses_without_discarding_active_facts() {
    crate::tests::init_config();
    let mut system = System::default();
    let mut facts = CombatFacts::new(7, 1, 32, 4);
    let mut activation = [0; 16]; activation[15] = 1;
    facts.register_source(activation, 1, 0).unwrap();
    hit(&mut facts, 0, crate::client::filetime_from_unix_ms(10));
    system.combat_facts = Some(facts);
    assert!(matches!(capture_fact_checkpoint_at(&mut system, crate::client::filetime_from_unix_ms(0)), Err(Refusal::Invalid)));
    assert!(system.combat_facts.as_ref().unwrap().is_dirty());
    assert!(!system.is_cold(false));
}

#[test]
fn mailbox_fact_capture_uses_actor_clock_before_next_tick() {
    crate::tests::init_config();
    let mut system = System::default();
    let mut facts = CombatFacts::new(7, 1, 32, 4);
    let mut activation = [0; 16]; activation[15] = 1;
    facts.register_source(activation, 1, 0).unwrap();
    let accepted_at = system.combat_fact_time();
    hit(&mut facts, 0, accepted_at);
    system.combat_facts = Some(facts);
    let frame = capture_fact_checkpoint(&mut system).unwrap().unwrap();
    let events = frame.batches[0].events();
    match &events[0].event {
        crate::lua::events::GameEvent::DamageApplied { slices, .. } => {
            assert!(slices[0].active_until >= slices[0].last_occurred_at);
            assert_eq!(slices[0].last_occurred_at, accepted_at);
        }
        _ => panic!("expected damage fact"),
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn canonical_receipt_rejects_divergent_ordinal_and_rollback_retains_frame() {
    crate::tests::init_config();
    let mut system = System::default();
    let mut facts = CombatFacts::new(crate::handshake::random_u64(), 1, 32, 4);
    let mut activation = [0; 16]; activation[15] = 1;
    facts.register_source(activation, 1, 0).unwrap();
    hit(&mut facts, 0, system.combat_fact_time());
    system.combat_facts = Some(facts);
    let frame = capture_fact_checkpoint(&mut system).unwrap().unwrap();
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    assert!(!frame.is_committed(&tx).await.unwrap());
    let captured = frame.batches[0].events().pop().unwrap();
    let key = SourceKey::canonical(captured.event.kind(), captured.identity, captured.ordinal);
    let (at, context) = crate::lua::events::combat::metadata(&captured.event).unwrap();
    let journal = crate::dao::progress::JournalEvent {
        source_namespace: "business".into(), source_key: key.as_str().into(),
        schema_version: crate::progress::codec::SCHEMA_VERSION, kind: captured.event.kind().into(),
        occurred_at: at, recorded_at: at, payload: crate::progress::codec::encode(&captured.event).unwrap(),
        context: serde_json::to_string(context).unwrap(), origin: context.origin.as_str().into(),
        ordinal: captured.ordinal, subject_character_id: crate::lua::events::combat::subject(&captured.event),
    };
    crate::dao::progress::insert_batch(&tx, &[journal]).await.unwrap();
    assert!(frame.is_committed(&tx).await.unwrap());
    tx.execute("UPDATE game.progress_events SET ordinal = ordinal + 1 WHERE source_namespace = 'business' AND source_key = $1", &[&key.as_str()]).await.unwrap();
    assert!(matches!(frame.is_committed(&tx).await, Err(crate::db::Error::Progress(_))));
    tx.rollback().await.unwrap();
    assert_eq!(system.combat_facts.as_ref().unwrap().batch_count(), 1);
    assert!(Arc::ptr_eq(&frame.batches[0], &capture_fact_checkpoint(&mut system).unwrap().unwrap().batches[0]));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn fact_and_condition_rollback_together_on_collision() {
    crate::tests::init_config();
    let mut system = System::default();
    let mut facts = CombatFacts::new(crate::handshake::random_u64(), 1, 32, 4);
    let mut activation = [0; 16]; activation[15] = 1;
    facts.register_source(activation, 1, 0).unwrap();
    hit(&mut facts, 0, system.combat_fact_time());
    system.combat_facts = Some(facts);
    let mut frame = capture_fact_checkpoint(&mut system).unwrap().unwrap();
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let item = crate::dao::items::creation::positioned(&tx, 1, 1, 1, [0.0; 3]).await.unwrap();
    Arc::get_mut(&mut frame).unwrap().conditions.push(combat::DirtyConditionCapture {
        item_id: item, generation: 1, revision: 1, shield: 5.0, armor_damage: 2.0, hull_damage: 1.0, sampled_at: 1,
    });
    let own = combat::ActorConditionSnapshot {
        item_id: item, generation: 1, revision: 2, saved_revision: 0, contributions: Vec::new(),
        condition: crate::services::combat::groups_health::Condition {
            stats: crate::services::combat::groups_health::ShipStats { max: [10.0; 3], resonance: [[1.0; 4]; 3], shield_tau_ms: 1.0 },
            now: [9.0, 8.0, 7.0], shield_at: 1,
        },
    };
    let savepoint = tx.transaction().await.unwrap();
    let mut outbox = Outbox::<()>::default();
    let ids = crate::services::combat::checkpoint_conditions_in(&savepoint, &mut outbox, 1, &[own], Some(&frame)).await.unwrap();
    assert_eq!(ids, vec![item]);
    crate::progress::prepare_batch(&mut outbox, &savepoint, 1).await.unwrap();
    assert!(frame.is_committed(&savepoint).await.unwrap());
    let values = crate::dao::items::dogma::overrides(&savepoint, item).await.unwrap();
    assert_eq!(values.len(), 3);
    assert_eq!(values.iter().find(|row| row.attribute_id == infinity_core::consts::dogma::ATTR_SHIELD_CHARGE).unwrap().value, 9.0);
    assert!(!frame.verified_condition_ids(&savepoint).await.unwrap().contains(&item));
    let attributes = [infinity_core::consts::dogma::ATTR_SHIELD_CHARGE, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, infinity_core::consts::dogma::ATTR_HULL_DAMAGE];
    crate::dao::items::dogma::save_conditions(&savepoint, 1, &attributes, &[(item, [5.0, 2.0, 1.0], 1)]).await.unwrap();
    assert!(frame.verified_condition_ids(&savepoint).await.unwrap().contains(&item));
    savepoint.rollback().await.unwrap();
    assert!(!frame.is_committed(&tx).await.unwrap());
    assert!(crate::dao::items::dogma::overrides(&tx, item).await.unwrap().is_empty());

    let captured = frame.batches[0].events().pop().unwrap();
    let key = SourceKey::canonical(captured.event.kind(), captured.identity, captured.ordinal);
    let (at, context) = crate::lua::events::combat::metadata(&captured.event).unwrap();
    let conflicting = crate::dao::progress::JournalEvent {
        source_namespace: "business".into(), source_key: key.as_str().into(),
        schema_version: crate::progress::codec::SCHEMA_VERSION, kind: captured.event.kind().into(),
        occurred_at: at, recorded_at: at, payload: crate::progress::codec::encode(&captured.event).unwrap(),
        context: serde_json::to_string(context).unwrap(), origin: context.origin.as_str().into(),
        ordinal: captured.ordinal + 1, subject_character_id: crate::lua::events::combat::subject(&captured.event),
    };
    crate::dao::progress::insert_batch(&tx, &[conflicting]).await.unwrap();
    let savepoint = tx.transaction().await.unwrap();
    let mut outbox = Outbox::<()>::default();
    crate::services::combat::checkpoint_conditions_in(&savepoint, &mut outbox, 1, &[], Some(&frame)).await.unwrap();
    assert!(crate::progress::prepare_batch(&mut outbox, &savepoint, 2).await.is_err());
    savepoint.rollback().await.unwrap();
    assert!(crate::dao::items::dogma::overrides(&tx, item).await.unwrap().is_empty());
    assert_eq!(system.combat_facts.as_ref().unwrap().batch_count(), 1);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn busy_gate_retains_frozen_fact_and_uses_existing_deadline() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let mut system = System::default();
    let mut facts = CombatFacts::new(7, 1, 32, 4);
    let mut activation = [0; 16]; activation[15] = 1;
    facts.register_source(activation, 1, 0).unwrap();
    hit(&mut facts, 0, system.combat_fact_time());
    system.combat_facts = Some(facts);
    let frame = capture_fact_checkpoint(&mut system).unwrap().unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    let guard = gate.clone().lock_owned().await;
    system.snapshot_context = Some(Context { tx: tx.downgrade(), gate, written: Arc::new(AtomicU64::new(0)), db });
    system.tick(1);
    let interval = crate::config::game().space.position_snapshot_secs * MILLISECONDS_PER_SECOND;
    let due = system.snapshot_due;
    system.tick(due);
    assert_eq!(system.snapshot_due, due + interval);
    assert!(Arc::ptr_eq(&frame.batches[0], &system.combat_facts.as_ref().unwrap().pending_batches()[0]));
    assert!(!system.is_cold(false));
    drop(guard);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn committed_old_frame_reconciliation_does_not_restore_old_hp() {
    crate::tests::init_config();
    let mut system = System::default();
    let mut facts = CombatFacts::new(crate::handshake::random_u64(), 1, 32, 4);
    let mut activation = [0; 16]; activation[15] = 1;
    facts.register_source(activation, 1, 0).unwrap();
    hit(&mut facts, 0, system.combat_fact_time());
    system.combat_facts = Some(facts);
    let mut frame = capture_fact_checkpoint(&mut system).unwrap().unwrap();
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let item = crate::dao::items::creation::positioned(&tx, 1, 1, 1, [0.0; 3]).await.unwrap();
    Arc::get_mut(&mut frame).unwrap().conditions.push(combat::DirtyConditionCapture {
        item_id: item, generation: 1, revision: 1, shield: 5.0, armor_damage: 2.0, hull_damage: 1.0, sampled_at: 1,
    });
    let mut outbox = Outbox::<()>::default();
    let ids = crate::services::combat::checkpoint_conditions_in(&tx, &mut outbox, 1, &[], Some(&frame)).await.unwrap();
    crate::progress::prepare_batch(&mut outbox, &tx, 1).await.unwrap();
    frame.acknowledge(&mut system, &ids.into_iter().collect());
    let attrs = [infinity_core::consts::dogma::ATTR_SHIELD_CHARGE, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, infinity_core::consts::dogma::ATTR_HULL_DAMAGE];
    crate::dao::items::dogma::save_conditions(&tx, 1, &attrs, &[(item, [3.0, 4.0, 2.0], 2)]).await.unwrap();
    if !frame.is_committed(&tx).await.unwrap() {
        let mut retry = Outbox::<()>::default();
        crate::services::combat::checkpoint_conditions_in(&tx, &mut retry, 1, &[], Some(&frame)).await.unwrap();
    }
    let values = crate::dao::items::dogma::overrides(&tx, item).await.unwrap();
    assert_eq!(values.iter().find(|row| row.attribute_id == attrs[0]).unwrap().value, 3.0);
    assert_eq!(values.iter().find(|row| row.attribute_id == attrs[1]).unwrap().value, 4.0);
    tx.rollback().await.unwrap();
}