//! Actor admission for launched probes: two probes keep separate fields after
//! the firing ship leaves, one lethal or stale terminal retires a probe once,
//! and a stasis pulse lands the published web through the actor timers.
use super::*;
use crate::services::effects::probes::{ProbeInputs, ProbePayload};
use crate::services::fit::{attributes::CommittedFitting, Key, OP_POST_PERCENT};
use crate::space::combat::inputs::ChargeToken;
use crate::space::combat::loaded::LoadedPermit;
use infinity_core::consts::dogma::{ATTR_MAX_VELOCITY, ATTR_STASIS_WEBIFIER_RESISTANCE, ATTR_WARP_BUBBLE_IMMUNE};

const SHIP: i32 = 1;
const CHARGE: i32 = 22778;
const MODULE: i64 = 2;
const COLLECTION: i32 = 27;
const NORMAL: f64 = 1.0;
const RESISTANT: f64 = 0.5;
const SPEED_FACTOR: f64 = -20.0;
const WARNING_MS: f64 = 1_000.0;
const DURATION_MS: f64 = 30_000.0;
const RADIUS: f64 = 15_000.0;

fn ship_overrides(resistance: f64, immune: f64) -> CommittedFitting {
    CommittedFitting::overridden_attributes(vec![
        (Key::Ship, ATTR_MAX_VELOCITY, Some(100.0)),
        (Key::Ship, ATTR_STASIS_WEBIFIER_RESISTANCE, Some(resistance)),
        (Key::Ship, ATTR_WARP_BUBBLE_IMMUNE, Some(immune)),
    ])
}

fn web_collection() -> String {
    format!(r#"{{"_key":{COLLECTION},"aggregateMode":"Minimum","operationName":"PostPercent","itemModifiers":[{{"dogmaAttributeID":{ATTR_MAX_VELOCITY}}}]}}"#)
}

fn scene() -> System {
    crate::tests::init_config();
    let combat = crate::sde::types::tests::combat_fixture();
    let mut sde = std::sync::Arc::try_unwrap(crate::sde::types::tests::attribute_fixture(
        &[(CHARGE, Some(548))],
        &[(CHARGE, combat.types.attributes(SHIP).collect())],
    )).ok().unwrap();
    sde.dbuffs = crate::sde::dbuff::fixture::collections(vec![crate::dao::sde::dbuff::Row {
        key: Some(COLLECTION.to_string()), data: web_collection(),
    }]).unwrap();
    let sde = std::sync::Arc::new(sde);
    let mut system = System::default();
    system.projection_sde = Some(sde.clone());
    system.insert(ship_ball(1));
    let mut fit = crate::services::fit::attributes::tests::charged_owner_fit(sde, 1, CHARGE, 2);
    assert!(fit.apply_committed_fitting(&ship_overrides(NORMAL, 0.0)));
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(1, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: crate::client::filetime_now(),
    }, fit);
    system
}

fn install(system: &mut System, ship: i64, resistance: f64, immune: f64) {
    let sde = system.projection_sde.clone().expect("projection sde");
    let mut fit = crate::services::fit::attributes::tests::owner_fit(sde, ship);
    assert!(fit.apply_committed_fitting(&ship_overrides(resistance, immune)));
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(ship, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: crate::client::filetime_now(),
    }, fit);
}

