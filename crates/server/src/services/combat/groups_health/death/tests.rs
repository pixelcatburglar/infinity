//! Immutable NPC death identity and lethal layer payload tests.
use super::*;
use crate::lua::events::{combat::{CombatEntity, ConflictKind, HpLayer}, resources::{FactContext, FactOrigin}};
use crate::space::combat_facts::{CombatSource, PendingDamage};
use crate::lua::events::combat::amount::ExactBinarySum;

fn entity(id: i64, character: Option<i64>) -> CombatEntity {
    CombatEntity { item_id: id, type_id: 10, incarnation: [id as u8; 16], character_id: character,
        corporation_id: None, alliance_id: None, faction_id: None }
}
fn context() -> FactContext {
    FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None,
        ship_type_id: None, system_id: Some(1), station_id: None, origin: FactOrigin::Natural }
}
fn snapshot() -> FirstLethalSnapshot {
    let victim = entity(2, None);
    FirstLethalSnapshot { token: 3, item_id: 2, condition: Condition {
        stats: ShipStats { max: [10.0; 3], resonance: [[1.0; 4]; 3], shield_tau_ms: 0.0 },
        now: [10.0; 3], shield_at: 1 }, contributions: vec![], undo_contributions: vec![],
        source: Default::default(), position: [0.0; 3], identity: None, owner_id: 1, corp_id: 1,
        is_drone: false, is_npc: true, expected_online_session: None, kind: Some(10), lethal_at: 10,
        generation: 4, before_revision: 1, victim, pending_damage: None, victim_context: context(), conflict: ConflictKind::Npc }
}
#[test]
fn npc_killer_requires_real_frozen_controller() {
    let mut s = snapshot();
    let npc = CombatSource { actual_actor_character: None, entity: entity(5, None), context: context(), source_is_npc: true,
        controller_incarnation: None, module_id: None, effect_id: None, weapon_type_id: None, activation: [5; 16], sequence: 1 };
    s.source.facts = Some(npc);
    assert!(matches!(death_event(&s, 1, 0.0).unwrap(), GameEvent::NpcDestroyed { killer_id: None, killer: None, .. }));
    let mut drone = npc;
    drone.source_is_npc = false;
    drone.actual_actor_character = Some(9);
    drone.context.ship_id = Some(8);
    drone.context.ship_type_id = Some(10);
    drone.controller_incarnation = Some([8; 16]);
    s.source.facts = Some(drone);
    assert!(matches!(death_event(&s, 1, 1.0).unwrap(), GameEvent::NpcDestroyed { killer_id: Some(9), killer: Some(killer), .. } if killer.item_id == 8 && killer.character_id == Some(9)));
    assert!(death_event(&s, 1, f64::NAN).is_err());
}
#[test]
fn lethal_events_are_layer_only_and_keep_original_settlement() {
    let mut s = snapshot();
    let source = CombatSource { actual_actor_character: None, entity: entity(5, None), context: context(), source_is_npc: true,
        controller_incarnation: None, module_id: None, effect_id: None, weapon_type_id: None, activation: [5; 16], sequence: 7 };
    let mut amount = ExactBinarySum::default();
    assert!(amount.try_add(0.5));
    s.pending_damage = Some(PendingDamage { settlement: crate::lua::events::combat::SettlementId {
        source_instance: source.activation, generation: 3, sequence: 7, target_id: 2, target_incarnation: s.victim.incarnation },
        source, target: s.victim, occurred_at: 10, amounts: [amount.clone(), ExactBinarySum::default(), amount],
        slice: 4, qualification_epoch: 5, boundary_at: 6 });
    let mut outbox: Outbox = Outbox::default();
    s.pending_damage.as_ref().unwrap().append(&mut outbox);
    let (_, events) = crate::db::outbox::tests::split(outbox);
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], GameEvent::DamageApplied { layer: HpLayer::Shield, slices, .. }
        if slices[0].index == 4 && slices[0].qualification_epoch == 5 && slices[0].boundary_at == 6 && slices[0].active_until == 10));
    assert!(matches!(&events[1], GameEvent::DamageApplied { layer: HpLayer::Hull, .. }));
    let key = death_key(&s).as_str().to_owned();
    s.token += 1;
    assert_ne!(key, death_key(&s).as_str());
}

#[test]
fn rewards_are_enabled_only_for_fresh_committed_death_source() {
    use crate::progress::AfterCommit;
    let registry = crate::progress::Registry::default();
    let mut effect = DeathEffect { key: "NpcDestroyed:one".into(), fresh: false };
    let mut committed = std::collections::HashSet::new();
    effect.after_commit(&registry, &committed);
    assert!(!effect.fresh);
    committed.insert(effect.key.clone());
    effect.after_commit(&registry, &committed);
    assert!(effect.fresh);
    committed.clear();
    effect.after_commit(&registry, &committed);
    assert!(!effect.fresh);
}

