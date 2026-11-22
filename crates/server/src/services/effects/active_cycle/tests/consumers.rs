//! In-space consumer scenarios: the assault damage control, signature radius
//! suppressor and emergency hull energizer through real Activate calls.
use super::*;
use crate::services::CallError;
use infinity_core::consts::dogma::{ATTR_DISALLOW_REPEATING_ACTIVATION, ATTR_HULL_DAMAGE, ATTR_IS_ONLINE, ATTR_REACTIVATION_DELAY, ATTR_WARP_BUBBLE_IMMUNE, EFFECT_INTERDICTION_NULLIFIER_BONUS};
use crate::services::fit::Key;

/// Assault damage control and signature radius suppressor share one shape:
/// start, bonus, list, end once, cool, refuse, admit after the delay.
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn bounded_activations_apply_end_and_delay_reactivation() {
    for effect in [EFFECT_ASSAULT_DAMAGE_CONTROL, EFFECT_TARGET_SPECTRUM_BREAKER_BONUS] {
        scenario(effect, async |space, sde, _db| {
            // The fixture pilot is an NPC-corporation character: session and
            // account carry no role or corporation authority.
            assert_eq!(space.client.session.int("role"), Some(0));
            let (system, module, ship) = (space.system, space.module, space.fixture.ship);
            let activate = activate_args(space, &effect_name(sde, effect));
            let watched = watched(sde, effect, module, ship);
            assert!(!watched.is_empty(), "consumer effect {effect} has no watched attribute");
            let mut baseline = Vec::new();
            for &(item, a) in &watched { baseline.push(fitted_attr(&mut space.client, item, a).await); }
            let _ = drain(&mut space.rx);
            effects_fixture::dogma_call(&mut space.client, "Activate", &activate).await.ok().expect("activate");
            until(&space.client.world, system, 5_000, || matches!(record(module), Record::Running)).await;
            let ev = drain(&mut space.rx);
            assert_eq!(godma(&ev, module, i64::from(effect), 1), 1, "start notification");
            for (&(item, a), &base) in watched.iter().zip(&baseline) { assert!(fitted_attr(&mut space.client, item, a).await < base, "effect {effect} must lower attribute {a}"); }
            assert!(active_effects(&mut space.client, module).await.contains(&i64::from(effect)), "running effect listed");
            until(&space.client.world, system, 5_000, || matches!(record(module), Record::Cooling { .. })).await;
            let ev = drain(&mut space.rx);
            assert_eq!(godma(&ev, module, i64::from(effect), 0), 1, "one stop notification");
            assert_eq!(godma(&ev, module, i64::from(effect), 1), 0, "no repeat start despite repeat=100");
            for (&(item, a), &base) in watched.iter().zip(&baseline) { assert_eq!(fitted_attr(&mut space.client, item, a).await, base, "attribute {a} restored"); }
            assert!(!active_effects(&mut space.client, module).await.contains(&i64::from(effect)), "cooling lists nothing");
            let Err(CallError::User { msg, dict }) = effects_fixture::dogma_call(&mut space.client, "Activate", &activate).await else { panic!("cooling activation must refuse") };
            assert_eq!(msg, messages::MSG_MODULE_REACTIVATION_DELAYED);
            assert!(dict.iter().find(|(k, _)| k == "delay").and_then(|(_, v)| v.as_float()).is_some_and(|d| d > 0.0));
            let Record::Cooling { deadline } = record(module) else { panic!("still cooling") };
            until(&space.client.world, system, 5_000, || now_ms() >= deadline).await;
            effects_fixture::dogma_call(&mut space.client, "Activate", &activate).await.ok().expect("reactivate after delay");
            until(&space.client.world, system, 5_000, || matches!(record(module), Record::Running)).await;
        }).await;
    }
}