fn ship_ball(id: i64) -> Ball {
    Ball { id, type_id: SHIP, owner_id: id, corp_id: 1, alliance_id: -1, char_id: Some(id),
        signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0, max_vel: 0.0, agility: 1.0,
        speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true,
        global: false, massive: false, interactive: true, warp: WarpState::default(), last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

fn permit(system: &mut System) -> (LoadedPermit, ChargeToken) {
    let permit = system.hold_loaded_use(1, MODULE).expect("loaded use");
    let token = system.fitted_charge_token(1, MODULE).expect("charge token");
    (permit, token)
}

fn warp_inputs() -> ProbeInputs {
    ProbeInputs { charge: CHARGE, lifetime_ms: 120_000.0, payload: ProbePayload::WarpField { radius: 20_000.0 } }
}

fn stasis_inputs() -> ProbeInputs {
    ProbeInputs { charge: CHARGE, lifetime_ms: 0.0, payload: ProbePayload::StasisPulse {
        warning_ms: WARNING_MS, radius: RADIUS, duration_ms: DURATION_MS, collection_id: COLLECTION, op: OP_POST_PERCENT,
        speed_factor: SPEED_FACTOR, resistance_attribute: ATTR_STASIS_WEBIFIER_RESISTANCE,
    } }
}

fn velocity(system: &mut System, ship: i64) -> f64 {
    system.with_combat_fit(ship, |fit, _, _| fit.value(Key::Ship, ATTR_MAX_VELOCITY)).unwrap()
}

#[test]
fn two_probes_keep_separate_fields_after_the_source_leaves() {
    let mut system = scene();
    let (first_permit, first_token) = permit(&mut system);
    let born = system.admit_probe(1, MODULE, 1, first_permit, first_token, warp_inputs()).expect("first probe");
    let (second_permit, second_token) = permit(&mut system);
    let other = system.admit_probe(1, MODULE, 1, second_permit, second_token, ProbeInputs {
        lifetime_ms: 180_000.0, payload: ProbePayload::WarpField { radius: 10_000.0 }, ..warp_inputs()
    }).expect("second probe");
    assert_ne!(born.0, other.0);
    assert!(system.interdicted(1));
    system.remove_ball(1);
    assert!(system.contains(born.0) && system.contains(other.0) && system.interdicted(born.0));
    system.end_projectile(born.0);
    assert!(!system.contains(born.0) && system.contains(other.0) && system.interdicted(other.0));
    system.tick(179_999);
    assert!(system.contains(other.0) && system.interdicted(other.0));
    system.tick(180_000);
    assert!(!system.contains(other.0) && !system.interdicted(other.0));
}

#[test]
fn stale_token_does_not_pulse_a_removed_probe() {
    let mut system = scene();
    let (loaded, token) = permit(&mut system);
    let (id, _) = system.admit_probe(1, MODULE, 1, loaded, token, stasis_inputs()).expect("stasis probe");
    let current = system.capture_ball(id).expect("probe token");
    system.end_projectile(id);
    system.tick(WARNING_MS as i64);
    system.stasis_pulse(current);
    assert!(!system.contains(id));
    assert!(system.stasis_inputs(id).is_none());
    assert_eq!(velocity(&mut system, 1), 100.0);
}

#[test]
fn stasis_pulse_lands_the_published_web_and_restores_it() {
    let mut system = scene();
    system.insert(ship_ball(2));
    install(&mut system, 2, RESISTANT, 1.0);
    system.insert(Ball { id: 3, pos: [100_000.0, 0.0, 0.0], ..ship_ball(3) });
    install(&mut system, 3, NORMAL, 0.0);
    system.insert(Ball { id: 4, pos: [100_000.0, 0.0, 0.0], ..ship_ball(4) });
    install(&mut system, 4, NORMAL, 0.0);
    let (loaded, token) = permit(&mut system);
    let (id, _) = system.admit_probe(1, MODULE, 1, loaded, token, stasis_inputs()).expect("stasis probe");
    system.command(4, |ball, _, _| { ball.pos = [0.0; 3]; Vec::new() });
    system.tick(WARNING_MS as i64 - 1);
    assert!(system.contains(id) && velocity(&mut system, 2) == 100.0);
    system.remove_ball(1);
    system.tick(WARNING_MS as i64);
    assert!(!system.contains(id));
    assert_eq!((velocity(&mut system, 2), velocity(&mut system, 3), velocity(&mut system, 4)), (90.0, 100.0, 80.0));
    system.tick(WARNING_MS as i64 + DURATION_MS as i64);
    assert_eq!(velocity(&mut system, 2), 90.0);
    system.tick(WARNING_MS as i64 + DURATION_MS as i64 + 1);
    assert_eq!((velocity(&mut system, 2), velocity(&mut system, 4)), (100.0, 100.0));
}

#[test]
fn one_unfitted_ship_does_not_cancel_the_pulse() {
    let mut system = scene();
    system.insert(ship_ball(2));
    let (loaded, token) = permit(&mut system);
    let (id, _) = system.admit_probe(1, MODULE, 1, loaded, token, stasis_inputs()).expect("stasis probe");
    system.tick(WARNING_MS as i64);
    assert!(!system.contains(id));
    assert_eq!(velocity(&mut system, 1), 80.0);
}

#[test]
fn projectile_damage_retires_the_probe_once() {
    let mut system = scene();
    let (loaded, token) = permit(&mut system);
    let (id, _) = system.admit_probe(1, MODULE, 1, loaded, token, warp_inputs()).expect("warp probe");
    let outcome = system.apply_projectile_damage(id, [0.0, 0.0, 0.0, 1_000.0]).expect("damage");
    assert!(outcome.destroyed);
    assert!(!system.contains(id) && !system.projectile_damageable(id));
    assert!(system.apply_projectile_damage(id, [0.0, 0.0, 0.0, 1_000.0]).is_none());
}

#[test]
fn invalid_payload_keeps_the_loaded_charge() {
    let mut system = scene();
    let (loaded, token) = permit(&mut system);
    let refused = system.admit_probe(1, MODULE, 1, loaded, token, ProbeInputs {
        payload: ProbePayload::WarpField { radius: 0.0 }, ..warp_inputs()
    });
    assert!(refused.is_none());
    assert_eq!(system.loaded_effective(1, MODULE).map(|(_, _, quantity, _)| quantity), Some(2));
    assert_eq!(system.balls().filter(|ball| ball.type_id == CHARGE).count(), 0);
}

#[test]
fn an_unrepresentable_timer_keeps_the_loaded_charge() {
    let mut system = scene();
    let (loaded, token) = permit(&mut system);
    let mut payload = stasis_inputs();
    let ProbePayload::StasisPulse { duration_ms, .. } = &mut payload.payload else { unreachable!() };
    *duration_ms = f64::MAX;
    let refused = system.admit_probe(1, MODULE, 1, loaded, token, payload);
    assert!(refused.is_none());
    assert_eq!(system.loaded_effective(1, MODULE).map(|(_, _, quantity, _)| quantity), Some(2));
}


