use super::*;
use super::missile::{launch_missile, Missile};
use crate::space::System;
use crate::dao::items::creation;
use infinity_core::consts::dogma::{ATTR_AOE_CLOUD_SIZE, ATTR_AOE_VELOCITY};

use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipLocation};

fn ball() -> Ball {
    Ball {
        id: 9_500_000_000_000, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [1.0, 2.0, 3.0], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [1.0, 2.0, 3.0], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: false,
        massive: true, warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

fn missile_charge(sde: &Sde) -> i32 {
    sde.types.iter().find(|t| t.type_id > 0 && t.published == Some(true)
        && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)
        && t.attribute(ATTR_AOE_CLOUD_SIZE).is_some_and(|v| v > 0.0)
        && t.attribute(ATTR_AOE_VELOCITY).is_some_and(|v| v > 0.0)
        && ATTR_DAMAGE.iter().any(|&a| t.attribute(a).is_some_and(|v| v > 0.0)))
        .expect("published damaging missile charge").type_id
}

async fn attempt(world: &Arc<World>, system: i64, source: i64, module: Option<i64>, weapon: i32) -> PreparedCombatAttempt {
    let generation = crate::space::targeting::next_generation();
    let weak = Arc::downgrade(world);
    world.with(system, move |s| {
        let facts = s.prepare_combat_source(source, module, None, Some(weapon),
            crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
        PreparedCombatAttempt::new(facts, weak, system)
    }).await.unwrap()
}

async fn retire(world: Arc<World>, db: &Db, f: crate::services::combat::tests::remote_repair_db::Fixture) {
    world.unregister_pilot_generation(f.source, None);
    world.remove_ball(1, f.ship).await;
    world.remove_ball(1, f.victim).await;
    world.flush_positions().await.unwrap();
    crate::services::combat::forget_ship(f.ship);
    crate::services::combat::forget_ship(f.victim);
    drop(world);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&&[f.module, f.ship, f.victim][..]]).await.unwrap();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_progress(tx, &[f.account, f.source, f.target], &[f.source, f.target]).await
    }), |(), ()| async { Ok(()) }).await.unwrap();
    crate::services::combat::tests::remote_repair_db::cleanup(db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn missile_impact_capacity_notifies_and_fences_activation_generation() {
    use crate::services::combat::{self, groups_health::{Condition, ShipStats}};
    use std::sync::atomic::AtomicBool;

    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let (ship_type, stats) = sde.types.iter().filter(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP))
        .find_map(|t| {
            let fit = Fit::for_space_type(sde.clone(), t.type_id, Vec::new());
            let stats = ShipStats::from_fit(&fit, Key::Ship);
            (stats.max[0].is_finite() && stats.max[0] > 10.0
                && stats.resonance[0][0].is_finite() && stats.resonance[0][0] > 0.0
                && stats.max.iter().all(|v| v.is_finite() && *v >= 0.0)
                && stats.resonance.iter().flatten().all(|v| v.is_finite() && *v >= 0.0)
                && stats.shield_tau_ms.is_finite() && stats.shield_tau_ms >= 0.0)
                .then_some((t.type_id, stats))
        }).unwrap();
    let charge = missile_charge(&sde);
    let system = -31_004;
    let fixture = create_ship(&db, ship_type, ShipLocation::Space { system }, &[]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(fixture.character, tx);
    let mut source = ball();
    source.id = fixture.ship;
    source.type_id = ship_type;
    source.char_id = Some(fixture.character);
    source.owner_id = fixture.character;
    source.pos = [0.0; 3];
    let mut target = ball();
    target.id = NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed);
    target.type_id = ship_type;
    target.pos = [1_000.0, 0.0, 0.0];
    target.mode = crate::space::MODE_RIGID;
    target.signature_radius = Some(100.0);
    world.add_ship(system, source.clone()).await.unwrap();
    world.add_ship(system, target.clone()).await.unwrap();
    let source_fit = Fit::for_space_type(sde.clone(), ship_type, Vec::new());
    let target_fit = Fit::for_space_type(sde, ship_type, Vec::new());
    let source_stats = stats.clone();
    let target_stats = stats.clone();
    let target_id = target.id;
    let source_id = source.id;
    let character = fixture.character;
    let raw = stats.max[0] * 0.01 / stats.resonance[0][0];
    let generation = crate::space::targeting::next_generation();
    let weak = Arc::downgrade(&world);
    let (first, second, before) = world.with(system, move |s| {
        // Each hit reserves all three layers before releasing its unused slots:
        // three shield keys plus two transient slots admit setup but not impact.
        crate::space::snapshot::facts_test_init(s, 5, 1);
        let condition = |stats: ShipStats| Condition {
            now: stats.max, stats, shield_at: crate::client::filetime_now() + 60 * infinity_core::consts::FILETIME_SECOND,
        };
        s.install_condition(source_id, condition(source_stats), source_fit);
        s.install_condition(target_id, condition(target_stats), target_fit);
        assert_eq!(s.pilot_ball(character).map(|ball| ball.id), Some(source_id));
        assert_eq!(s.combat_phase(source_id), Some(crate::space::combat::CombatPhase::Live));
        for sequence in 1..=3 {
            let facts = s.prepare_combat_source(source_id, Some(100 + sequence as i64), None, Some(charge),
                crate::lua::events::resources::FactOrigin::Natural, generation, sequence, 1, |_| true).unwrap();
            let damage = DamageSource { attacker_id: Some(character), facts: Some(facts), ..Default::default() };
            assert!(s.apply_damage(target_id, [raw, 0.0, 0.0, 0.0], damage, false).unwrap().is_some());
        }
        let first = s.prepare_combat_source(source_id, Some(200), None, Some(charge),
            crate::lua::events::resources::FactOrigin::Natural, generation, 4, 1, |_| true).unwrap();
        let first = PreparedCombatAttempt::new(first, weak.clone(), system);
        let second = s.prepare_combat_source(source_id, Some(200), None, Some(charge),
            crate::lua::events::resources::FactOrigin::Natural, generation, 5, 1, |_| true).unwrap();
        (first, PreparedCombatAttempt::new(second, weak, system), s.combat_condition_snapshot(target_id).unwrap().condition.now)
    }).await.unwrap();
    let master = 200;
    let old = Arc::new(AtomicBool::new(false));
    {
        let mut state = combat::state();
        state.active.insert(master, combat::ActiveModule { char_id: character, stop: old.clone(), forced_inactive: false });
        state.activation_generation.insert(master, old.clone());
    }
    let projectile = || Missile { charge, raw: [raw, 0.0, 0.0, 0.0], aoe_cloud: 1.0,
        aoe_velocity: 10_000.0, drf: 1.0, velocity: 4_000.0, flight_time_ms: 5_000.0,
        weapon: charge, illegal: false, dot: None };
    launch_missile(db.clone(), world.clone(), system, source.clone(), target.clone(), character, Some(master),
        projectile(), None, first.with_activation(master, old.clone())).await.unwrap();
    assert!(world.after(system, 2_000).await);
    world.with(system, |_| ()).await.unwrap();
    assert!(old.load(Ordering::Relaxed));
    assert_eq!(world.with(system, move |s| s.combat_condition_snapshot(target_id).unwrap().condition.now).await.unwrap(), before);
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|(event, args)| event == "OnRemoteMessage"
        && args == vec![Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY),
            Value::dict([("notify", Value::unicode(infinity_core::messages::effects::COMBAT_CAPACITY_FULL))])]));
    let replacement = Arc::new(AtomicBool::new(false));
    {
        let mut state = combat::state();
        state.active.insert(master, combat::ActiveModule { char_id: character, stop: replacement.clone(), forced_inactive: false });
        state.activation_generation.insert(master, replacement.clone());
    }
    launch_missile(db.clone(), world.clone(), system, source, target, character, Some(master),
        projectile(), None, second.with_activation(master, old)).await.unwrap();
    assert!(world.after(system, 2_000).await);
    world.with(system, |_| ()).await.unwrap();
    assert!(!replacement.load(Ordering::Relaxed));
    assert!(combat::state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &replacement) && !active.forced_inactive));
    assert_eq!(world.with(system, move |s| s.combat_condition_snapshot(target_id).unwrap().condition.now).await.unwrap(), before);
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|(event, _)| event == "OnRemoteMessage"));
    {
        let mut state = combat::state();
        state.active.remove(&master);
        state.activation_generation.remove(&master);
    }
    world.unregister_pilot_generation(character, None);
    world.remove_ball(system, fixture.ship).await;
    world.remove_ball(system, target_id).await;
    drop(world);
    cleanup_ship(&db, None, fixture).await.unwrap();
}

