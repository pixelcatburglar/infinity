//! Admission regressions using SDE projection modifiers and the actor's fitted cache.
use super::*;
use infinity_core::consts::dogma::ATTR_WARP_SCRAMBLE_STATUS;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::effects::{projection::Projection, EffectKind, JammingType};
use crate::services::fit::{Fit, Key};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

async fn fixture() -> (space::System, Projection) {
    fixture_for(JammingType::Scramble).await
}

async fn fixture_for(jamming: JammingType) -> (space::System, Projection) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && {
            let fit = Fit::for_space_type(sde.clone(), t.type_id, vec![]);
            let stats = ShipStats::from_fit(&fit, Key::Ship);
            fit.value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS) <= 0.0
                && stats.max.iter().all(|v| v.is_finite() && *v > 0.0)
        }).expect("ship with usable fit").type_id;
    let mut selected = None;
    'search: for source in sde.types.iter() {
        let source_fit = Fit::for_space_type(sde.clone(), source.type_id, vec![]);
        for effect in source.effects() {
            let Some(row) = sde.dogma.effect(effect.effect_id) else { continue };
            if !crate::services::effects::registry::resolve(&sde, effect.effect_id)
                .is_some_and(|e| e.kind == (EffectKind::Projected { jamming })) { continue; }
            let mut values = Vec::new();
            for modifier in row.modifiers().iter().filter(|m| m.domain == "targetID") {
                if values.iter().any(|(id, _)| *id == modifier.modifying) { continue; }
                let attr = crate::sde::dogma::python_effects::projected_interim_source(modifier.modifying).unwrap_or(modifier.modifying);
                values.push((modifier.modifying, source_fit.value(Key::Ship, attr)));
            }
            let p = Projection { source_ship: 124, source_module: 125, source_type: source.type_id,
                effect_id: effect.effect_id, kind: EffectKind::Projected { jamming },
                until_ms: i64::MAX, values, charge_values: vec![], charge_type: None, scramble: None };
            let projected = Fit::for_space_type(sde.clone(), hull, vec![p.clone()]);
            if projected.value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS) > 0.0 {
                selected = Some(p);
                break 'search;
            }
        }
    }
    let projection = selected.expect("SDE projection that overcomes hull resistance");
    let fit = Fit::for_space_type(sde, hull, vec![]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    let mut system = space::System::new(0, Arc::default(), 0);
    let mut victim = ship();
    victim.type_id = hull;
    victim.mode = MODE_STOP;
    victim.pos = [0.0; 3];
    victim.vel = [0.0; 3];
    // Zero max speed forces the native alignment deadline rather than early alignment.
    victim.max_vel = 0.0;
    let mut source = victim.clone();
    source.id = 124;
    source.free = false;
    system.insert(victim);
    system.insert(source);
    system.install_condition(123, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    assert!(system.combat_membership_generation(123).is_some());
    (system, projection)
}

async fn disruption_after_acceptance(already_aligning: bool) {
    let (mut system, projection) = fixture().await;
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    system.observe(123, 123, tx);
    while events.try_recv().is_ok() {}
    let called = Arc::new(AtomicBool::new(false));
    let exit = called.clone();
    let revision = system.projection_revision(123);
    let arrival = system.register_warp_arrival(Box::new(move |_| { exit.store(true, Ordering::SeqCst); }));
    let result = commit_warp(&mut system, 123, revision, false, 4000,
        WarpDestination::Exact([1_000_000.0, 0.0, 0.0]), Some(arrival));
    assert_eq!(result, WarpCommit::Queued);
    system.release_warp_arrival(arrival);
    if already_aligning {
        system.tick(1000);
        assert_eq!(system.ball(123).unwrap().mode, MODE_WARP);
        assert!(!system.ball(123).unwrap().is_warping());
    }
    system.upsert_projection(123, projection);
    assert!(system.projection_revision(123) > revision);
    assert_eq!(system.projections_on(123).len(), 1);
    for second in (if already_aligning { 2 } else { 1 })..=190 {
        system.tick(second * 1000);
        assert!(!system.ball(123).unwrap().is_warping(), "disrupted ship activated warp at tick {second}");
    }
    if !already_aligning {
        let batches: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).flatten().collect();
        let actions: Vec<_> = batches.iter().filter_map(|v| {
            let Value::Tuple(parts) = v else { return None };
            let Value::Tuple(call) = &parts[1] else { return None };
            Some((parts[0].clone(), call[0].as_bytes()?.to_vec()))
        }).filter(|(_, name)| name == b"WarpTo" || name == b"Stop").collect();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].1, b"WarpTo");
        assert_eq!(actions[1].1, b"Stop");
        assert_eq!(actions[0].0, actions[1].0);
    }
    assert!(!called.load(Ordering::SeqCst), "interrupted warp invoked its arrival callback");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn disruption_after_acceptance_before_command_prevents_activation() {
    disruption_after_acceptance(false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn disruption_during_alignment_prevents_timeout_activation() {
    disruption_after_acceptance(true).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn stale_fit_refresh_cannot_replace_new_projection() {
    let (mut system, projection) = fixture().await;
    let generation = system.combat_membership_generation(123).unwrap();
    let fit_revision = system.combat_fit_revision(123).unwrap();
    let old_revision = system.projection_revision(123);
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let fit = Fit::for_space_type(sde, system.ball(123).unwrap().type_id, vec![]);
    system.upsert_projection(123, projection);
    assert!(!system.replace_fitted_cache(123, generation, fit_revision, fit, Key::Ship, Some(old_revision)));
    assert_eq!(system.projections_on(123).len(), 1);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn disruption_does_not_interrupt_active_warp() {
    let (mut system, projection) = fixture().await;
    system.command(123, |b, _, _| {
        b.max_vel = 100.0;
        b.vel = [100.0, 0.0, 0.0];
        Vec::new()
    });
    let revision = system.projection_revision(123);
    assert_eq!(commit_warp(&mut system, 123, revision, false, 4000,
        WarpDestination::Exact([1_000_000_000.0, 0.0, 0.0]), None), WarpCommit::Queued);
    system.tick(1000);
    assert!(system.ball(123).unwrap().is_warping());
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    system.observe(123, 123, tx);
    while events.try_recv().is_ok() {}
    system.upsert_projection(123, projection);
    system.tick(2000);
    assert!(system.ball(123).unwrap().is_warping());
    while let Ok(batch) = events.try_recv() {
        assert!(!batch.iter().any(|v| {
            let Value::Tuple(parts) = v else { return false };
            let Value::Tuple(call) = &parts[1] else { return false };
            call[0].as_bytes() == Some(b"Stop")
        }));
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn queued_warp_does_not_apply_to_replacement_with_same_id() {
    let (mut system, _) = fixture().await;
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let mut pilot = system.take(123).unwrap();
    pilot.char_id = Some(789);
    let hull = pilot.type_id;
    system.insert(pilot);
    let fit = Fit::for_space_type(sde.clone(), hull, vec![]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(123, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    let generation = system.combat_membership_generation(123).unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let exit = called.clone();
    let revision = system.projection_revision(123);
    let arrival = system.register_warp_arrival(Box::new(move |_| { exit.store(true, Ordering::SeqCst); }));
    assert_eq!(commit_warp(&mut system, 123, revision, false, 4000,
        WarpDestination::Exact([1_000_000.0, 0.0, 0.0]), Some(arrival)), WarpCommit::Queued);
    system.release_warp_arrival(arrival);
    let mut replacement = system.take(123).unwrap();
    replacement.mode = MODE_STOP;
    replacement.pos = [50.0, 0.0, 0.0];
    replacement.vel = [0.0; 3];
    system.insert(replacement);
    let fit = Fit::for_space_type(sde, hull, vec![]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(123, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    assert_ne!(system.combat_membership_generation(123).unwrap(), generation);
    system.tick(1000);
    let ball = system.ball(123).unwrap();
    assert_eq!(ball.mode, MODE_STOP, "old warp command changed replacement");
    assert_eq!(ball.pos, [50.0, 0.0, 0.0]);
    assert!(!called.load(Ordering::SeqCst), "old arrival callback ran for replacement");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn removing_scramble_allows_a_new_warp_without_old_callback() {
    let (mut system, projection) = fixture().await;
    let called = Arc::new(AtomicBool::new(false));
    let exit = called.clone();
    let revision = system.projection_revision(123);
    let arrival = system.register_warp_arrival(Box::new(move |_| { exit.store(true, Ordering::SeqCst); }));
    assert_eq!(commit_warp(&mut system, 123, revision, false, 4000,
        WarpDestination::Exact([1_000_000.0, 0.0, 0.0]), Some(arrival)), WarpCommit::Queued);
    system.release_warp_arrival(arrival);
    let module = projection.source_module;
    system.upsert_projection(123, projection);
    system.tick(1000);
    assert!(!system.ball(123).unwrap().is_warping());
    system.remove_projection(module);
    assert!(system.projections_on(123).is_empty());
    system.command(123, |b, _, _| {
        b.max_vel = 100.0;
        b.vel = [100.0, 0.0, 0.0];
        Vec::new()
    });
    let revision = system.projection_revision(123);
    assert_eq!(commit_warp(&mut system, 123, revision, false, 4320,
        WarpDestination::Exact([2_000_000.0, 0.0, 0.0]), None), WarpCommit::Queued);
    system.tick(2000);
    assert!(system.ball(123).unwrap().is_warping(), "warp remained blocked after projection removal");
    assert_eq!(system.ball(123).unwrap().warp.factor, 4320);
    assert!(!called.load(Ordering::SeqCst), "old arrival callback ran after a new warp request");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn disruptor_cancels_accepted_warp() {
    let (mut system, projection) = fixture_for(JammingType::Disrupt).await;
    let revision = system.projection_revision(123);
    assert_eq!(commit_warp(&mut system, 123, revision, false, 4000,
        WarpDestination::Exact([1_000_000.0, 0.0, 0.0]), None), WarpCommit::Queued);
    system.upsert_projection(123, projection);
    system.tick(1000);
    assert_eq!(system.ball(123).unwrap().mode, MODE_STOP);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn stale_cancellation_does_not_stop_replacement() {
    let (mut system, projection) = fixture().await;
    let generation = system.combat_membership_generation(123).unwrap();
    system.upsert_projection(123, projection);
    let mut replacement = system.take(123).unwrap();
    replacement.mode = MODE_WARP;
    replacement.effect_stamp = -1;
    system.insert(replacement);
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let fit = Fit::for_space_type(sde, system.ball(123).unwrap().type_id, vec![]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(123, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    assert!(system.combat_membership_generation(123).is_some());
    assert_ne!(system.combat_membership_generation(123), Some(generation));
    system.tick(1000);
    assert_eq!(system.ball(123).unwrap().mode, MODE_WARP);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn effective_resistance_preserves_preparing_warp() {
    let (mut system, projection) = fixture().await;
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.published == Some(true)
        && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && {
            let base = Fit::for_space_type(sde.clone(), t.type_id, vec![]);
            let projected = Fit::for_space_type(sde.clone(), t.type_id, vec![projection.clone()]);
            base.value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS) < 0.0
                && projected.value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS) <= 0.0
                && ShipStats::from_fit(&base, Key::Ship).max.iter().all(|v| v.is_finite() && *v > 0.0)
        }).expect("published hull resisting projection").type_id;
    let mut victim = system.take(123).unwrap();
    victim.type_id = hull;
    system.insert(victim);
    let fit = Fit::for_space_type(sde, hull, vec![]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(123, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    let revision = system.projection_revision(123);
    assert_eq!(commit_warp(&mut system, 123, revision, false, 4000,
        WarpDestination::Exact([1_000_000.0, 0.0, 0.0]), None), WarpCommit::Queued);
    system.upsert_projection(123, projection);
    system.tick(1000);
    assert_eq!(system.ball(123).unwrap().mode, MODE_WARP);
    assert!(!system.ball(123).unwrap().is_warping());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn disruption_does_not_emit_stop_for_non_preparing_modes() {
    for mode in [MODE_STOP, crate::space::MODE_GOTO] {
        let (mut system, projection) = fixture().await;
        let mut victim = system.take(123).unwrap();
        victim.mode = mode;
        system.insert(victim);
        let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
            schema_on_start: false, ..crate::config::tests::test_database()
        }).await.unwrap();
        let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
        let fit = Fit::for_space_type(sde, system.ball(123).unwrap().type_id, vec![]);
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        system.install_condition(123, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        system.observe(123, 123, tx);
        while events.try_recv().is_ok() {}
        system.upsert_projection(123, projection);
        system.tick(1000);
        assert_eq!(system.ball(123).unwrap().mode, mode);
        while let Ok(batch) = events.try_recv() {
            assert!(!batch.iter().any(|v| {
                let Value::Tuple(parts) = v else { return false };
                let Value::Tuple(call) = &parts[1] else { return false };
                call[0].as_bytes() == Some(b"Stop")
            }));
        }
    }
}
