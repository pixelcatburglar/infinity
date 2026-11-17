use super::*;
use crate::db::outbox::{Effect as OutboxEffect, Outbox};
use crate::services::identity::Identity;
use infinity_core::consts::dogma::EFFECT_ONLINE;
use std::sync::Mutex;
mod refund_db;
mod forced_retire;
mod banks_db;
mod boundary;
mod event_guards;
mod remote_facts;
mod actor_fit;
mod civilian_weapon;
mod active_attributes;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn identity_lifecycle_requires_explicit_space_ship_and_character() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let module = -9041;
    let identities = [
        (Identity { char_id: 1, ship_id: Some(1), corp_id: 1, station_id: None, system_id: Some(1) }, None),
        (Identity { char_id: 1, ship_id: None, corp_id: 1, station_id: None, system_id: Some(1) }, Some(1)),
        (Identity { char_id: 0, ship_id: Some(1), corp_id: 1, station_id: None, system_id: Some(1) }, Some(1)),
    ];
    for (identity, system) in identities {
        let mut notifications = 0;
        let result = activate(&identity, &db, &world, &sde, system, sender.clone(), 1, module, "", None, 0, |_| {
            notifications += 1;
            Ok(())
        }).await;
        assert!(matches!(result, Err(CallError::User { msg, dict }) if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))]));
        let result = deactivate(&identity, &db, &world, &sde, system, sender.clone(), 1, module, "", |_| {
            notifications += 1;
            Ok(())
        }).await;
        assert!(matches!(result, Err(CallError::User { msg, dict }) if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))]));
        assert_eq!(notifications, 0);
        assert!(receiver.try_recv().is_err());
        let st = state();
        assert!(!st.active.contains_key(&module));
        assert!(!st.activation_generation.contains_key(&module));
    }
}

#[test]
fn blocker_uses_target_modifier_source_not_scramble_identity() {
    let mut row = crate::sde::dogma::Modifier {
        func: "LocationRequiredSkillModifier".into(), domain: "targetID".into(), op: 2,
        modified: ATTR_ACTIVATION_BLOCKED, modifying: 2509, group: None, skill: None, target_category: None,
    };
    assert!(!blocker_rows(&[(2509, 0.0)], std::slice::from_ref(&row)));
    assert!(blocker_rows(&[(2509, 1.0)], std::slice::from_ref(&row)));
    assert!(!blocker_rows(&[(1350, 1.0)], std::slice::from_ref(&row)));
    row.domain = "shipID".into();
    assert!(!blocker_rows(&[(2509, 1.0)], &[row]));
}

#[test]
fn fuel_discharge_and_empty_booster_policy() {
    let shield = EffectKind::FueledLocalRepair { layer: 0, amount_attr: 1, charge_bonus_attr: None };
    let armor = EffectKind::FueledLocalRepair { layer: 1, amount_attr: 1, charge_bonus_attr: Some(2) };
    assert_eq!(cycle_discharge(shield, true, 20.0), 0.0);
    assert_eq!(cycle_discharge(shield, false, 20.0), 20.0);
    assert_eq!(cycle_discharge(armor, true, 20.0), 20.0);
    assert_eq!(cycle_discharge(armor, false, 20.0), 20.0);
    assert!(needs_charge(EffectKind::CapBooster));
    assert!(!needs_charge(shield));
    assert!(charge_sensitive(armor));
    assert!(!charge_sensitive(EffectKind::Neutralizer));
    for kind in [EffectKind::Bomb, EffectKind::Vorton] {
        assert!(needs_charge(kind));
        assert!(charge_sensitive(kind));
        assert_eq!(cycle_discharge(kind, true, 20.0), 20.0);
    }
}

#[test]
fn completed_cycle_retirement_preserves_committed_action_generation() {
    use crate::space::{Ball, WarpState, MODE_STOP};
    crate::tests::init_config();
    let mut system = System::new(1, Arc::default(), 1000);
    system.insert(Ball {
        id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1e6,
        max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], old_pos: [0.0; 3],
        old_vel: [0.0; 3], segment_acceleration: None, follow_id: 0, follow_range: 0.0,
        free: true, global: false, interactive: false, massive: true,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    });
    let generation = Arc::new(AtomicBool::new(false));
    let guard = generation.clone();
    system.set_ball_position(1, [10.0, 20.0, 30.0], move |_, _| !guard.load(Ordering::Relaxed));
    let mut active = HashMap::new();
    active.insert(1, ActiveModule { char_id: 1, stop: generation.clone(), forced_inactive: false });
    assert!(retire_activation(&mut active, 1, &generation, || {}));
    system.tick(2000);
    assert_eq!(system.ball(1).unwrap().pos, [10.0, 20.0, 30.0]);
}