/// The energizer burns itself to effective hit points once, keeps the bonus
/// and stays online for the whole accepted duration, then stays burnt; the
/// refused re-burn emits nothing, applies no bonus, keeps the same Cooling
/// record and leaves the capacitor untouched.
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn emergency_hull_energizer_burns_stays_burnt_and_refuses_reuse() {
    scenario(EFFECT_EMERGENCY_HULL_ENERGIZER, async |space, sde, db| {
        let (system, module, ship) = (space.system, space.module, space.fixture.ship);
        let activate = activate_args(space, &effect_name(sde, EFFECT_EMERGENCY_HULL_ENERGIZER));
        let watched = watched(sde, EFFECT_EMERGENCY_HULL_ENERGIZER, module, ship);
        assert!(!watched.is_empty(), "energizer has no watched attribute");
        let hp = fitted_attr(&mut space.client, module, ATTR_STRUCTURE_HP).await;
        let mut baseline = Vec::new();
        for &(item, a) in &watched { baseline.push(fitted_attr(&mut space.client, item, a).await); }
        let _ = drain(&mut space.rx);
        effects_fixture::dogma_call(&mut space.client, "Activate", &activate).await.ok().expect("activate");
        until(&space.client.world, system, 5_000, || matches!(record(module), Record::Running)).await;
        let burn = item_dogma::overrides(db, module).await.unwrap().into_iter()
            .find(|r| r.attribute_id == ATTR_HULL_DAMAGE).map(|r| r.value).expect("activation burn");
        assert!((burn - hp).abs() < 1e-9, "energizer burns to effective hit points once");
        let ev = drain(&mut space.rx);
        assert_eq!(godma(&ev, module, i64::from(EFFECT_EMERGENCY_HULL_ENERGIZER), 1), 1, "start notification");
        // `ShipGetInfo` runs the burnt reconcile; the running activation must
        // keep its burned module online and its bonus live for the duration.
        assert_eq!(fitted_attr(&mut space.client, module, ATTR_IS_ONLINE).await, 1.0, "burned module stays online while running");
        for (&(item, a), &base) in watched.iter().zip(&baseline) { assert!(fitted_attr(&mut space.client, item, a).await < base, "energizer must lower resonance {a}"); }
        until(&space.client.world, system, 5_000, || matches!(record(module), Record::Cooling { .. })).await;
        let Record::Cooling { deadline } = record(module) else { panic!("cooling") };
        let ev = drain(&mut space.rx);
        assert_eq!(godma(&ev, module, i64::from(EFFECT_EMERGENCY_HULL_ENERGIZER), 0), 1, "one stop notification");
        assert_eq!(godma(&ev, module, i64::from(EFFECT_EMERGENCY_HULL_ENERGIZER), 1), 0, "no repeat start despite repeat=100");
        let burn = item_dogma::overrides(db, module).await.unwrap().into_iter()
            .find(|r| r.attribute_id == ATTR_HULL_DAMAGE).map(|r| r.value).expect("burn persists");
        assert!((burn - hp).abs() < 1e-9, "the burn stays after the stop");
        // The burnt validation refuses a second activation: no start event,
        // no bonus, the same Cooling record and no capacitor charge.
        let cap = combat::groups_health::capacitor_snapshot(db, &space.client.world, system, ship).await.unwrap().expect("capacitor").level;
        let Err(CallError::User { msg, dict }) = effects_fixture::dogma_call(&mut space.client, "Activate", &activate).await else { panic!("unrepaired activation must refuse") };
        assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
        assert!(dict.iter().any(|(k, v)| k == "notify" && *v == Value::unicode(messages::heat::BURNT_OUT)));
        let ev = drain(&mut space.rx);
        assert_eq!(godma(&ev, module, i64::from(EFFECT_EMERGENCY_HULL_ENERGIZER), 1), 0, "refused burn emits no start");
        assert_eq!(record(module), Record::Cooling { deadline }, "refused burn keeps the cooldown");
        for (&(item, a), &base) in watched.iter().zip(&baseline) { assert_eq!(fitted_attr(&mut space.client, item, a).await, base, "refused burn applies no bonus to {a}"); }
        let after = combat::groups_health::capacitor_snapshot(db, &space.client.world, system, ship).await.unwrap().expect("capacitor").level;
        assert!(after >= cap, "refused burn must not debit the capacitor: before {cap}, after {after}");
    }).await;
}

