//! Immutable NPC death identity and lethal layer payload tests.
use super::*;
use crate::lua::events::{combat::{CombatEntity, ConflictKind, HpLayer}, resources::{FactContext, FactOrigin}};
use crate::space::combat_facts::{CombatSource, PendingDamage};
use crate::lua::events::combat::amount::ExactBinarySum;
use crate::db::executor::Executor;
use crate::db::outbox::dispatch_world;

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
        npc_security_multiplier: None, controller_incarnation: None, module_id: None, effect_id: None, weapon_type_id: None, activation: [5; 16], sequence: 1 };
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
        npc_security_multiplier: None, controller_incarnation: None, module_id: None, effect_id: None, weapon_type_id: None, activation: [5; 16], sequence: 7 };
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

const WAIT: std::time::Duration = std::time::Duration::from_secs(30);

fn ball(id: i64, type_id: i32, pilot: Option<i64>) -> Ball {
    Ball { id, type_id, owner_id: pilot.unwrap_or(1), corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

async fn fault_scenario(commit_before_retry: bool, known_rollback: bool) {
    use std::sync::atomic::{AtomicBool, Ordering};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let registry = crate::progress::tests::fixture_registry(&db, crate::progress::Registry::new(vec![], vec![
        crate::services::sovereignty::indices::progress::registration(),
    ]).unwrap()).await;
    let db = db.with_progress_registry(registry);
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
    // Real index path: publish the policy, register the consumer and install
    // the capability before the NPC dies so its event lands inside coverage.
    let mut indices = if commit_before_retry && !known_rollback {
        assert!(crate::dao::sovereignty::development::read(&db, system).await.unwrap().is_none());
        let fixture = crate::progress::tests::indices_host(&db, world.sde(), &world).await;
        let history = crate::services::sovereignty::indices::activation::load(&fixture.policies).unwrap();
        crate::space::sovereignty::indices::install_policy(&world, crate::services::sovereignty::indices::model::IndexCapability::Ready(history));
        let (notify_tx, notifications) = tokio::sync::mpsc::unbounded_channel();
        world.register_pilot(item + 1, notify_tx);
        world.add_ship(system, Ball { owner_id: 1, free: true, interactive: true, ..ball(item + 2, ty, Some(item + 1)) }).await.unwrap();
        Some((fixture, notifications))
    } else { None };
    world.add_ship(system, ball(item, ty, None)).await.unwrap();
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
        db.uncached().execute(&format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.source_key = '{}' THEN RAISE EXCEPTION 'NPC death fixture failure'{code}; END IF; RETURN NEW; END $$", key), &[]).await.unwrap();
        function_created = true;
        db.uncached().execute(&format!("CREATE TRIGGER {name} BEFORE INSERT ON game.progress_events FOR EACH ROW EXECUTE FUNCTION {function}()"), &[]).await.unwrap();
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
        db.uncached().execute(&format!("DROP TRIGGER {name} ON game.progress_events"), &[]).await.unwrap();
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
        let death_event = crate::dao::progress::source(&db, "business", &key).await.unwrap().expect("committed death event");
        assert!(!world.with(system, move |s| s.ball(item).is_some()).await.unwrap());
        assert!(cleared.load(Ordering::SeqCst));
        if let Some((fixture, notifications)) = indices.as_mut() {
            crate::progress::tests::drain_indices(&db, world.sde(), &world, fixture.policies.clone(), &[death_event.event_id]).await;
            let row = crate::dao::sovereignty::development::read(&db, system).await.unwrap().expect("development row");
            assert!(row.military_points > 0.0 && row.military_points <= 100_000.0 && row.industrial_points == 0.0);
            let args = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let (event, args) = notifications.recv().await.expect("notice channel closed");
                    if event == "OnSolarSystemDevIndexChanged" { break args; }
                }
            }).await.expect("index invalidation notice");
            assert_eq!(args, vec![marshal::Value::Int(system)]);
        }
    }).await;
    if let Some((fixture, _)) = &indices {
        world.remove_ball(system, item + 2).await;
        db.execute("DELETE FROM game.sov_development_checkpoints WHERE system_id=$1 AND activation_ordinal=$2", &[&system, &fixture.activation.ordinal]).await.unwrap();
        db.execute("DELETE FROM game.sov_development WHERE system_id=$1 AND activation_ordinal=$2", &[&system, &fixture.activation.ordinal]).await.unwrap();
        db.execute("DELETE FROM game.lua_content_activations WHERE ordinal=$1", &[&fixture.activation.ordinal]).await.unwrap();
        db.execute("DELETE FROM game.lua_content_versions WHERE namespace=$1 AND version=$2", &[&fixture.activation.namespace, &fixture.activation.version]).await.unwrap();
    }
    if trigger_created { db.uncached().execute(&format!("DROP TRIGGER {name} ON game.progress_events"), &[]).await.unwrap(); }
    if function_created { db.uncached().execute(&format!("DROP FUNCTION {function}()"), &[]).await.unwrap(); }
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

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn npc_death_releases_snapshot_gate_before_rewards() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let multiplier = crate::config::game().combat.bounty_multiplier;
    assert!(multiplier.is_finite() && multiplier > 0.0, "test requires a positive bounty multiplier");
    let defenses = [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP,
        ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE];
    let npc_row = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_ENTITY)
        && ty.attribute(ATTR_ENTITY_KILL_BOUNTY).is_some_and(|v| v.is_finite() && v > 0.0)
        && ty.group_id.and_then(|g| sde.wrecks.get(g, ty.race_id.unwrap_or(0))).is_some()
        && defenses.iter().all(|&a| ty.attribute(a).is_some_and(|v| v.is_finite() && v > 0.0))
        && super::super::bounty_payout(ty.attribute(ATTR_ENTITY_KILL_BOUNTY).unwrap(), multiplier)
            .is_some_and(|payout| crate::services::market::round2(payout) > 0.0))
        .expect("SDE requires a bounty NPC with a wreck and EM defenses");
    let wreck_type = npc_row.group_id.and_then(|g| sde.wrecks.get(g, npc_row.race_id.unwrap_or(0))).unwrap();
    let expected = crate::isk::IskAmount::from_isk(crate::services::market::round2(
        super::super::bounty_payout(npc_row.attribute(ATTR_ENTITY_KILL_BOUNTY).unwrap(), multiplier).unwrap())).unwrap();
    let ship_type = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && ty.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v.is_finite() && v > 0.0))
        .expect("SDE requires a ship hull with structure").type_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (owner, system) = (fixture.character, 1_000_000_000_000_i64 + fixture.character);
    let fast_talk = sde.types.iter().find(|skill| skill.category_id == Some(infinity_core::consts::CATEGORY_SKILL)
        && skill.modifiers(&sde.dogma, 359).any(|(_, modifier)| modifier.domain == "charID" && modifier.modified == 359))
        .expect("SDE Fast Talk modifies character attribute 359");
    let rank = fast_talk.attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT).expect("Fast Talk rank");
    let ship = crate::dao::items::creation::positioned(&db, ship_type, owner, system, [0.0; 3]).await.unwrap();
    let npc = 9_000_000_000_000_i64 + fixture.character;
    let world = World::new(db.clone(), sde.clone(), 1);
    world.add_ship(system, ball(ship, ship_type, Some(owner))).await.unwrap();
    let untrained_fit = Fit::load(&db, sde.clone(), owner, ship).await.ok().expect("load untrained fit");
    let untrained_value = untrained_fit.value(crate::services::fit::Key::Char, 359);
    crate::dao::skills::training::set_level(&db, owner, fast_talk.type_id, 5, crate::services::skills::formulas::sp_for_level(rank, 5)).await.unwrap();
    let fit = Fit::load(&db, sde.clone(), owner, ship).await.ok().expect("load trained fit");
    let trained_value = fit.value(crate::services::fit::Key::Char, 359);
    assert!(fit.value(crate::services::fit::Key::Skill(fast_talk.type_id), infinity_core::consts::dogma::ATTR_SKILL_LEVEL) == 5.0 && trained_value > untrained_value, "level 5 must exceed level 0");
    let condition = crate::services::combat::prepare_condition_with_fit(&db, ship, &fit).await.ok().expect("load attacker condition");
    let published = world.with(system, move |s| {
        s.install_condition(ship, condition.into_condition(), fit);
        s.with_combat_fit(ship, |fit, _, _| fit.value(crate::services::fit::Key::Char, 359))
    }).await.unwrap();
    assert_eq!(published, Some(trained_value));
    world.add_ship(system, ball(npc, npc_row.type_id, None)).await.unwrap();
    let (cleared_tx, cleared_rx) = tokio::sync::oneshot::channel::<()>();
    let installed = world.with(system, move |s| {
        let brain = crate::space::NpcBrain::new(None, crate::space::NpcConfig { fly_range: 1.0, aggro_range: 1.0,
            rate_of_fire_ms: 1.0, scan_resolution: 1.0, repairs: (0.0, 0.0, 1.0) }, Arc::new(|_, _| Some(Box::pin(async {}))));
        let ok = s.add_npc(npc, brain);
        if ok { s.spawn_group([npc], move |_| { let _ = cleared_tx.send(()); }); }
        ok
    }).await.unwrap();
    assert!(installed);
    let mut task = None;
    let mut key = String::new();
    let scenario = crate::tests::catch_scenario(async {
        let market = crate::services::market::lock().await;
        let outcome = world.with(system, move |s| {
            let generation = s.combat_membership_generation(ship).expect("attacker combat membership");
            let facts = s.prepare_combat_source(ship, None, None, Some(ship_type), FactOrigin::Natural, generation, 1, 1,
                move |s| s.pilot_ball(owner).is_some_and(|b| b.id == ship)).expect("real attacker source");
            assert_eq!(facts.npc_security_multiplier.unwrap().value(), trained_value);
            let mut changed = crate::services::fit::character::CharacterInputs::default();
            changed.attributes = Some(vec![(359, untrained_value)]);
            assert!(s.publish_character_inputs(ship, owner, generation, &changed));
            assert_eq!(s.with_combat_fit(ship, |fit, _, _| fit.value(crate::services::fit::Key::Char, 359)), Some(untrained_value));
            s.apply_damage(npc, [1e30, 0.0, 0.0, 0.0], crate::services::combat::damage::DamageSource {
                attacker_id: Some(owner), attacker_ball_id: Some(ship), source_ship_id: Some(ship),
                source_ship_type: Some(ship_type), facts: Some(facts), ..Default::default() }, false).unwrap()
        }).await.unwrap().expect("lethal hit admitted");
        let snapshot = outcome.first_lethal.expect("first lethal snapshot");
        assert_eq!(snapshot.source.facts.and_then(|f| f.npc_security_multiplier).unwrap().value(), trained_value);
        let expected_gain = crate::npc_security::SecurityGain::for_kill(crate::config::game().crimewatch.security_gain_npc_kill, snapshot.source.facts.unwrap().npc_security_multiplier.unwrap());
        key = death_key(&snapshot).as_str().to_owned();
        task = Some(tokio::spawn(settle_npc_loss(db.clone(), world.clone(), system, snapshot.clone(), outcome.new)));
        tokio::time::timeout(WAIT, cleared_rx).await.expect("NPC cleared signal").unwrap();
        assert!(world.with(system, move |s| s.ball(npc).is_none() && s.combat_phase(npc).is_none()).await.unwrap());
        assert!(crate::dao::progress::source(&db, "business", key.as_str()).await.unwrap().is_some());
        let gate = tokio::time::timeout(WAIT, world.lock_condition_snapshot(system)).await.expect("snapshot gate while economy lock held").unwrap();
        drop(gate);
        assert!(crate::dao::wallets::characters::balance(&db, owner).await.unwrap().is_zero());
        drop(market);
        let joined = tokio::time::timeout(WAIT, task.as_mut().unwrap()).await;
        if joined.is_ok() { task.take(); }
        joined.expect("death settlement joins").unwrap().unwrap();
        world.flush_positions().await.unwrap();
        let started: i64 = db.query_one("SELECT batch_started_at FROM game.npc_bounty_pools WHERE character_id=$1", &[&owner]).await.unwrap().get("batch_started_at");
        let stored = db.query_one("SELECT security_gain_log, legacy_kill_count FROM game.npc_bounty_pools WHERE character_id=$1", &[&owner]).await.unwrap();
        let stored_gain = crate::npc_security::SecurityGain::from_storage(stored.get("security_gain_log")).unwrap();
        let legacy: i64 = stored.get("legacy_kill_count");
        assert_eq!((stored_gain, legacy), (expected_gain, 0));
        world.with(system, move |s| { s.remove_ball(ship); }).await.unwrap();
        crate::services::bounty::npc::settle_due(&db, &world, (owner, started), i64::MAX / 4).await.unwrap();
        assert_eq!(crate::dao::wallets::characters::balance(&db, owner).await.unwrap(), expected);
        let security: f64 = db.query_one("SELECT security_status FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("security_status");
        let expected_security = 10.0 * expected_gain.fraction();
        assert!((security - expected_security).abs() <= 1e-9, "payout repriced frozen gain: {security} vs {expected_security}");
        // The natural wreck is transient actor state: exactly one ball bound
        // to the launcher, and neither the root nor its loot writes a row.
        let wrecks = world.with(system, move |s| s.balls().filter(|b| b.type_id == wreck_type)
            .map(|b| (b.id, b.slim.clone())).collect::<Vec<_>>()).await.unwrap();
        let [wreck] = wrecks.as_slice() else { panic!("expected exactly one owned wreck") };
        let (wreck, slim) = (wreck.0, &wreck.1);
        assert!(slim.iter().any(|(k, v)| *k == "launcherID" && *v == Value::Int(npc)));
        assert_eq!(db.query_one("SELECT count(*) AS c FROM game.items WHERE item_id=$1 OR location_id=$1",
            &[&wreck]).await.unwrap().get::<_, i64>("c"), 0);
        settle_npc_loss(db.clone(), world.clone(), system, snapshot, outcome.new).await.unwrap();
        assert_eq!(crate::dao::wallets::characters::balance(&db, owner).await.unwrap(), expected);
        let again = world.with(system, move |s| s.balls().filter(|b| b.type_id == wreck_type)
            .map(|b| b.id).collect::<Vec<_>>()).await.unwrap();
        assert_eq!(again, vec![wreck], "a repeated finalize never re-publishes the same draft");
    }).await;
    if let Some(mut handle) = task {
        if tokio::time::timeout(WAIT, &mut handle).await.is_err() { handle.abort(); let _ = handle.await; }
    }
    if let Some(ids) = world.with(system, |s| s.balls().map(|b| b.id).collect::<Vec<_>>()).await {
        for id in ids { world.remove_ball(system, id).await; crate::services::combat::forget_ship(id); }
    }
    world.flush_positions().await.unwrap();
    db.execute("DELETE FROM game.map_events WHERE solar_system_id=$1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.npc_bounty_pools WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id LIKE 'npc-bounty:' || $1::text || ':%'", &[&owner.to_string()]).await.unwrap();
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE source_namespace='business' AND (source_key=$1 OR (kind='DamageApplied' AND (payload->'DamageApplied'->'target'->>'item_id')::bigint=$2)))", &[&key, &npc]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE source_namespace='business' AND (source_key=$1 OR (kind='DamageApplied' AND (payload->'DamageApplied'->'target'->>'item_id')::bigint=$2))", &[&key, &npc]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
}
