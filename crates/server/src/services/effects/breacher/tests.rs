use super::*;
use crate::services::combat::{self, Ctx, targeting_charges};
use crate::services::combat::groups_health::ShipStats;
use crate::services::effects::{behaviors, lifecycle::Module, registry};
use crate::services::effects::weapons::missile::{launch_missile, Missile};
use crate::services::effects::space_source::PreparedCombatAttempt;
use crate::services::effects::tests::fixture::{admit, ball};
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipFixture, ShipItemSpec, ShipLocation};
use crate::space::{dbuff::TimedRuntimeInput, World};
use crate::services::CallError;
use crate::services::fit::runtime::{BREACH_CONTROL_NAMESPACE, RuntimeReceiverKey, RuntimeSourceKey};
use crate::sde::Sde;
use infinity_core::consts::{CATEGORY_DRONE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST, TYPE_CAPSULE,
    dogma::{ATTR_BREACHER_POD_ACTIVATED_DAMAGE_RECEIVED_PERCENTAGE, ATTR_VALID_TARGET_WHITELIST,
        DBUFF_BPDC_DAMAGE_RECEIVED_PERCENTAGE, EFFECT_MODULE_BONUS_BREACHER_POD_DAMAGE_CONTROL, EFFECT_USE_MISSILES}};
use infinity_core::messages;
use std::sync::Arc;

fn app(id: i64, max_damage: f64) -> DotApplication {
    DotApplication {
        id, attacker: Some(id), source_ship: id, source_type: 1, source_generation: 1,
        module: None, weapon_type: 1, illegal: false, expires_ft: i64::MAX,
        max_damage, max_hp_fraction: 1.0, generation: id as u64, sequence: 0, active: false,
    }
}

fn target(applications: Vec<DotApplication>) -> DotTarget {
    DotTarget { applications, published_fraction: None, armed: None }
}

#[test]
fn effective_tick_damage_truncates_caps_and_scales_by_resistance() {
    assert_eq!(effective_tick_damage(200.0, 0.006, 10_000.0, 1.0), 60.0);
    assert_eq!(effective_tick_damage(200.0, 0.006, 100_000.0, 1.0), 200.0);
    assert_eq!(effective_tick_damage(200.0, 0.00607, 10_000.0, 1.0), 60.0);
    assert_eq!(effective_tick_damage(200.0, 0.006, 10_000.0, 0.1), 6.0);
    assert_eq!(effective_tick_damage(f64::NAN, 0.006, 10_000.0, 1.0), 0.0);
    assert_eq!(effective_tick_damage(200.0, f64::INFINITY, 10_000.0, 1.0), 0.0);
    assert_eq!(effective_tick_damage(200.0, 0.006, -1.0, 1.0), 0.0);
    assert_eq!(effective_tick_damage(200.0, 0.006, 10_000.0, -0.5), 0.0);
}

#[test]
fn highest_application_wins_and_ties_keep_the_active_one() {
    let entry = target(vec![app(1, 100.0), app(2, 200.0), app(3, 50.0)]);
    assert_eq!(entry.select(1_000.0, 1.0), Some((1, 200.0)));
    let mut tied = target(vec![app(1, 200.0), app(2, 200.0)]);
    assert_eq!(tied.select(1_000.0, 1.0), Some((0, 200.0)));
    assert_eq!(tied.set_active(1), (true, None, Some(2)));
    assert_eq!(tied.select(1_000.0, 1.0), Some((1, 200.0)));
    assert_eq!(tied.set_active(0), (true, Some(2), Some(1)));
    assert_eq!(tied.select(1_000.0, 1.0), Some((0, 200.0)));
    assert_eq!(tied.set_active(0), (false, Some(1), Some(1)));
}

#[test]
fn expired_applications_leave_and_last_one_clears_the_target() {
    let now = crate::client::filetime_now();
    let mut entry = target(vec![app(7, 100.0), app(8, 50.0)]);
    entry.applications[0].expires_ft = now - 1;
    assert_eq!(entry.prune(now, |_| true).iter().map(|app| app.id).collect::<Vec<_>>(), vec![7]);
    assert_eq!(entry.applications.len(), 1);
    let mut entry = target(vec![app(9, 100.0)]);
    assert_eq!(entry.prune(now, |_| false).iter().map(|app| app.id).collect::<Vec<_>>(), vec![9]);
    assert!(entry.applications.is_empty());
}