#[test]
fn missile_lifecycle_preserves_exploration_site() {
    crate::tests::init_config();
    let mut system = System::new(0, Arc::default(), 12_345);
    let site = ball();
    let (site_id, site_type, site_pos) = (site.id, site.type_id, site.pos);
    let missile_id = NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed);
    let missile = Ball {
        id: missile_id, type_id: 2, mode: crate::space::MODE_STOP, pos: [4.0, 5.0, 6.0],
        ..site.clone()
    };
    system.insert(site);
    system.launch_missile(missile, site_id, site_id, 500.0, |_, _| {});

    let retained_site = system.ball(site_id).unwrap();
    assert_eq!((retained_site.id, retained_site.type_id, retained_site.pos), (site_id, site_type, site_pos));
    let retained_missile = system.ball(missile_id).unwrap();
    assert_eq!((retained_missile.id, retained_missile.type_id, retained_missile.pos), (missile_id, 2, [4.0, 5.0, 6.0]));
    let second_id = NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed);
    assert_ne!(missile_id, second_id);
    assert_eq!(second_id, missile_id + 1);

    system.tick(13_000);
    system.tick(13_500);
    assert!(!system.contains(missile_id));
    let retained_site = system.ball(site_id).unwrap();
    assert_eq!((retained_site.id, retained_site.type_id, retained_site.pos), (site_id, site_type, site_pos));
}