async fn fault_scenario(commit_before_retry: bool, known_rollback: bool) {
    use std::sync::atomic::{AtomicBool, Ordering};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|ty| {
        ty.category_id == Some(infinity_core::consts::CATEGORY_ENTITY)
            && ty.group_id.and_then(|group| sde.wrecks.get(group, ty.race_id.unwrap_or(0))).is_none()
            && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP,
                ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE]
                .iter().all(|&id| ty.attribute(id).is_some_and(|value| value.is_finite() && value > 0.0))
    }).expect("SDE requires an NPC without a wreck and with positive defenses").type_id;
    let system = sde.map.systems().next().expect("SDE requires a system").solar_system_id;
    let item = 9_000_000_000_000_i64 + (crate::handshake::random_u64() % 1_000_000_000) as i64;
    let world = World::new(db.clone(), sde, 1);
    world.add_ship(system, Ball { id: item, type_id: ty, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }).await.unwrap();
    let cleared = Arc::new(AtomicBool::new(false));
    let flag = cleared.clone();
    let outcome = world.with(system, move |s| {
        assert!(s.add_npc(item, crate::space::NpcBrain::new(None,
            crate::space::NpcConfig { fly_range: 1.0, aggro_range: 1.0, rate_of_fire_ms: 1.0,
                scan_resolution: 1.0, repairs: (0.0, 0.0, 1.0) }, Arc::new(|_, _| Some(Box::pin(async {}))))));
        s.spawn_group([item], move |_| { flag.store(true, Ordering::SeqCst); });
        let facts = s.prepare_body_combat_source(item, None, None, None, FactOrigin::Natural, |s| s.is_npc(item)).unwrap();
        s.apply_damage(item, [1e30, 0.0, 0.0, 0.0], crate::services::combat::damage::DamageSource {
            facts: Some(facts), ..Default::default()
        }, false).unwrap()
    }).await.unwrap().expect("test hit must be admitted");
    let snapshot = outcome.first_lethal.expect("test hit must be lethal");
    let key = death_key(&snapshot).as_str().to_owned();
    let name = format!("npc_death_fault_{item}");
    let function = format!("game.{name}_fn");
    let mut function_created = false;
    let mut trigger_created = false;
    let scenario = crate::tests::catch_scenario(async {
        let code = if known_rollback { " USING ERRCODE = '40001'" } else { "" };
        db.execute(&format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.source_key = '{}' THEN RAISE EXCEPTION 'NPC death fixture failure'{code}; END IF; RETURN NEW; END $$", key), &[]).await.unwrap();
        function_created = true;
        db.execute(&format!("CREATE TRIGGER {name} BEFORE INSERT ON game.progress_events FOR EACH ROW EXECUTE FUNCTION {function}()"), &[]).await.unwrap();
        trigger_created = true;
        assert!(settle_npc_loss(db.clone(), world.clone(), system, snapshot.clone(), outcome.new).await.is_err());
        if known_rollback {
            let restored = world.with(system, move |s| (s.ball(item).is_some(), s.combat_phase(item), s.combat_condition_snapshot(item))).await.unwrap();
            assert!(restored.0);
            assert_eq!(restored.1, Some(crate::space::combat::CombatPhase::Live));
            let condition = restored.2.unwrap();
            assert_eq!(condition.condition.now, snapshot.condition.now);
            assert_eq!(condition.contributions.len(), snapshot.undo_contributions.len());
            assert!(!cleared.load(Ordering::SeqCst));
            assert!(crate::dao::progress::source(&db, "business", &key).await.unwrap().is_none());
            return;
        }
        assert!(world.with(system, move |s| s.ball(item).is_some()
            && matches!(s.combat_phase(item), Some(crate::space::combat::CombatPhase::Dying { .. }))).await.unwrap());
        assert!(!cleared.load(Ordering::SeqCst));
        assert!(crate::dao::progress::source(&db, "business", &key).await.unwrap().is_none());
        let gate = world.lock_condition_snapshot(system).await.unwrap();
        db.execute(&format!("DROP TRIGGER {name} ON game.progress_events"), &[]).await.unwrap();
        trigger_created = false;
        if commit_before_retry {
            let token = snapshot.token;
            let generation = snapshot.generation;
            let frozen = world.with(system, move |s| s.npc_death_context(item, token, generation)).await.unwrap().unwrap();
            db.transaction(
                |_, outbox: &mut Outbox| {
                    let frozen = frozen.clone();
                    let snapshot = snapshot.clone();
                    Box::pin(async move {
                        snapshot.pending_damage.as_ref().unwrap().append(outbox);
                        outbox.event_from(death_key(&snapshot), frozen.event.clone());
                        Ok::<_, CallError>(())
                    })
                },
                |(), outbox| async { dispatch_world((), outbox, &world) },
            ).await.unwrap();
            assert!(world.with(system, move |s| s.ball(item).is_some()).await.unwrap());
            assert!(!cleared.load(Ordering::SeqCst));
        }
        drop(gate);
        settle_npc_loss(db.clone(), world.clone(), system, snapshot.clone(), outcome.new).await.unwrap();
        assert!(crate::dao::progress::source(&db, "business", &key).await.unwrap().is_some());
        assert!(!world.with(system, move |s| s.ball(item).is_some()).await.unwrap());
        assert!(cleared.load(Ordering::SeqCst));
    }).await;
    if trigger_created { db.execute(&format!("DROP TRIGGER {name} ON game.progress_events"), &[]).await.unwrap(); }
    if function_created { db.execute(&format!("DROP FUNCTION {function}()"), &[]).await.unwrap(); }
    world.remove_ball(system, item).await;
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE source_namespace='business' AND (source_key=$1 OR (kind='DamageApplied' AND (payload->'DamageApplied'->'target'->>'item_id')::bigint=$2)))", &[&key, &item]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE source_namespace='business' AND (source_key=$1 OR (kind='DamageApplied' AND (payload->'DamageApplied'->'target'->>'item_id')::bigint=$2))", &[&key, &item]).await.unwrap();
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn journal_failure_keeps_npc_dying_until_retry_commits() {
    fault_scenario(false, false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn committed_tombstone_recovers_without_native_ack() {
    fault_scenario(true, false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn definite_serialization_rollback_restores_npc_condition() {
    fault_scenario(false, true).await;
}