/// Decode the `__builtin__.set` list inside `OnDotVictimUpdated` args.
fn dot_set(args: &[Value]) -> Option<&Vec<Value>> {
    let Some(Value::Reduce { data, .. }) = args.get(1) else { return None };
    let Value::Tuple(outer) = &**data else { return None };
    let Some(Value::Tuple(inner)) = outer.get(1) else { return None };
    match inner.first() { Some(Value::List(items)) => Some(items), _ => None }
}

/// One `_`-prefixed integer field of each `DotApplicationInfo` in the set
/// inside `OnDotVictimUpdated` args (its reduce state dict).
fn dot_field(args: &[Value], field: &[u8]) -> Vec<i64> {
    dot_set(args).map(|items| items.iter().filter_map(|item| {
        let Value::Reduce { data, .. } = item else { return None };
        let Value::Tuple(parts) = &**data else { return None };
        let Some(Value::Dict(state)) = parts.get(2) else { return None };
        state.iter().find(|(key, _)| key.as_bytes() == Some(field)).and_then(|(_, value)| value.as_int())
    }).collect()).unwrap_or_default()
}

type PilotRx = tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>;

/// Whether `messages` carries an `OnDotVictimUpdated` for `target` passing `check`.
fn saw_update(messages: &[(String, Vec<Value>)], target: i64, check: impl Fn(&[Value]) -> bool) -> bool {
    messages.iter().any(|(name, args)| name == "OnDotVictimUpdated"
        && args.first().and_then(Value::as_int) == Some(target) && check(args))
}

/// Dedicated test database, its loaded SDE and an empty world.
async fn rig() -> (Db, Arc<Sde>, Arc<World>) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    (db, sde, world)
}

