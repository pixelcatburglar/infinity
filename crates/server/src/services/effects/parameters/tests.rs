use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn capture_source_reports_real_values_for_entity_kinds() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let entity = |wanted: fn(EffectKind) -> bool, fitted: fn(&Fit) -> bool| sde.types.iter()
        .filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_ENTITY)).find_map(|ty| {
            let fit = Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
            if !fitted(&fit) { return None; }
            // NPC missile behavior is selected by the shared missile effect.
            let effect = ty.effects().iter().find_map(|row|
                super::super::registry::resolve(&sde, row.effect_id).filter(|effect| wanted(effect.kind)))
                .or_else(|| wanted(EffectKind::Missile)
                    .then(|| super::super::registry::resolve(&sde, infinity_core::consts::dogma::EFFECT_USE_MISSILES))
                    .flatten());
            effect.map(|effect| (fit, effect))
        });
    let (fit, effect) = entity(|k| k == EffectKind::Turret, |fit| fit.value(Key::Ship, ATTR_MAX_RANGE) > 0.0
        && ATTR_DAMAGE.iter().any(|a| fit.value(Key::Ship, *a) > 0.0)).expect("SDE turret entity");
    let SourceInputs::Turret(inputs) = capture_source(&fit, Key::Ship, &effect, 0.0).expect("turret capture") else { panic!("turret packet") };
    assert_eq!(inputs.parameters, (fit.value(Key::Ship, ATTR_MAX_RANGE), fit.value(Key::Ship, ATTR_FALLOFF),
        fit.value(Key::Ship, ATTR_TRACKING_SPEED), fit.value(Key::Ship, ATTR_OPTIMAL_SIG_RADIUS)));
    assert_eq!(inputs.raw, ATTR_DAMAGE.map(|id| fit.value(Key::Ship, id) * fit.value(Key::Ship, ATTR_DAMAGE_MULTIPLIER).max(0.0)));
    let (fit, effect) = entity(|k| k == EffectKind::Missile, |fit| fit.value(Key::Ship, ATTR_ENTITY_MISSILE_TYPE) > 0.0).expect("SDE missile entity");
    let SourceInputs::Missile(inputs) = capture_source(&fit, Key::Ship, &effect, 0.0).expect("missile capture") else { panic!("missile packet") };
    let charge = fit.value(Key::Ship, ATTR_ENTITY_MISSILE_TYPE) as i32;
    let attr = |id| sde.types.attribute(charge, id).or_else(|| sde.dogma.attribute(id).and_then(|a| a.default_value)).unwrap_or(0.0);
    assert_eq!(inputs.charge, Some(charge));
    assert_eq!(inputs.raw, ATTR_DAMAGE.map(|id| attr(id) * fit.value(Key::Ship, ATTR_MISSILE_DAMAGE_MULTIPLIER)));
    assert_eq!(inputs.velocity, (attr(ATTR_MAX_VELOCITY) * fit.value(Key::Ship, ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER)).max(1.0));
    assert_eq!(inputs.flight_time_ms, attr(ATTR_EXPLOSION_DELAY) * fit.value(Key::Ship, ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER));
    let (fit, effect) = entity(|k| k == EffectKind::Ecm, |_| true).expect("SDE ECM entity");
    let SourceInputs::Ecm(inputs) = capture_source(&fit, Key::Ship, &effect, 4200.0).expect("ecm capture") else { panic!("ecm packet") };
    let duration = sde.dogma.attributes_by_name("ecmJamDuration").next().map(|attribute| attribute.attribute_id)
        .and_then(|attr| fit.type_has_attribute(Key::Ship, attr).then(|| fit.value(Key::Ship, attr)))
        .filter(|v| v.is_finite() && *v > 0.0);
    assert_eq!(inputs.duration_ms, duration.unwrap_or(4200.0));
    assert_eq!(inputs.ranges, ranges(&fit, Key::Ship, &effect));
    assert_eq!(inputs.strengths, super::super::ecm::JAM_STRENGTH.map(|attr| fit.value(Key::Ship, attr)));
    assert_eq!(inputs.owner_projection, None);
    let (fit, effect) = entity(|k| k == EffectKind::Vorton, |_| true).expect("SDE vorton entity");
    let SourceInputs::Vorton(inputs) = capture_source(&fit, Key::Ship, &effect, 0.0).expect("vorton capture") else { panic!("vorton packet") };
    assert_eq!(inputs.raw, ATTR_DAMAGE.map(|id| fit.value(Key::Ship, id) * fit.value(Key::Ship, ATTR_DAMAGE_MULTIPLIER).max(0.0)));
    assert_eq!(inputs.aoe_cloud, fit.value(Key::Ship, ATTR_AOE_CLOUD_SIZE));
    assert_eq!(inputs.aoe_velocity, fit.value(Key::Ship, ATTR_AOE_VELOCITY));
    assert_eq!(inputs.drf, fit.value(Key::Ship, ATTR_AOE_DAMAGE_REDUCTION_FACTOR));
    assert_eq!(inputs.arc_range, fit.value(Key::Ship, super::super::vorton::ATTR_VORTON_ARC_RANGE));
    assert_eq!(inputs.arc_targets, fit.value(Key::Ship, super::super::vorton::ATTR_VORTON_ARC_TARGETS));
    let stale = vec![(sde.types.iter().next().expect("SDE type").type_id, 5_i64)];
    assert!(super::super::fuel::requirements(&fit, &[], stale).expect("fuel collection").is_empty());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn turret_capture_arms_spool_only_for_the_disintegrator_effect() {
    use crate::services::fit::attributes::tests::module_fit;
    use infinity_core::consts::{CATEGORY_MODULE, dogma::EFFECT_TARGET_ATTACK};
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let disintegrator = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_TARGET_DISINTEGRATOR_ATTACK)).expect("SDE disintegrator").type_id;
    let fit = module_fit(sde.clone(), disintegrator, disintegrator, true);
    let module = Module { id: 2, type_id: disintegrator, flag: 0 };
    let effect = |id| super::super::registry::resolve(&sde, id).unwrap();
    assert_eq!(module_turret(&fit, &module, &effect(EFFECT_TARGET_DISINTEGRATOR_ATTACK), false).spool, Some(SpoolParameters {
        step: fit.value(Key::Module(2), ATTR_DAMAGE_MULTIPLIER_BONUS_PER_CYCLE), maximum: fit.value(Key::Module(2), ATTR_DAMAGE_MULTIPLIER_BONUS_MAX) }));
    let attack = effect(EFFECT_TARGET_ATTACK);
    assert!(module_turret(&fit, &module, &attack, false).spool.is_none() && module_turret(&fit, &module, &attack, true).spool.is_none());
    assert!(source_turret(&fit, Key::Ship).spool.is_none());
}