#[test]
fn hit_quality_bands() {
    assert_eq!(hit_quality(0.0), 0);
    assert_eq!(hit_quality(0.5), 1);
    assert_eq!(hit_quality(1.1), 4);
    assert_eq!(hit_quality(3.0), 6);
}


#[test]
fn turret_native_accuracy_vectors_and_guards() {
    let mut src = ball();
    src.id = 1;
    src.pos = [0.0; 3];
    src.radius = 100.0;
    let mut dst = ball();
    dst.id = 2;
    dst.radius = 10.0;
    dst.pos = [10_000.0, 0.0, 0.0];
    dst.vel = [0.0; 3];
    let chance = |a: &Ball, b: &Ball, optimal, falloff, tracking| {
        turret_chance(a, b, optimal, falloff, tracking, 400.0, 40.0)
    };
    assert_eq!(chance(&src, &dst, 20_000.0, 10_000.0, 0.01), 1.0);
    dst.vel = [500.0, 0.0, 0.0];
    assert_eq!(chance(&src, &dst, 20_000.0, 10_000.0, 0.01), 1.0);
    dst.vel = [0.0, 10.0, 0.0];
    assert!((chance(&src, &dst, 20_000.0, 10_000.0, 0.01) - 0.5).abs() < 1e-12);
    dst.vel = [0.0, 500.0, 0.0];
    assert!(chance(&src, &dst, 20_000.0, 10_000.0, 0.01) < 0.2);
    dst.vel = [0.0; 3];
    dst.pos[0] = 30_110.0;
    assert!((chance(&src, &dst, 20_000.0, 10_000.0, 0.01) - 0.5).abs() < 1e-12);
    dst.vel = [0.0, 30.11, 0.0];
    assert!((chance(&src, &dst, 20_000.0, 10_000.0, 0.01) - 0.25).abs() < 1e-12);
    assert!((chance(&src, &dst, 20_000.0, -10_000.0, -0.01) - 0.25).abs() < 1e-12);
    assert!((chance(&src, &dst, 20_000.0, -10_000.0, 0.01) - 0.25).abs() < 1e-12);
    assert!((chance(&src, &dst, 20_000.0, 10_000.0, -0.01) - 0.25).abs() < 1e-12);
    assert_eq!(chance(&src, &dst, 20_000.0, 0.0, 0.0), 0.0);
    assert_eq!(chance(&src, &dst, 40_000.0, 0.0, 0.0), 1.0);
    assert_eq!(chance(&src, &dst, -1.0, 10_000.0, 0.01), 0.0);
    dst.id = src.id;
    assert_eq!(chance(&src, &dst, 20_000.0, 10_000.0, 0.01), 0.0);
    dst.id = 2;
    dst.pos = src.pos;
    assert_eq!(chance(&src, &dst, 20_000.0, 10_000.0, 0.01), 0.0);
}