#[test]
fn ancillary_remote_uses_local_charge_and_capacitor_policy() {
    let shield = charge_kind(i64::from(EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER), EffectKind::RemoteRepair { layer: 0 });
    let armor = charge_kind(i64::from(EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER), EffectKind::RemoteRepair { layer: 1 });
    assert!(charge_sensitive(shield));
    assert!(charge_sensitive(armor));
    assert_eq!(cycle_discharge(shield, true, 20.0), 0.0);
    assert_eq!(cycle_discharge(shield, false, 20.0), 20.0);
    assert_eq!(cycle_discharge(armor, true, 20.0), 20.0);
    assert!(matches!(armor, EffectKind::FueledLocalRepair { charge_bonus_attr: Some(ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER), .. }));
    assert!(!charge_sensitive(charge_kind(0, EffectKind::RemoteRepair { layer: 1 })));
}
#[test]
fn discarded_cycle_owners_release_only_their_generation() {
    let master = -9011;
    let generation = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        assert!(!st.active.contains_key(&master));
        st.active.insert(master, ActiveModule { char_id: 1, stop: generation.clone(), forced_inactive: false });
        st.activation_generation.insert(master, generation.clone());
    }
    let owner = ActivationOwner { master, generation: generation.clone(), armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 };
    let mut system = System::default();
    schedule_cycle_end(&mut system, 0, 1000.0, move |_, _| drop(owner));
    drop(system);
    {
        let mut st = state();
        assert!(!st.active.contains_key(&master));
        assert!(!st.activation_generation.contains_key(&master));
        st.active.insert(master, ActiveModule { char_id: 1, stop: generation.clone(), forced_inactive: false });
        st.activation_generation.insert(master, generation.clone());
    }
    let owner = ActivationOwner { master, generation: generation.clone(), armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 };
    let system = System::default();
    system.defer(async move { drop(owner); });
    {
        let st = state();
        assert!(!st.active.contains_key(&master));
        assert!(!st.activation_generation.contains_key(&master));
    }
    {
        let mut st = state();
        st.active.insert(master, ActiveModule { char_id: 1, stop: generation.clone(), forced_inactive: false });
        st.activation_generation.insert(master, generation.clone());
    }
    let owner = ActivationOwner { master, generation: generation.clone(), armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 };
    let rejected_post = move |_: &mut System| drop(owner);
    drop(rejected_post);
    {
        let st = state();
        assert!(!st.active.contains_key(&master));
        assert!(!st.activation_generation.contains_key(&master));
    }

    let replacement = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        st.active.insert(master, ActiveModule { char_id: 1, stop: replacement.clone(), forced_inactive: false });
        st.activation_generation.insert(master, replacement.clone());
    }
    let owner = ActivationOwner { master, generation, armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 };
    let mut system = System::default();
    schedule_cycle_end(&mut system, 0, 1000.0, move |_, _| drop(owner));
    drop(system);
    let mut st = state();
    assert!(Arc::ptr_eq(&st.active[&master].stop, &replacement));
    assert!(Arc::ptr_eq(&st.activation_generation[&master], &replacement));
    st.active.remove(&master);
    st.activation_generation.remove(&master);
}