/// One ship parked in system 1: created, its pilot registered, its ball admitted.
async fn parked_ship(db: &Db, world: &Arc<World>, hull: i32, x: f64, items: &[ShipItemSpec<'_>]) -> (ShipFixture, PilotRx) {
    let ship = create_ship(db, hull, ShipLocation::Space { system: 1 }, items).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(ship.character, tx);
    admit(db, world, 1, ball(ship.ship, hull, ship.character, Some(ship.character), x)).await;
    (ship, rx)
}

/// Register one pod contact on `target` in the actor and finish its combat attempt.
async fn apply_pod(world: &Arc<World>, db: &Db, source: i64, target: i64, damage: f64) -> bool {
    let (weak, db) = (Arc::downgrade(world), db.clone());
    let payload = DotPayload { duration_ms: 60_000.0, max_damage_per_tick: damage, max_hp_fraction_per_tick: 1.0 };
    world.with(1, move |s| {
        let facts = s.prepare_combat_source(source, Some(1), Some(i64::from(EFFECT_DOT_MISSILE_LAUNCHING)),
            Some(1), FactOrigin::Natural, crate::space::targeting::next_generation(), 1, 1, |_| true).unwrap();
        let registered = apply_pod_contact(s, db, weak, target, &facts, Some(1), 1, false, payload);
        s.finish_combat_attempt(&facts);
        registered
    }).await.unwrap()
}

/// Remove every fixture and flush positions.
async fn teardown(db: &Db, world: &Arc<World>, ships: Vec<ShipFixture>) {
    for ship in ships { cleanup_ship(db, Some(&**world), ship).await.unwrap(); }
    world.flush_positions().await.unwrap();
}

async fn take_notifications(rx: &mut PilotRx) -> Vec<(String, Vec<Value>)> {
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn pod_contact_registers_dot_and_first_tick_deals_flat_damage() {
    let (db, sde, world) = rig().await;
    let pod = sde.types.iter().find(|t| t.effects().iter().any(|e| e.effect_id == EFFECT_DOT_MISSILE_LAUNCHING))
        .expect("SCARAB breacher pod charge").type_id;
    let launcher = sde.types.iter().find(|t| t.attribute(ATTR_VALID_TARGET_WHITELIST).is_some_and(|v| v > 0.0))
        .expect("breacher pod launcher").type_id;
    let (hull, stats) = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP))
        .find_map(|t| {
            let stats = ShipStats::from_fit(&Fit::for_space_type(sde.clone(), t.type_id, Vec::new()), Key::Ship);
            (stats.max[0] > 250.0 && stats.max.iter().all(|v| v.is_finite() && *v > 0.0)
                && stats.resonance.iter().flatten().all(|v| v.is_finite() && *v > 0.0)
                && stats.resonance.iter().flatten().any(|v| *v < 1.0)
                && stats.shield_tau_ms.is_finite() && stats.shield_tau_ms >= 0.0)
                .then_some((t.type_id, stats))
        }).expect("hull with a resisted channel and over 250 shield");
    let system = 1;
    let (attacker, mut rx_attacker) = parked_ship(&db, &world, hull, 0.0,
        &[ShipItemSpec { type_id: launcher, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await;
    let (victim_ship, mut rx_victim) = parked_ship(&db, &world, hull, 5_000.0, &[]).await;
    let module = attacker.module();
    let weak = Arc::downgrade(&world);
    let source_ball = world.ball(system, attacker.ship).await.unwrap();
    let target_ball = world.ball(system, victim_ship.ship).await.unwrap();
    let payload = DotPayload { duration_ms: 60_000.0, max_damage_per_tick: 100.0, max_hp_fraction_per_tick: 1.0 };
    let missile = Missile { charge: pod, raw: [0.0; 4], aoe_cloud: 1.0, aoe_velocity: 1.0, drf: 1.0,
        velocity: 6_000.0, flight_time_ms: 5_000.0, weapon: launcher, illegal: false, dot: Some(payload) };
    let facts = world.with(system, move |s| s.prepare_combat_source(attacker.ship, Some(module), None,
        Some(launcher), FactOrigin::Natural, crate::space::targeting::next_generation(), 1, 1, |_| true).unwrap()).await.unwrap();
    launch_missile(db.clone(), world.clone(), system, source_ball, target_ball, attacker.character,
        Some(module), missile, None, PreparedCombatAttempt::new(facts, weak, system)).await.unwrap();
    let target_id = victim_ship.ship;
    let (now, slim) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let sampled = world.with(system, move |s| {
                let applications = &s.breacher.targets.get(&target_id)?.applications;
                assert_eq!(applications.len(), 1);
                if applications[0].sequence == 0 { return None; }
                assert_eq!(applications[0].sequence, 1, "first damage tick was not observed");
                Some((s.combat_condition_snapshot(target_id)?.condition.now, s.ball(target_id)?.slim.clone()))
            }).await.flatten();
            if let Some(sampled) = sampled { break sampled; }
            assert!(world.after(system, 100).await);
        }
    }).await.expect("first breacher damage tick");
    // A full shield sees the flat 100 in the shield layer only; the ship's
    // own resonances do not reduce it.
    assert_eq!(now, [stats.max[0] - 100.0, stats.max[1], stats.max[2]]);
    let total: f64 = stats.max.iter().sum();
    assert_eq!(slim.iter().find(|(key, _)| *key == wire::SLIM_DOT_FIELD).map(|(_, v)| v.clone()),
        Some(Value::Float(100.0 * (100.0 / total))));
    // Victim and attacker each receive the stream independently: the
    // registration update carries the new application as DOT_IDLE, and the
    // first tick's update marks exactly one application DOT_ACTIVE.
    let attacker_messages = take_notifications(&mut rx_attacker).await;
    let victim_messages = take_notifications(&mut rx_victim).await;
    for messages in [&attacker_messages, &victim_messages] {
        assert!(saw_update(messages, target_id, |args| dot_field(args, b"_activityState") == vec![DOT_IDLE]));
        assert!(saw_update(messages, target_id, |args| dot_field(args, b"_activityState") == vec![DOT_ACTIVE]));
    }
    let damage_message = |args: &[Value]| args.iter().any(|arg| matches!(arg, Value::Dict(entries)
        if entries.iter().any(|(k, v)| k.as_bytes() == Some(b"weapon".as_slice())
            && v.as_int() == Some(i64::from(launcher)))));
    let mut seen = attacker_messages.iter().chain(&victim_messages)
        .any(|(name, args)| name == "OnDamageMessage" && damage_message(args));
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while !seen {
            seen = std::iter::from_fn(|| rx_attacker.try_recv().ok().or_else(|| rx_victim.try_recv().ok()))
                .any(|(name, args)| name == "OnDamageMessage" && damage_message(&args));
            assert!(world.after(system, 50).await);
        }
    }).await.expect("damage message");
    teardown(&db, &world, vec![attacker, victim_ship]).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn source_departure_and_target_removal_tear_down_applications() {
    let (db, sde, world) = rig().await;
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).expect("any ship").type_id;
    let system = 1;
    let (attacker, mut rx_attacker) = parked_ship(&db, &world, hull, 0.0, &[]).await;
    let (victim_ship, mut rx_victim) = parked_ship(&db, &world, hull, 5_000.0, &[]).await;
    let (source_id, target_id) = (attacker.ship, victim_ship.ship);
    assert!(apply_pod(&world, &db, source_id, target_id, 100.0).await);
    assert!(world.with(system, move |s| s.breacher.targets.get(&target_id).is_some()).await.unwrap());
    world.remove_ball(system, source_id).await;
    // The source leaving the actor withdraws its application and clears the target.
    assert!(world.with(system, move |s| s.breacher.targets.get(&target_id).is_none()).await.unwrap());
    for rx in [&mut rx_attacker, &mut rx_victim] {
        assert!(saw_update(&take_notifications(rx).await, target_id,
            |args| dot_set(args).is_some_and(|items| items.is_empty())));
    }
    // Re-register on the victim's own ship, then remove the target itself.
    assert!(apply_pod(&world, &db, target_id, target_id, 100.0).await);
    world.remove_ball(system, target_id).await;
    assert!(world.with(system, move |s| s.breacher.targets.is_empty()).await.unwrap());
    teardown(&db, &world, vec![attacker, victim_ship]).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn weaker_application_leaving_publishes_the_surviving_set() {
    let (db, sde, world) = rig().await;
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).expect("any ship").type_id;
    let system = 1;
    let (strong, mut rx_strong) = parked_ship(&db, &world, hull, 0.0, &[]).await;
    let (weak_source, mut rx_weak) = parked_ship(&db, &world, hull, 2_000.0, &[]).await;
    let (victim_ship, mut rx_victim) = parked_ship(&db, &world, hull, 5_000.0, &[]).await;
    let target_id = victim_ship.ship;
    // The stronger application survives; the weaker source leaves the actor.
    assert!(apply_pod(&world, &db, strong.ship, target_id, 200.0).await);
    assert!(apply_pod(&world, &db, weak_source.ship, target_id, 50.0).await);
    assert_eq!(world.with(system, move |s| s.breacher.targets.get(&target_id)
        .map(|entry| entry.applications.len())).await.unwrap(), Some(2));
    world.remove_ball(system, weak_source.ship).await;
    assert_eq!(world.with(system, move |s| s.breacher.targets.get(&target_id)
        .map(|entry| entry.applications.iter().map(|app| app.source_ship).collect::<Vec<_>>())).await.unwrap(),
        Some(vec![strong.ship]));
    // The surviving set reaches the victim, the attacker still on it and the
    // attacker whose application just left.
    for rx in [&mut rx_victim, &mut rx_strong, &mut rx_weak] {
        assert!(saw_update(&take_notifications(rx).await, target_id,
            |args| dot_field(args, b"_attackerID") == vec![strong.character]));
    }
    teardown(&db, &world, vec![strong, weak_source, victim_ship]).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn launcher_refuses_targets_outside_the_whitelist() {
    let (db, sde, world) = rig().await;
    let (launcher, list) = sde.types.iter().find_map(|t| t.attribute(ATTR_VALID_TARGET_WHITELIST)
        .filter(|v| *v > 0.0).map(|list| (t.type_id, list as i64))).expect("breacher launcher");
    let drone_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_DRONE)
        && t.published == Some(true)).expect("any drone");
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && sde.typelists.contains(list, t.type_id, t.group_id, t.category_id)).expect("whitelisted hull").type_id;
    let f = create_ship(&db, hull, ShipLocation::Space { system: 1 },
        &[ShipItemSpec { type_id: launcher, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(f.character, tx.clone());
    admit(&db, &world, 1, ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
    world.add_ship(1, ball(-1, drone_type.type_id, 1, None, 5_000.0)).await.unwrap();
    world.add_ship(1, ball(-2, hull, 1, None, 6_000.0)).await.unwrap();
    world.add_ship(1, ball(-3, TYPE_CAPSULE, 1, None, 7_000.0)).await.unwrap();
    let cx = Ctx { db: db.clone(), world: world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
    let fit = Fit::load(&db, sde.clone(), f.character, f.ship).await.unwrap();
    let effect = registry::resolve(&sde, EFFECT_USE_MISSILES).expect("useMissiles effect");
    let activation = |target| behaviors::Activation {
        effect: effect.clone(), module: Module { id: module_id, type_id: launcher, flag: FLAG_HI_SLOT_FIRST }, target: Some(target), aim: None };
    for (target_id, label, whitelisted) in [(-1, "drone", false), (-3, "capsule", false), (-2, "ship", true)] {
        targeting_charges::add_target_ctx(&cx, target_id, true).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !targeting_charges::targets_of(cx.char_id).contains(&target_id) {
                assert!(world.after(1, 100).await);
            }
        }).await.expect("whitelist fixture target locked");
        let refused = behaviors::on_activate(&cx, &fit, &activation(target_id)).await.expect_err(label);
        let CallError::User { msg, dict } = refused else { panic!("{label} refusal must be a user error") };
        if whitelisted {
            // A whitelisted ship passes the gate and fails later at charge admission.
            assert!(msg != messages::MSG_CUSTOM_NOTIFY || !dict.iter().any(|(k, v)| k == "notify"
                && *v == Value::unicode(messages::effects::BREACHER_INVALID_TARGET)));
        } else {
            assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
            assert!(dict.iter().any(|(k, v)| k == "notify"
                && *v == Value::unicode(messages::effects::BREACHER_INVALID_TARGET)));
        }
        targeting_charges::clear_locks_on(&world, 1, f.ship, f.character);
    }
    for target_id in [-1, -2, -3] { world.remove_ball(1, target_id).await; combat::forget_ship(target_id); }
    teardown(&db, &world, vec![f]).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn dot_tick_honours_breacher_pod_damage_resistance() {
    let (db, sde, world) = rig().await;
    let (hull, stats) = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP))
        .find_map(|t| { let stats = ShipStats::from_fit(&Fit::for_space_type(sde.clone(), t.type_id, Vec::new()), Key::Ship);
            (stats.max[0] > 250.0 && stats.max.iter().all(|v| v.is_finite() && *v > 0.0)).then_some((t.type_id, stats)) })
        .expect("hull with enough shield");
    let (attacker, _rx) = parked_ship(&db, &world, hull, 0.0, &[]).await;
    let (victim, _rx) = parked_ship(&db, &world, hull, 5_000.0, &[]).await;
    let (target, source) = (victim.ship, attacker.ship);
    let module_type = sde.types.iter().find(|t| t.effects().iter().any(|e| e.effect_id == EFFECT_MODULE_BONUS_BREACHER_POD_DAMAGE_CONTROL)).unwrap().type_id;
    let value = sde.types.get(module_type).and_then(|t| t.attribute(ATTR_BREACHER_POD_ACTIVATED_DAMAGE_RECEIVED_PERCENTAGE)).unwrap();
    let accepted = world.with(1, move |s| s.land_runtime_inputs(&[TimedRuntimeInput { receiver: RuntimeReceiverKey::Ship(target),
        collection_id: DBUFF_BPDC_DAMAGE_RECEIVED_PERCENTAGE,
        source: RuntimeSourceKey { producer: BREACH_CONTROL_NAMESPACE, source_id: 1, incarnation: 1 },
        value, expires_at_ms: s.now_ms().saturating_add(60_000) }])).await.unwrap();
    assert_eq!(accepted, 1);
    let resistance = world.with(1, move |s| s.with_combat_fit(target, |fit, key, _| fit.value(key, ATTR_BREACHER_POD_DAMAGE_RESISTANCE))).await.flatten().expect("resident fit");
    assert!((resistance - (1.0 + value / crate::services::fit::PERCENT_SCALE)).abs() < 0.01, "collection must fold through the evaluator");
    assert!(apply_pod(&world, &db, source, target, 100.0).await);
    let now = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let now = world.with(1, move |s| s.combat_condition_snapshot(target).map(|c| c.condition.now)).await.flatten();
            if let Some(now) = now.filter(|now| *now != stats.max) { break now; }
            assert!(world.after(1, 100).await);
        }
    }).await.expect("damage tick");
    assert_eq!(now, [stats.max[0] - 100.0 * resistance, stats.max[1], stats.max[2]]);
    teardown(&db, &world, vec![attacker, victim]).await;
}
