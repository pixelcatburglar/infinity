//! Dedicated SDE checks for effective ECM duration and projected sensor strength.
use super::*;
use crate::services::effects::{projection::Projection, registry, JammingType};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn fitted_jam_duration_overrides_cycle() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let (fit, effect, duration) = sde.types.iter().find_map(|ty| {
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
        let duration = fit.value(Key::Ship, JAM_DURATION);
        if !fit.type_has_attribute(Key::Ship, JAM_DURATION) || !duration.is_finite() || duration <= 0.0 { return None; }
        ty.effects().iter().find_map(|row| registry::resolve(&sde, row.effect_id)
            .filter(|effect| effect.kind == EffectKind::Ecm).map(|effect| (effect, duration)))
            .map(|(effect, duration)| (fit, effect, duration))
    }).expect("ECM type with jam duration");
    let src = source(&fit, Key::Ship, -1, -2, &effect, duration * 2.0);
    assert_eq!(src.duration, duration);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn projected_sensor_boost_reduces_fitted_ecm_chance() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let mut found = None;
    'search: for hull in sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP)) {
        let baseline = Fit::for_space_type(sde.clone(), hull.type_id, Vec::new());
        let Some((index, defense)) = primary(SENSORS.map(|a| baseline.value(Key::Ship, a))) else { continue };
        if !defense.is_finite() || defense <= 0.0 { continue; }
        for booster in sde.types.iter() {
            for row in booster.effects() {
                let Some(effect) = registry::resolve(&sde, row.effect_id) else { continue };
                if effect.kind != (EffectKind::Projected { jamming: JammingType::SensorBoost }) { continue; }
                let Some(dogma) = sde.dogma.effect(row.effect_id) else { continue };
                if !dogma.modifiers().iter().any(|m| m.domain == "targetID" && m.modified == SENSORS[index]) { continue; }
                let source_fit = Fit::for_space_type(sde.clone(), booster.type_id, Vec::new());
                let mut values = Vec::new();
                for modifier in dogma.modifiers().iter().filter(|m| m.domain == "targetID") {
                    if values.iter().any(|(attr, _)| *attr == modifier.modifying) { continue; }
                    let source = crate::sde::dogma::python_effects::projected_interim_source(modifier.modifying)
                        .unwrap_or(modifier.modifying);
                    values.push((modifier.modifying, source_fit.value(Key::Ship, source)));
                }
                let projection = Projection { source_ship: -1, source_module: -2, source_type: booster.type_id,
                    effect_id: row.effect_id, kind: effect.kind, until_ms: i64::MAX,
                    values, charge_values: Vec::new(), charge_type: None, scramble: None };
                let boosted = Fit::for_space_type(sde.clone(), hull.type_id, vec![projection]);
                let Some((boosted_index, boosted_defense)) = primary(SENSORS.map(|a| boosted.value(Key::Ship, a))) else { continue };
                if boosted_index != index || !boosted_defense.is_finite() || boosted_defense <= defense { continue; }
                for jammer in sde.types.iter() {
                    if !jammer.effects().iter().any(|e| registry::resolve(&sde, e.effect_id).is_some_and(|e| e.kind == EffectKind::Ecm)) { continue; }
                    let jammer_fit = Fit::for_space_type(sde.clone(), jammer.type_id, Vec::new());
                    let strength = jammer_fit.value(Key::Ship, JAM_STRENGTH[index]);
                    let before = chance(strength, defense, 1.0);
                    let after = chance(strength, boosted_defense, 1.0);
                    if before > 0.0 && after < before { found = Some((before, after)); break 'search; }
                }
            }
        }
    }
    let (before, after) = found.expect("SDE projected sensor boost and matching ECM source");
    assert!(after > 0.0 && after < before);
}