#[test]
fn cycle_deadlines_and_deactivation() {
    crate::tests::init_config();
    let mut s = System::default();
    let completed = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let seen = completed.clone();
    let flag = stop.clone();
    schedule_cycle_end(&mut s, 1000, 500.0, move |s, end| {
        seen.lock().unwrap().push((1, s.now_ms(), end));
        assert!(!cycle_is_final(flag.load(Ordering::Relaxed), false, 3, 1));
        assert!(cycle_is_final(false, true, 3, 1));
        // Model a continuation returning late from the worker.
        s.schedule_at(1700, move |s| {
            assert!(!cycle_should_stop(false, &flag));
            schedule_cycle_end(s, end, 500.0, move |s, end| {
                seen.lock().unwrap().push((2, s.now_ms(), end));
                assert!(cycle_is_final(flag.load(Ordering::Relaxed), false, 3, 2));
                assert!(cycle_should_stop(false, &flag));
            });
        });
    });
    s.tick(1499);
    assert!(completed.lock().unwrap().is_empty());
    s.tick(1500);
    s.tick(1700);
    stop.store(true, Ordering::Relaxed);
    s.tick(1999);
    assert_eq!(*completed.lock().unwrap(), [(1, 1500, 1500)]);
    s.tick(2000);
    s.tick(3000);
    assert_eq!(*completed.lock().unwrap(), [(1, 1500, 1500), (2, 2000, 2000)]);

    let old = Arc::new(AtomicBool::new(false));
    let replacement = Arc::new(AtomicBool::new(false));
    let mut active = HashMap::from([(1, ActiveModule { char_id: 1, stop: replacement.clone(), forced_inactive: false })]);
    let mut published = false;
    assert!(!retire_activation(&mut active, 1, &old, || published = true));
    assert!(!published);
    assert!(Arc::ptr_eq(&active[&1].stop, &replacement));
    assert!(retire_activation(&mut active, 1, &replacement, || published = true));
    assert!(published);
    assert!(active.is_empty());
}

#[test]
fn subject_requires_owned_fitted_module_on_permitted_ship() {
    use infinity_core::consts::{FLAG_CARGO, FLAG_FITTING_FIRST};
    use crate::sde::types::TypeEffect;
    let mut hull = Item { item_id: 3, type_id: 1, owner_id: 2, location_id: 4, flag_id: FLAG_HANGAR, quantity: 1, singleton: true, group_id: 0, category_id: i64::from(CATEGORY_SHIP), custom_info: None };
    let mut module = Item { item_id: 5, type_id: 1, owner_id: 2, location_id: 3, flag_id: FLAG_FITTING_FIRST, quantity: 1, singleton: true, group_id: 0, category_id: i64::from(CATEGORY_MODULE), custom_info: None };
    let effects = [TypeEffect { effect_id: EFFECT_ONLINE as i32, is_default: false }];
    let valid = |h: &Item, m: &Item, active, station, entries: &[TypeEffect], require_online, online| valid_subject(h, m, 2, 3, active, station, entries, EFFECT_ONLINE as i32, require_online, online);
    assert!(valid(&hull, &module, Some(3), None, &effects, true, true));
    assert!(valid(&hull, &module, Some(9), Some(4), &effects, false, false));
    assert!(!valid(&hull, &module, Some(9), None, &effects, false, false));
    assert!(!valid(&hull, &module, Some(3), None, &[], false, false));
    assert!(!valid(&hull, &module, Some(3), None, &effects, true, false));
    hull.owner_id = 9;
    assert!(!valid(&hull, &module, Some(3), None, &effects, false, false));
    hull.owner_id = 2;
    module.owner_id = 9;
    assert!(!valid(&hull, &module, Some(3), None, &effects, false, false));
    module.owner_id = 2;
    module.location_id = 9;
    assert!(!valid(&hull, &module, Some(3), None, &effects, false, false));
    module.location_id = 3;
    module.flag_id = FLAG_CARGO;
    assert!(!valid(&hull, &module, Some(3), None, &effects, false, false));
    module.flag_id = FLAG_FITTING_FIRST;
    module.category_id = i64::from(CATEGORY_SHIP);
    assert!(!valid(&hull, &module, Some(3), None, &effects, false, false));
}

#[test]
fn bank_stop_dispatch_stops_current_cycle_after_commit() {
    let module = -9021;
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        st.active.insert(module, ActiveModule { char_id: 2, stop: stop.clone(), forced_inactive: false });
        st.activation_generation.insert(module, stop.clone());
    }
    let mut outbox = Outbox::default();
    outbox.operation(crate::services::combat::banks::capture_stop(module).expect("captured active generation"));
    assert!(!stop.load(Ordering::Relaxed));
    for entry in outbox.into_entries() {
        if let OutboxEffect::Operation(stop) = entry { crate::services::combat::banks::apply_stop(stop); }
    }
    assert!(stop.load(Ordering::Relaxed));
    let st = state();
    assert!(Arc::ptr_eq(&st.active[&module].stop, &stop));
    assert!(Arc::ptr_eq(&st.activation_generation[&module], &stop));
    drop(st);
    let mut st = state();
    st.active.remove(&module);
    st.activation_generation.remove(&module);
}