#[test]
fn missile_application_vectors() {
    // Scourge Light: sig_f=35/40=.875; vel_f=(.875*170/400)^.604 ≈ .550.
    let expected = (0.875_f64 * 170.0 / 400.0).powf(0.604);
    assert!((missile_applied(35.0, 400.0, 40.0, 170.0, 0.604) - expected).abs() < 1e-12);
    assert!((expected - 0.550).abs() < 0.001);
    assert_eq!(missile_applied(35.0, 0.0, 40.0, 170.0, 0.604), 0.875);
    assert_eq!(missile_applied(35.0, 400.0, 40.0, 0.0, 0.604), 0.875);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn signature_uses_cached_type_and_default_values() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let data = sde.types.iter().find(|t| t.attribute(ATTR_SIGNATURE_RADIUS).is_some_and(|v| v > 0.0))
        .expect("SDE type with signature radius");
    let mut target = ball();
    target.type_id = data.type_id;
    target.signature_radius = Some(0.0);
    assert_eq!(target_signature(&sde, &target), data.attribute(ATTR_SIGNATURE_RADIUS).unwrap());
    target.signature_radius = Some(42.0);
    assert_eq!(target_signature(&sde, &target), 42.0);
    target.type_id = -1;
    target.signature_radius = Some(0.0);
    assert_eq!(target_signature(&sde, &target), 100.0);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn missile_uses_signature_and_speed_at_impact() {
    use infinity_core::consts::{CATEGORY_ENTITY, FILETIME_SECOND};
    use crate::services::combat::groups_health::{Condition, ShipStats};

    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let (type_id, fit, stats) = sde.types.iter().filter(|t| t.type_id > 0 && t.category_id == Some(CATEGORY_ENTITY))
        .find_map(|t| {
            let fit = Fit::for_space_type(sde.clone(), t.type_id, Vec::new());
            let stats = ShipStats::from_fit(&fit, Key::Ship);
            (stats.max.iter().all(|v| v.is_finite() && *v >= 0.0)
                && stats.max[0] > 0.0
                && stats.resonance.iter().flatten().all(|v| v.is_finite() && *v >= 0.0)
                && stats.resonance[0][0] > 0.0
                && stats.shield_tau_ms.is_finite() && stats.shield_tau_ms >= 0.0)
                .then_some((t.type_id, fit, stats))
        }).expect("SDE NPC with fitted shield and EM resonance");
    let (shield, resonance) = (stats.max[0], stats.resonance[0][0]);
    let charge = missile_charge(&sde);
    let world = World::new(db.clone(), sde, 1);
    let system = -31_001;
    let target_id = NEXT_MISSILE_ID.fetch_add(2, Ordering::Relaxed);
    let mut source = ball();
    source.id = target_id + 1;
    source.type_id = type_id;
    source.char_id = None;
    source.pos = [0.0; 3];
    let mut target = ball();
    target.id = target_id;
    target.type_id = type_id;
    target.mode = crate::space::MODE_RIGID;
    target.free = false;
    target.pos = [2_000.0, 0.0, 0.0];
    target.signature_radius = Some(200.0);
    target.vel = [0.0; 3];
    assert!(crate::dao::items::dogma::overrides(&db, target_id).await.unwrap().is_empty());
    world.system(system).await.unwrap();
    let inserted = target.clone();
    let test_world = world.clone();
    let test_db = db.clone();
    let source_ball = source.clone();
    let source_fit = Fit::for_space_type(test_world.sde().clone(), type_id, Vec::new());
    let source_stats = stats.clone();
    let result = tokio::spawn(async move {
    test_world.with(system, move |s| {
        s.insert(inserted);
        s.insert(source_ball);
        s.install_condition(target_id + 1, Condition {
            now: source_stats.max, stats: source_stats, shield_at: crate::client::filetime_now() + 60 * FILETIME_SECOND,
        }, source_fit);
        s.install_condition(target_id, Condition {
            now: stats.max, stats, shield_at: crate::client::filetime_now() + 60 * FILETIME_SECOND,
        }, fit);
        assert_eq!(s.combat_phase(target_id + 1), Some(crate::space::combat::CombatPhase::Live));
    }).await.unwrap();
    let stale = Missile {
        charge, raw: [shield, 0.0, 0.0, 0.0], aoe_cloud: 40.0, aoe_velocity: 170.0,
        drf: 0.604, velocity: 1_000.0, flight_time_ms: 5_000.0, weapon: charge, illegal: false, dot: None,
    };
    let before = test_world.with(system, |s| s.balls().count()).await.unwrap();
    let guard = crate::services::effects::space_source::SourceGuard::Npc {
        source: source.id, target: target.id, generation: 0,
    };
    let stale_attempt = attempt(&test_world, system, source.id, None, charge).await;
    let stale_facts = stale_attempt.facts();
    assert_eq!(stale_facts.entity.item_id, source.id);
    assert_eq!(stale_facts.entity.type_id, type_id);
    assert_eq!(stale_facts.module_id, None);
    assert_eq!(stale_facts.actual_actor_character, None);
    launch_missile(test_db.clone(), test_world.clone(), system, source.clone(), target.clone(), 1, None, stale, Some(guard), stale_attempt).await.ok().unwrap();
    let weak = Arc::downgrade(&test_world);
    let replay = test_world.with(system, move |s| s.prepare_combat_source(stale_facts.entity.item_id, None, None, Some(charge),
        crate::lua::events::resources::FactOrigin::Natural,
        u64::from_be_bytes(stale_facts.activation[8..].try_into().unwrap()), stale_facts.sequence, 1, |_| true)
        .map(|facts| PreparedCombatAttempt::new(facts, weak, system))).await.unwrap();
    assert!(matches!(replay, Err(crate::space::combat_facts::Refusal::Replay)));
    assert_eq!(test_world.with(system, |s| s.balls().count()).await.unwrap(), before);
    let missile = Missile {
        charge, raw: [shield * 0.25 / resonance, 0.0, 0.0, 0.0],
        aoe_cloud: 40.0, aoe_velocity: 170.0, drf: 0.604,
        velocity: 1_000.0, flight_time_ms: 5_000.0, weapon: charge, illegal: false, dot: None,
    };
    let impact_attempt = attempt(&test_world, system, source.id, None, charge).await;
    launch_missile(test_db.clone(), test_world.clone(), system, source.clone(), target.clone(), 1, None, missile, None, impact_attempt).await.ok().unwrap();
    test_world.with(system, move |s| s.command(target_id, |ball, _, _| {
        ball.signature_radius = Some(35.0);
        ball.vel = [0.0, 400.0, 0.0];
        ball.old_vel = ball.vel;
        ball.segment_acceleration = None;
        vec![]
    })).await.unwrap();
    assert!(test_world.after(system, 3_000).await);
    // The stored impact anchor excludes recharge after damage; ball health is sampled later.
    let condition = test_world.with(system, move |s| s.combat_condition_snapshot(target_id).unwrap().condition).await.unwrap();
    let fraction = condition.now[0] / condition.stats.max[0];
    let application = (0.875_f64 * 170.0 / 400.0).powf(0.604);
    assert!((fraction - (1.0 - 0.25 * application)).abs() < 1e-9,
        "fraction={fraction:?}, application={application:?}, condition={condition:?}");
    let mut duplicate_attempt = attempt(&test_world, system, target_id + 1, Some(123), charge).await;
    let facts = duplicate_attempt.facts();
    assert_eq!(facts.module_id, Some(123));
    let damage = DamageSource { attacker_id: facts.actual_actor_character,
        attacker_ball_id: Some(facts.entity.item_id), source_ship_id: Some(facts.entity.item_id),
        source_ship_type: Some(facts.entity.type_id), weapon_type_id: Some(charge),
        facts: Some(facts), force_origin: None, illegal: false };
    assert!(apply_damage_guarded(&test_db, &test_world, system, target_id, [1.0, 0.0, 0.0, 0.0],
        damage.clone(), false, |_| true).await.unwrap().is_some());
    duplicate_attempt.disarm();
    let after_first = test_world.with(system, move |s| s.combat_condition_snapshot(target_id).unwrap().condition.now).await.unwrap();
    assert!(matches!(apply_damage_guarded(&test_db, &test_world, system, target_id, [1.0, 0.0, 0.0, 0.0],
        damage, false, |_| true).await, Ok(None)));
    let after_duplicate = test_world.with(system, move |s| s.combat_condition_snapshot(target_id).unwrap().condition.now).await.unwrap();
    assert_eq!(after_first, after_duplicate);
    // Reserve both launches before either arrives, with the later sequence faster.
    let activation = crate::space::targeting::next_generation();
    let source_id = source.id;
    let weak = Arc::downgrade(&test_world);
    let (first, second) = test_world.with(system, move |s| {
        let prepare = |s: &mut System, sequence| s.prepare_combat_source(source_id, Some(124), None, Some(charge),
            crate::lua::events::resources::FactOrigin::Natural, activation, sequence, 1, |_| true).unwrap();
        let first = PreparedCombatAttempt::new(prepare(s, 1), weak.clone(), system);
        let second = PreparedCombatAttempt::new(prepare(s, 2), weak, system);
        (first, second)
    }).await.unwrap();
    let first_facts = first.facts();
    let second_facts = second.facts();
    assert_eq!(first_facts.activation, second_facts.activation);
    assert_eq!((first_facts.sequence, second_facts.sequence), (1, 2));
    assert_eq!(first_facts.entity, second_facts.entity);
    assert_eq!(first_facts.context, second_facts.context);
    let projectile = |velocity| Missile {
        charge, raw: [shield * 0.05 / resonance, 0.0, 0.0, 0.0],
        aoe_cloud: 1.0, aoe_velocity: 10_000.0, drf: 1.0,
        velocity, flight_time_ms: 20_000.0, weapon: charge, illegal: false, dot: None,
    };
    // Restore the rigid target's consistent stationary state: the 400 m/s
    // sideways velocity only served the application check above, and native
    // steering would chase a phantom intercept the slow shot cannot reach.
    test_world.with(system, move |s| s.command(target_id, |ball, _, _| {
        ball.vel = [0.0; 3];
        ball.old_vel = ball.vel;
        ball.segment_acceleration = None;
        vec![]
    })).await.unwrap();
    let before = after_duplicate[0];
    launch_missile(test_db.clone(), test_world.clone(), system, source.clone(), target.clone(), 1, Some(124),
        projectile(200.0), None, first).await.unwrap();
    launch_missile(test_db.clone(), test_world.clone(), system, source.clone(), target.clone(), 1, Some(124),
        projectile(4_000.0), None, second).await.unwrap();
    test_world.with(system, move |s| s.command(source_id, move |ball, _, _| {
        ball.corp_id = 54321;
        ball.type_id = charge;
        vec![]
    })).await.unwrap();
    assert!(test_world.after(system, 2_000).await);
    let (current_source, after_second) = test_world.with(system, move |s| (
        s.combat_entity(source_id).unwrap(), s.combat_condition_snapshot(target_id).unwrap().condition.now[0]
    )).await.unwrap();
    assert_ne!(current_source.corporation_id, second_facts.entity.corporation_id);
    assert_eq!(second_facts.entity.type_id, type_id);
    assert_eq!(second_facts.context, first_facts.context);
    assert!((before - after_second - shield * 0.05).abs() < 1e-7 * shield);
    // Redelivery must reach the real missile impact and still leave HP unchanged.
    launch_missile(test_db.clone(), test_world.clone(), system, source.clone(), target.clone(), 1, Some(124),
        projectile(4_000.0), None, PreparedCombatAttempt::new(second_facts, Arc::downgrade(&test_world), system)).await.unwrap();
    assert!(test_world.after(system, 2_000).await);
    let after_replay = test_world.with(system, move |s| s.combat_condition_snapshot(target_id).unwrap().condition.now[0]).await.unwrap();
    assert_eq!(after_replay, after_second);
    // The slow shot needs about ten seconds of real flight; wait on the actual
    // condition terminal instead of a fixed arrival estimate.
    let after_first = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let now = test_world.with(system, move |s| s.combat_condition_snapshot(target_id).unwrap().condition.now[0]).await.unwrap();
            if now < after_replay { break now; }
            assert!(test_world.after(system, 500).await, "actor stopped awaiting slow impact");
        }
    }).await.expect("slow missile impact timed out");
    assert!((after_replay - after_first - shield * 0.05).abs() < 1e-7 * shield);
    let weak = Arc::downgrade(&test_world);
    let (miss, cancelled) = test_world.with(system, move |s| {
        let prepare = |s: &mut System, sequence| s.prepare_combat_source(source_id, Some(124), None, Some(charge),
            crate::lua::events::resources::FactOrigin::Natural, activation, sequence, 1, |_| true).unwrap();
        let miss = PreparedCombatAttempt::new(prepare(s, 3), weak.clone(), system);
        let cancelled = PreparedCombatAttempt::new(prepare(s, 4), weak, system);
        (miss, cancelled)
    }).await.unwrap();
    let miss_facts = miss.facts();
    let cancelled_facts = cancelled.facts();
    let mut missed_projectile = projectile(4_000.0);
    missed_projectile.flight_time_ms = 100.0;
    let mut miss_source = source.clone();
    miss_source.pos = [-1_000.0, 0.0, 0.0];
    launch_missile(test_db.clone(), test_world.clone(), system, miss_source, target.clone(), 1, Some(124),
        missed_projectile, None, miss).await.unwrap();
    drop(cancelled);
    assert!(test_world.after(system, 2_000).await);
    for facts in [miss_facts, cancelled_facts] {
        launch_missile(test_db.clone(), test_world.clone(), system, source.clone(), target.clone(), 1, Some(124),
            projectile(4_000.0), None, PreparedCombatAttempt::new(facts, Arc::downgrade(&test_world), system)).await.unwrap();
    }
    assert!(test_world.after(system, 2_000).await);
    let after_miss_and_cancel = test_world.with(system, move |s| s.combat_condition_snapshot(target_id).unwrap().condition.now[0]).await.unwrap();
    assert_eq!(after_miss_and_cancel, after_first);
    }).await;
    crate::services::combat::forget_ship(target_id);
    world.remove_ball(system, target_id).await;
    world.remove_ball(system, target_id + 1).await;
    assert!(crate::dao::items::dogma::overrides(&db, target_id).await.unwrap().is_empty());
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn npc_miss_reports_physical_source_to_drone_owner() {
    use crate::space::drone::PreparedDrone;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let drone_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_DRONE))
        .expect("SDE drone type").type_id;
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP))
        .expect("SDE hull").type_id;
    let system = -31_002;
    let f = create_ship(&db, hull, ShipLocation::Space { system }, &[]).await.unwrap();
    let drone_id = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let tracked = drone_id.clone();
    let result = tokio::spawn(async move {
        w.system(system).await.unwrap();
        d.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&f.character, &f.ship]).await.unwrap();
        let id = creation::positioned(&d, drone_type, f.character, system, [0.0; 3]).await.unwrap();
        tracked.store(id, Ordering::Relaxed);
        let fit = crate::services::fit::Fit::drone(&d, sde, f.character, id).await.ok().expect("owned drone fit");
        let key = crate::services::fit::Key::Drone(id);
        let spec = crate::services::drones::worker::spec(&fit, key, drone_type).ok().expect("drone specification");
        let condition = crate::services::combat::prepare_condition_with_fit_key(&d, id, &fit, key).await.ok().expect("drone condition");
        let mut source = ball();
        source.id = NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed);
        source.type_id = drone_type;
        source.char_id = None;
        source.pos = [0.0; 3];
        let mut target = ball();
        target.id = id;
        target.type_id = drone_type;
        target.owner_id = f.character;
        target.pos = source.pos;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        w.register_pilot(target.owner_id, tx);
        let (a, b) = (source.clone(), target.clone());
        let source_fit = Fit::for_space_type(w.sde().clone(), drone_type, Vec::new());
        let source_stats = crate::services::combat::groups_health::ShipStats::from_fit(&source_fit, Key::Ship);
        let source_id = source.id;
        w.with(system, move |s| {
            s.insert(a);
            s.install_condition(source_id, crate::services::combat::groups_health::Condition {
                now: source_stats.max, stats: source_stats, shield_at: crate::client::filetime_now(),
            }, source_fit);
            s.restore_drone(PreparedDrone { ball: b, spec, condition, fit, key, npc_ignored: false },
                Arc::new(|_, _| Some(Box::pin(async {}))));
        }).await.unwrap();
        assert_eq!(target.char_id, None);
        let shot = attempt(&w, system, source.id, None, 42).await;
        turret_shot(&d, &w, system, &source, &target, None, 42, false, (1.0, 1.0, 1.0, 1.0), [1.0; 4], None, None, shot).await.ok().unwrap();
        let (event, args) = rx.try_recv().expect("drone owner receives NPC hit log");
        assert_eq!(event, "OnDamageMessage");
        assert_eq!(args[0], damage_message("source", source.id, 42, 0.0, HIT_MISS, false));
    }).await;
    world.unregister_pilot_generation(f.character, None);
    world.remove_ball(system, drone_id.load(Ordering::Relaxed)).await;
    // The temporary source belongs to this isolated actor and disappears with it.
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, drone_id.load(Ordering::Relaxed)]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&drone_id.load(Ordering::Relaxed), &f.character]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&f.character]).await.unwrap();
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn lethal_missile_contact_recovers_the_persistent_target_once() {
    let mut f = crate::services::drones::tests::fixture::Fixture::real_system().await;
    let (victim, target) = f.add_pilot().await;
    let result = crate::tests::catch_scenario(async {
        let system = f.system;
        f.world.with(system, move |s| s.command(target, |ball, _, _| {
            ball.pos = [1_000.0, 0.0, 0.0];
            ball.old_pos = ball.pos;
            vec![]
        })).await.unwrap();
        assert!(f.world.after(system, 1_000).await);
        let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(f.owner, sender);
        let source = f.world.ball(system, f.ship).await.unwrap();
        let target_ball = f.world.ball(system, target).await.unwrap();
        let charge = missile_charge(&f.sde);
        let projectile = || Missile { charge, raw: [1e30; 4], aoe_cloud: 1.0, aoe_velocity: 10_000.0,
            drf: 1.0, velocity: 100.0, flight_time_ms: 20_000.0, weapon: charge, illegal: false, dot: None };
        let shot = attempt(&f.world, system, source.id, None, charge).await;
        let facts = shot.facts();
        launch_missile(f.db.clone(), f.world.clone(), system, source.clone(), target_ball.clone(), f.owner, None, projectile(), None, shot).await.unwrap();
        assert!(f.world.with(system, move |s| matches!(s.combat_phase(target), Some(crate::space::combat::CombatPhase::Live))).await.unwrap());
        assert!(f.db.query("SELECT 1 FROM game.repairable_recovery_groups WHERE source_item_id=$1", &[&target]).await.unwrap().is_empty());
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if notices.try_recv().is_ok_and(|(event, _)| event == "OnDamageMessage") { break; }
                assert!(f.world.after(system, 100).await);
            }
        }).await.expect("missile contact and settlement receipt");
        assert!(f.world.ball(system, target).await.is_none());
        let selections = crate::dao::repairable_ship::list_owned(&f.db, victim).await.unwrap();
        assert_eq!(selections.len(), 1);
        let selected = &selections[0];
        assert_eq!(selected.group.shape.item.get(), target);
        assert_eq!(selected.items.iter().map(|m| m.item.get()).collect::<Vec<_>>(), [target]);
        let physical = crate::dao::items::repairable_ship::capture_many(&f.db, &[selected.group.shape.item]).await.unwrap().remove(0);
        assert_eq!((physical.owner, physical.location, physical.type_id, physical.flag, physical.quantity, physical.singleton),
            (infinity_core::consts::NPC_CORP_INTERBUS, selected.group.id.get(), target_ball.type_id, 0, 1, true));
        launch_missile(f.db.clone(), f.world.clone(), system, source, target_ball, f.owner, None, projectile(), None, PreparedCombatAttempt::new(facts, Arc::downgrade(&f.world), system)).await.unwrap();
        assert_eq!(f.db.query("SELECT kill_id FROM game.killmails WHERE victim_item_id=$1", &[&target]).await.unwrap().len(), 1);
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn singleton_turret_rolls_once_and_reports_unbanked() {
    use crate::services::combat::groups_health::{Condition, ShipStats};
    use crate::services::combat::tests::remote_repair_db as fixture;
    use infinity_core::consts::dogma::{ATTR_ARMOR_HP, ATTR_SHIELD_CAPACITY, ATTR_STRUCTURE_HP};

    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP].iter().all(|&a| t.attribute(a).is_some_and(|v| v > 0.0))).unwrap().type_id;
    let f = fixture::create(&db, ship_type, ship_type, 1).await.unwrap();
    let world = World::new(db.clone(), sde, 1);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(f.source, tx);
    world.add_ship(1, fixture::ball(f.ship, ship_type, f.source)).await.unwrap();
    let mut target = fixture::ball(f.victim, ship_type, f.target);
    target.pos = [1_000.0, 0.0, 0.0];
    world.add_ship(1, target.clone()).await.unwrap();
    for (ship, owner) in [(f.ship, f.source), (f.victim, f.target)] {
        let fit = Fit::load_in_space(&db, &world, 1, owner, ship).await.unwrap();
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        world.with(1, move |s| s.install_condition(ship, Condition { now: stats.max, stats,
            shield_at: crate::client::filetime_now() + 60 * infinity_core::consts::FILETIME_SECOND }, fit)).await.unwrap();
    }
    let before = fixture::condition(&world, 1, f.victim).await;
    let raw = [10.0; 4];
    let expected: f64 = (0..4).map(|i| raw[i] * before.stats.resonance[0][i]).sum();
    let rolls = Arc::new(AtomicI64::new(0));
    let counter = rolls.clone();
    let source = world.ball(1, f.ship).await.unwrap();
    let shot = attempt(&world, 1, f.ship, None, 42).await;
    turret_shot_with_roll(&db, &world, 1, &source, &target, Some(f.source), 42, false,
        (30_000.0, 10_000.0, 1.0, 400.0), raw, None, None, shot,
        move |_| { counter.fetch_add(1, Ordering::Relaxed); 1.0 }).await.unwrap();
    assert_eq!(rolls.load(Ordering::Relaxed), 1, "one roll for one singleton shot");
    let dealt: f64 = before.now.iter().zip(fixture::condition(&world, 1, f.victim).await.now).map(|(old, new)| old - new).sum();
    assert!(dealt > 0.0 && (dealt - expected).abs() <= 1e-6, "real condition absorbed the expected damage");
    let (event, args) = rx.try_recv().expect("shooter receives the hit log");
    assert_eq!((event.as_str(), args[0].clone()), ("OnDamageMessage", damage_message("target", f.victim, 42, expected, hit_quality(1.0), false)));
    assert!(rx.try_recv().is_err(), "singleton reports once per applicable observer");
    retire(world, &db, f).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn singleton_spool_returns_the_committed_ramp_and_applied_bonus() {
    use crate::services::combat::groups_health::{Condition, ShipStats};
    use crate::services::combat::tests::remote_repair_db as fixture;
    use crate::services::combat::tests::remote_repair_db::set_locked_target;
    use crate::services::combat::ActiveModule;
    use infinity_core::consts::dogma::{ATTR_ARMOR_HP, ATTR_SHIELD_CAPACITY, ATTR_STRUCTURE_HP};

    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP].iter().all(|&a| t.attribute(a).is_some_and(|v| v > 0.0))).unwrap().type_id;
    let f = fixture::create(&db, ship_type, ship_type, 1).await.unwrap();
    let world = World::new(db.clone(), sde, 1);
    world.add_ship(1, fixture::ball(f.ship, ship_type, f.source)).await.unwrap();
    let mut target = fixture::ball(f.victim, ship_type, f.target);
    target.pos = [1_000.0, 0.0, 0.0];
    world.add_ship(1, target.clone()).await.unwrap();
    for (ship, owner) in [(f.ship, f.source), (f.victim, f.target)] {
        let fit = Fit::load_in_space(&db, &world, 1, owner, ship).await.unwrap();
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        world.with(1, move |s| s.install_condition(ship, Condition { now: stats.max, stats,
            shield_at: crate::client::filetime_now() + 60 * infinity_core::consts::FILETIME_SECOND }, fit)).await.unwrap();
    }
    set_locked_target(f.source, f.victim, true);
    let stop = Arc::new(AtomicBool::new(false));
    state().active.insert(f.module, ActiveModule { char_id: f.source, stop: stop.clone(), forced_inactive: false });
    let binding = world.with(1, move |s| SpoolBinding {
        source_membership: s.combat_membership_generation(f.ship).unwrap(),
        activation_generation: 7, target: f.victim,
        target_membership: s.combat_membership_generation(f.victim).unwrap() }).await.unwrap();
    let parameters = SpoolParameters { step: 0.5, maximum: 0.5 };
    let mut state_spool = SpoolState::default();
    state_spool.bind(binding);
    let source = fixture::ball(f.ship, ship_type, f.source);
    let fire = |state_spool, range: (f64, f64, f64, f64), roll: f64| {
        let (db, world, stop, source, target) = (db.clone(), world.clone(), stop.clone(), source.clone(), target.clone());
        async move {
            turret_shot_with_roll(&db, &world, 1, &source, &target, Some(f.source), 42, false, range, [10.0; 4], None,
                Some(TurretSpool::Fitted { binding, module: f.module, master: f.module, pilot: f.source, stop, parameters, state: state_spool }),
                attempt(&world, 1, f.ship, None, 42).await, move |_| roll).await.unwrap()
        }
    };
    for shot in 0..2 {
        let TurretResolution::Fired { fitted_spool: Some(result) } = fire(state_spool, (30_000.0, 10_000.0, 1.0, 400.0), 1.0).await
            else { panic!("shot {shot} must fire") };
        assert_eq!(result.applied_bonus, 0.5 * shot as f64);
        state_spool = result.state;
    }
    assert_eq!(state_spool.current(parameters), parameters.maximum);
    assert!(matches!(fire(state_spool, (500.0, 100.0, 1.0, 400.0), 1.0).await, TurretResolution::NotFired)
        && state_spool.current(parameters) == parameters.maximum);
    let before = world.with(1, move |s| s.combat_condition_snapshot(f.victim).unwrap().condition.now).await.unwrap();
    let TurretResolution::Fired { fitted_spool: Some(missed) } = fire(state_spool, (30_000.0, 10_000.0, 1.0, 400.0), 0.0).await
        else { panic!("a genuine miss still fires") };
    let after = world.with(1, move |s| s.combat_condition_snapshot(f.victim).unwrap().condition.now).await.unwrap();
    assert_eq!((missed.applied_bonus, missed.state.current(parameters), after), (parameters.maximum, parameters.maximum, before));
    set_locked_target(f.source, f.victim, false);
    state().active.remove(&f.module);
    retire(world, &db, f).await;
}