/// The nullifier runs exactly one real-SDE cycle and then its real
/// 100-second cooldown, whatever repeat count the client sent; its
/// `warpBubbleImmune` bonus raises rather than lowers the watched attribute.
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn nullifier_uses_single_cycle_and_sde_cooldown() {
    scenario(EFFECT_INTERDICTION_NULLIFIER_BONUS, async |space, sde, db| {
        let (system, module, ship) = (space.system, space.module, space.fixture.ship);
        let type_id = module_type(sde, EFFECT_INTERDICTION_NULLIFIER_BONUS);
        let row = sde.types.get(type_id).expect("nullifier module type");
        let duration_attr = sde.dogma.effect(EFFECT_INTERDICTION_NULLIFIER_BONUS)
            .and_then(|e| e.duration_attribute_id).expect("nullifier duration attribute");
        let (duration, delay, disallow) = (
            row.attribute(duration_attr).expect("SDE duration"),
            row.attribute(ATTR_REACTIVATION_DELAY).expect("SDE reactivation delay"),
            row.attribute(ATTR_DISALLOW_REPEATING_ACTIVATION).expect("SDE disallow flag"));
        assert_eq!((duration, delay, disallow), (10_000.0, 100_000.0, 1.0), "real SDE nullifier timings");
        // The shared scenario shrank all three for bounded tests; restore the
        // real values so the assertions measure the production timings.
        for (attribute, value) in [(duration_attr, duration), (ATTR_REACTIVATION_DELAY, delay), (ATTR_DISALLOW_REPEATING_ACTIVATION, disallow)] {
            item_dogma::set_override(db, module, attribute, value).await.unwrap();
        }
        effects_fixture::publish_fit(db, &space.client.world, system, space.fixture.character, ship).await;
        let activate = activate_args(space, &effect_name(sde, EFFECT_INTERDICTION_NULLIFIER_BONUS));
        let watched = watched(sde, EFFECT_INTERDICTION_NULLIFIER_BONUS, module, ship);
        assert_eq!(watched, vec![(ship, ATTR_WARP_BUBBLE_IMMUNE)], "nullifier raises only warpBubbleImmune");
        // ShipGetInfo only emits attributes a type declares; warpBubbleImmune is
        // observed on the actor's resident fit, the same read the field's own
        // immunity check performs.
        let immune_of = move |s: &mut crate::space::System| s.with_combat_fit(ship, |fit, _, _| fit.value(Key::Ship, ATTR_WARP_BUBBLE_IMMUNE));
        let baseline = space.client.world.with(system, immune_of).await.flatten().expect("resident fit");
        let _ = drain(&mut space.rx);
        effects_fixture::dogma_call(&mut space.client, "Activate", &activate).await.ok().expect("activate");
        until(&space.client.world, system, 5_000, || matches!(record(module), Record::Running)).await;
        let ev = drain(&mut space.rx);
        assert_eq!(godma(&ev, module, i64::from(EFFECT_INTERDICTION_NULLIFIER_BONUS), 1), 1, "start notification");
        assert!(space.client.world.with(system, immune_of).await.flatten().expect("resident fit") > baseline, "immunity applies while running");
        until(&space.client.world, system, 15_000, || matches!(record(module), Record::Cooling { .. })).await;
        let ev = drain(&mut space.rx);
        assert_eq!(godma(&ev, module, i64::from(EFFECT_INTERDICTION_NULLIFIER_BONUS), 0), 1, "one stop notification");
        assert_eq!(godma(&ev, module, i64::from(EFFECT_INTERDICTION_NULLIFIER_BONUS), 1), 0, "no repeat start despite repeat=100");
        assert_eq!(space.client.world.with(system, immune_of).await.flatten().expect("resident fit"), baseline, "immunity restores at the stop");
        let Some(&combat::ActiveCycle::Cooling { stopped, deadline }) = combat::state().active_cycles.get(&module) else { panic!("cooling record") };
        assert_eq!(deadline - stopped, 100_000, "cooling runs the real SDE delay");
        let Err(CallError::User { msg, dict }) = effects_fixture::dogma_call(&mut space.client, "Activate", &activate).await else { panic!("cooling activation must refuse") };
        assert_eq!(msg, messages::MSG_MODULE_REACTIVATION_DELAYED);
        assert!(dict.iter().find(|(k, _)| k == "delay").and_then(|(_, v)| v.as_float()).is_some_and(|d| d > 0.0));
    }).await;
}