#[test]
fn passive_effect_list_is_not_online_capable() {
    let passive = [crate::sde::types::TypeEffect { effect_id: 1, is_default: false }];
    assert!(!has_effect(&passive, EFFECT_ONLINE as i32));
}

#[test]
fn activation_current_binds_character_stop_generation_and_inactive_flag() {
    let generation = Arc::new(AtomicBool::new(false));
    let other = Arc::new(AtomicBool::new(false));
    let module = -9_000_030_i64;
    {
        let mut st = crate::services::combat::state();
        st.active.insert(module, ActiveModule { char_id: 7, stop: generation.clone(), forced_inactive: false });
    }
    assert!(activation_current(module, 7, &generation));
    assert!(!activation_current(module, 8, &generation), "different pilot");
    assert!(!activation_current(module, 7, &other), "stale activation generation");
    {
        let mut st = crate::services::combat::state();
        st.active.get_mut(&module).unwrap().forced_inactive = true;
    }
    assert!(!activation_current(module, 7, &generation), "forced-inactive module");
    crate::services::combat::state().active.remove(&module);
}

// Superweapon source lockouts: armed once per accepted firing, refusing until
// their deadlines regardless of activation stop, and bounded by the expiry
// timer, replacement and ball departure.
fn sw_ball(id: i64, character: Option<i64>, x: f64) -> crate::space::Ball {
    crate::space::Ball { id, type_id: 1, owner_id: character.unwrap_or(1), corp_id: 1, alliance_id: -1,
        char_id: character, signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1e6,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [x, 0.0, 0.0], vel: [0.0; 3],
        old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0,
        follow_range: 0.0, free: true, global: false, interactive: character.is_some(), massive: true,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

fn sw_spec(ship: i64, character: i64, master: i64, generation: &Arc<AtomicBool>,
    immobility_ms: i64, no_jump_or_cloak_ms: i64) -> crate::space::system::super_weapons::FiringSpec {
    crate::space::system::super_weapons::FiringSpec { ship, character, master, weapon_type: 1, effect: 1,
        generation: generation.clone(), combat_generation: 0, sequence: 1,
        aim: crate::space::system::super_weapons::Aim::Cone { origin: [0.0; 3], direction: [1.0, 0.0, 0.0], cos_half: 1.0 },
        hit_delay_ms: 0, window_ms: 0, cycle_ms: 1.0, radius: 100.0, range: 1_000.0,
        signature_radius: 400.0, damage: [100.0; 4], immobility_ms, no_jump_or_cloak_ms, single: true, illegal: false }
}

/// Admit one activation owner, source ball and firing through the production
/// `arm_super_weapon_firing` admission path.
fn sw_arm(s: &mut crate::space::System, ship: i64, character: i64, master: i64, generation: &Arc<AtomicBool>,
    immobility_ms: i64, no_jump_or_cloak_ms: i64) -> bool {
    {
        let mut st = crate::services::combat::state();
        st.active.insert(master, ActiveModule { char_id: character, stop: generation.clone(), forced_inactive: false });
    }
    s.insert(sw_ball(ship, Some(character), 0.0));
    s.arm_super_weapon_firing(sw_spec(ship, character, master, generation, immobility_ms, no_jump_or_cloak_ms))
}

#[test]
fn lockout_is_armed_once_with_exact_fitted_deadlines() {
    crate::tests::init_config();
    let mut s = crate::space::System::new(1, Arc::default(), 1_000);
    let generation = Arc::new(AtomicBool::new(false));
    assert!(sw_arm(&mut s, 7, 42, 9_000_011, &generation, 30_000, 300_000));
    // `until > now_ms()` is the exact comparison the movement/warp/jump/cloak
    // seams apply; the deadline itself is the exclusive boundary.
    assert_eq!((s.super_weapon_immobile_until(7), s.super_weapon_jump_cloak_until(7)),
        (Some(31_000), Some(301_000)));
    assert!(s.super_weapon_immobile_until(7).is_some_and(|u| u > s.now_ms()));
    assert!(s.super_weapon_jump_cloak_until(7).is_some_and(|u| u > s.now_ms()));
    // A second ball nearby is never a lockout source: firing ship only.
    s.insert(sw_ball(8, Some(43), 50.0));
    assert_eq!((s.super_weapon_immobile_until(8), s.super_weapon_jump_cloak_until(8)), (None, None));
    s.tick(31_000);
    assert_eq!(s.super_weapon_immobile_until(7), Some(31_000));
    assert!(!s.super_weapon_immobile_until(7).is_some_and(|u| u > s.now_ms()));
    assert!(s.super_weapon_jump_cloak_until(7).is_some_and(|u| u > s.now_ms()));
    crate::services::combat::state().active.remove(&9_000_011);
}

#[test]
fn lockout_survives_module_stop_and_dies_on_deadline() {
    crate::tests::init_config();
    let mut s = crate::space::System::new(1, Arc::default(), 1_000);
    let generation = Arc::new(AtomicBool::new(false));
    assert!(sw_arm(&mut s, 7, 42, 9_000_012, &generation, 30_000, 300_000));
    generation.store(true, Ordering::Relaxed);
    s.end_super_weapon_firing(9_000_012, &generation);
    // Stop/offline cancels future pulses but never clears an armed deadline.
    assert_eq!((s.super_weapon_immobile_until(7), s.super_weapon_jump_cloak_until(7)),
        (Some(31_000), Some(301_000)));
    s.tick(300_999);
    assert_eq!(s.super_weapon_jump_cloak_until(7), Some(301_000));
    assert!(s.super_weapon_jump_cloak_until(7).is_some_and(|u| u > s.now_ms()));
    s.tick(301_000);
    assert_eq!((s.super_weapon_immobile_until(7), s.super_weapon_jump_cloak_until(7)), (None, None));
    crate::services::combat::state().active.remove(&9_000_012);
}

#[test]
fn expiry_timer_removes_only_the_same_lockout() {
    crate::tests::init_config();
    let mut s = crate::space::System::new(1, Arc::default(), 1_000);
    let first = Arc::new(AtomicBool::new(false));
    assert!(sw_arm(&mut s, 7, 42, 9_000_013, &first, 30_000, 300_000));
    first.store(true, Ordering::Relaxed);
    // A new accepted firing replaces the lockout and arms its own expiry;
    // the older timer must never remove it.
    let second = Arc::new(AtomicBool::new(false));
    assert!(sw_arm(&mut s, 7, 42, 9_000_014, &second, 10_000, 500_000));
    assert_eq!(s.super_weapon_jump_cloak_until(7), Some(501_000));
    s.tick(301_000);
    assert_eq!(s.super_weapon_jump_cloak_until(7), Some(501_000));
    s.tick(501_000);
    assert_eq!(s.super_weapon_jump_cloak_until(7), None);
    crate::services::combat::state().active.remove(&9_000_013);
    crate::services::combat::state().active.remove(&9_000_014);
}

#[test]
fn ball_leaving_removes_lockout_and_missing_or_stale_source_cannot_arm() {
    crate::tests::init_config();
    let mut s = crate::space::System::new(1, Arc::default(), 1_000);
    let generation = Arc::new(AtomicBool::new(false));
    assert!(sw_arm(&mut s, 7, 42, 9_000_015, &generation, 30_000, 300_000));
    assert!(s.remove_ball(7).is_some());
    assert_eq!((s.super_weapon_immobile_until(7), s.super_weapon_jump_cloak_until(7)), (None, None));
    let stale = Arc::new(AtomicBool::new(false));
    {
        let mut st = crate::services::combat::state();
        st.active.insert(9_000_016, ActiveModule { char_id: 42, stop: stale.clone(), forced_inactive: false });
    }
    // No source ball: admission refuses and installs nothing.
    assert!(!s.arm_super_weapon_firing(sw_spec(7, 42, 9_000_016, &stale, 30_000, 300_000)));
    assert_eq!(s.super_weapon_immobile_until(7), None);
    // A stopped activation generation refuses the same way.
    s.insert(sw_ball(7, Some(42), 0.0));
    stale.store(true, Ordering::Relaxed);
    assert!(!s.arm_super_weapon_firing(sw_spec(7, 42, 9_000_016, &stale, 30_000, 300_000)));
    crate::services::combat::state().active.remove(&9_000_015);
    crate::services::combat::state().active.remove(&9_000_016);
}
