use super::*;
use crate::lua::events::{GameEvent, resources::FactOrigin};
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::effects::JammingType;
use crate::services::fit::{Fit, Key};

#[test]
fn observing_without_a_ledger_advances_actor_clock_without_allocating_one() {
    let mut system = System::default();
    assert_eq!(system.combat_observed_time(), 0);
    system.observe_combat_utc().unwrap();
    assert!(system.combat_observed_time() > 0);
    assert!(system.combat_facts.is_none());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn real_projection_refresh_and_stop_record_actual_lifetime() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|ty| {
        if ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP) { return false; }
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
        ShipStats::from_fit(&fit, Key::Ship).max.iter().all(|max| max.is_finite() && *max > 1.0)
    }).unwrap().type_id;
    let ball = Ball {
        id: 1, type_id: ty, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: None,
        signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        goto: [0.0; 3], follow_id: 0, old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, follow_range: 0.0, free: true, global: false, massive: true,
        interactive: true, warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    let mut system = System::default();
    for id in [1, 2, 3] {
        system.insert(Ball { id, ..ball.clone() });
        let fit = Fit::for_space_type(sde.clone(), ty, Vec::new());
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        system.install_condition(id, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    }
    system.nativefact_clock = 0;
    system.observe_combat_utc().unwrap();
    assert!(system.combat_observed_time() > 0);
    assert!(system.combat_facts.is_none());
    assert!(system.set_condition_fraction(1, 0.5));
    assert!(system.repair_condition(1, 1, 1.0, crate::space::combat::RepairSource::Local).unwrap());
    assert!(crate::space::snapshot::capture_fact_checkpoint(&mut system).unwrap().is_none());
    let sampled_at = system.combat_observed_time();
    let condition = system.capture_dirty_condition(1, true, sampled_at).unwrap();
    assert_eq!(condition.sampled_at, sampled_at);
    assert!(condition.sampled_at > 0);
    assert!(system.combat_facts.is_none());

    let mut origin = System::default();
    origin.ensure_combat_facts();
    origin.nativefact_clock = sampled_at + infinity_core::consts::FILETIME_DAY;
    origin.close_combat_character_boundary(777).unwrap();
    let receipt = origin.combat_character_scope(777).unwrap();
    let imported = system.import_combat_character_scope(777, receipt).unwrap();
    assert!(system.combat_observed_time() >= receipt.at);
    assert!(imported.at >= receipt.at);
    let generation = crate::space::targeting::next_generation();
    let pending = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
        generation, 1, 1, |_| true).unwrap();
    system.finish_combat_attempt(&pending);
    let generation = crate::space::targeting::next_generation();
    let mut projection = Projection { source_ship: 1, source_module: 10, source_type: ty, effect_id: 11,
        kind: EffectKind::Projected { jamming: JammingType::Scramble }, until_ms: i64::MAX,
        values: vec![], charge_values: vec![], charge_type: None, scramble: None };
    for sequence in 1..=2 {
        let source = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
            generation, sequence, 1, |_| true).unwrap();
        projection.scramble = Some(system.accept_projection_scramble(2, &projection, source).unwrap());
        system.upsert_projection(2, projection.clone());
    }
    system.remove_projection(10);
    assert!(system.projections_on(2).is_empty());
    let at = system.combat_fact_time();
    let batch = system.combat_facts.as_mut().unwrap().capture(at).unwrap().unwrap();
    let events = batch.events();
    let GameEvent::WarpScrambleApplied { slices, intervals, .. } = &events[0].event else { panic!("scramble fact") };
    assert_eq!(slices[0].successful_count, 2);
    assert_eq!(intervals.len(), 1);
    assert!(intervals[0].active_after <= intervals[0].active_until);

    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(9, system.id, 2, 4));
    let generation = crate::space::targeting::next_generation();
    let source = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
        generation, 1, 1, |_| true).unwrap();
    projection.scramble = Some(system.accept_projection_scramble(2, &projection, source).unwrap());
    system.upsert_projection(2, projection.clone());
    system.checkpoint_combat_scrambles().unwrap();
    assert!(system.projections_on(2).is_empty(), "full continuation must end the real projection");
    let at = system.combat_observed_time();
    let batch = system.combat_facts.as_mut().unwrap().capture(at).unwrap().unwrap();
    assert!(matches!(&batch.events()[0].event, GameEvent::WarpScrambleApplied { .. }));

    for mode in 0..3 {
        system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(10 + mode, system.id, 8, 4));
        let generation = crate::space::targeting::next_generation();
        let source = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
            generation, 1, 1, |_| true).unwrap();
        projection.until_ms = if mode == 0 { 0 } else { i64::MAX };
        projection.scramble = Some(system.accept_projection_scramble(2, &projection, source).unwrap());
        system.upsert_projection(2, projection.clone());
        match mode {
            0 => system.expire_projections(),
            1 => system.remove_projections_from(1),
            _ => system.remove_projections_on_departure(2),
        }
        assert!(system.projections_on(2).is_empty());
        let at = system.combat_fact_time();
        let batch = system.combat_facts.as_mut().unwrap().capture(at).unwrap().unwrap();
        let events = batch.events();
        let GameEvent::WarpScrambleApplied { slices, intervals, .. } = &events[0].event else { panic!("scramble fact") };
        assert_eq!(slices[0].successful_count, 1);
        assert_eq!(intervals.len(), 1);
        assert!(intervals[0].active_until <= at);
    }

    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(20, system.id, 2, 4));
    projection.until_ms = i64::MAX;
    let generation = crate::space::targeting::next_generation();
    let source = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
        generation, 1, 1, |_| true).unwrap();
    projection.scramble = Some(system.accept_projection_scramble(2, &projection, source).unwrap());
    system.upsert_projection(2, projection.clone());
    let next = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
        generation, 2, 1, |_| true).unwrap();
    assert!(matches!(system.accept_projection_scramble(2, &projection, next), Err(Refusal::Full)));
    system.finish_combat_attempt(&next);
    assert!(system.projections_on(2).is_empty());
    let at = system.combat_fact_time();
    let batch = system.combat_facts.as_mut().unwrap().capture(at).unwrap().unwrap();
    let events = batch.events();
    let GameEvent::WarpScrambleApplied { slices, .. } = &events[0].event else { panic!("scramble fact") };
    assert_eq!(slices[0].successful_count, 1);

    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(21, system.id, 8, 4));
    let generation = crate::space::targeting::next_generation();
    let source = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
        generation, 1, 1, |_| true).unwrap();
    projection.scramble = Some(system.accept_projection_scramble(2, &projection, source).unwrap());
    system.upsert_projection(2, projection.clone());
    let start = system.combat_observed_time();
    system.nativefact_clock = start + 3 * infinity_core::consts::FILETIME_DAY;
    system.observe_combat_utc().unwrap();
    let end = system.combat_observed_time();
    assert!(!system.projections_on(2).is_empty());
    let batch = system.combat_facts.as_mut().unwrap().capture(end).unwrap().unwrap();
    let events = batch.events();
    let GameEvent::WarpScrambleApplied { intervals, .. } = &events[0].event else { panic!("scramble fact") };
    assert_eq!((intervals[0].active_after, intervals[0].active_until), (start, end));
    let next_day = (start / infinity_core::consts::FILETIME_DAY + 1) * infinity_core::consts::FILETIME_DAY;
    assert_eq!((end.min(next_day) - start).max(0), next_day - start);

    system.remove_projection(10);
    for mode in 0..3 {
        system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(30 + mode, system.id, 8, 4));
        projection.effect_id = 11;
        let generation = crate::space::targeting::next_generation();
        let source = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
            generation, 1, 1, |_| true).unwrap();
        projection.scramble = Some(system.accept_projection_scramble(2, &projection, source).unwrap());
        system.upsert_projection(2, projection.clone());
        let next_generation = if mode == 2 { crate::space::targeting::next_generation() } else { generation };
        let effect = if mode == 1 { 12 } else { 11 };
        let target = if mode == 0 { 3 } else { 2 };
        projection.effect_id = effect;
        let sequence = if mode == 2 { 1 } else { 2 };
        let source = system.prepare_combat_source(1, Some(10), Some(i64::from(effect)), None, FactOrigin::Natural,
            next_generation, sequence, 1, |_| true).unwrap();
        projection.scramble = Some(system.accept_projection_scramble(target, &projection, source).unwrap());
        system.upsert_projection(target, projection.clone());
        assert_eq!(system.projections_on(target).len(), 1);
        system.remove_projection(10);
        let at = system.combat_fact_time();
        let batch = system.combat_facts.as_mut().unwrap().capture(at).unwrap().unwrap();
        let events = batch.events();
        assert_eq!(events.len(), 2);
        for event in events {
            let GameEvent::WarpScrambleApplied { slices, intervals, .. } = &event.event else { panic!("scramble fact") };
            assert_eq!(slices[0].successful_count, 1);
            assert_eq!(intervals.len(), 1);
        }
    }

    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(40, system.id, 2, 4));
    let mut target_ball = system.ball(2).unwrap().clone();
    target_ball.char_id = Some(22);
    system.insert(target_ball);
    let fit = Fit::for_space_type(sde.clone(), ty, Vec::new());
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(2, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    projection.effect_id = 11;
    let generation = crate::space::targeting::next_generation();
    let source = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
        generation, 1, 1, |_| true).unwrap();
    projection.scramble = Some(system.accept_projection_scramble(2, &projection, source).unwrap());
    system.upsert_projection(2, projection.clone());
    system.close_combat_character_boundary(22).unwrap();
    assert!(system.projections_on(2).is_empty());
    let at = system.combat_fact_time();
    let batch = system.combat_facts.as_mut().unwrap().capture(at).unwrap().unwrap();
    assert!(matches!(&batch.events()[0].event, GameEvent::WarpScrambleApplied { .. }));

    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(43, system.id, 8, 4));
    let generation = crate::space::targeting::next_generation();
    let source = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
        generation, 1, 1, |_| true).unwrap();
    projection.scramble = Some(system.accept_projection_scramble(2, &projection, source).unwrap());
    system.upsert_projection(2, projection.clone());
    system.close_combat_character_boundary(22).unwrap();
    system.rebind_combat_character(22, Some(99), None);
    let handle = system.projections_on(2)[0].scramble.unwrap();
    let (_, target) = system.combat_facts.as_ref().unwrap().scramble_identity(handle).unwrap();
    assert_eq!(target.corporation_id, Some(99));
    system.remove_projection(10);

    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(41, system.id, 8, 4));
    projection.kind = EffectKind::Projected { jamming: JammingType::Webify };
    projection.scramble = None;
    system.upsert_projection(2, projection.clone());
    system.remove_projection(10);
    let at = system.combat_fact_time();
    assert!(system.combat_facts.as_mut().unwrap().capture(at).unwrap().is_none());

    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(42, system.id, 1, 4));
    projection.kind = EffectKind::Projected { jamming: JammingType::Scramble };
    let generation = crate::space::targeting::next_generation();
    let source = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
        generation, 1, 1, |_| true).unwrap();
    assert!(matches!(system.accept_projection_scramble(2, &projection, source), Err(Refusal::Full)));
    system.finish_combat_attempt(&source);
    assert!(system.projections_on(2).is_empty());
    let at = system.combat_fact_time();
    assert!(system.combat_facts.as_mut().unwrap().capture(at).unwrap().is_none());

    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(44, system.id, 8, 4));
    let mut source_ball = system.ball(1).unwrap().clone();
    source_ball.char_id = Some(11);
    system.insert(source_ball);
    let fit = Fit::for_space_type(sde.clone(), ty, Vec::new());
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(1, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    let generation = crate::space::targeting::next_generation();
    let pending = system.prepare_combat_source(1, Some(10), Some(11), None, FactOrigin::Natural,
        generation, 1, 1, |_| true).unwrap();
    system.close_combat_character_boundary(11).unwrap();
    system.take(1).unwrap();
    system.retire_combat_source(generation).unwrap();
    system.checkpoint_combat_scrambles().unwrap();
    assert!(system.combat_facts.as_ref().unwrap().export_character_scope(11).is_some());
    system.finish_combat_attempt(&pending);
    system.checkpoint_combat_scrambles().unwrap();
    assert!(system.combat_facts.as_ref().unwrap().export_character_scope(11).is_none());
}
