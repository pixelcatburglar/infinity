//! Command bursts: fit-side runtime modifier sources and the producer's
//! classification, wire payloads and pulse through a real system actor.
use super::fixture;
use crate::services::combat::{self, ActiveModule, Ctx, groups_health::{Condition, ShipStats}};
use crate::services::effects::{behaviors::{self, Activation}, command_burst::{self, CommandBurstFamily}, command_burst_fx::command_burst_guids,
    lifecycle::{ActivationOwner, Cycle, CycleOutcome, Module}, notify, parameters, registry, weapons, EffectKind};
use crate::services::fit::{runtime::{RuntimeModifierSource, RuntimeProducerNamespace, RuntimeReceiverKey, RuntimeSourceKey}, Fit, Key};
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
use crate::space::combat::inputs::ChargeToken;
use crate::sde::Sde;
use marshal::Value;
use std::sync::{atomic::AtomicBool, Arc};
use infinity_core::consts::{dogma::*, CATEGORY_DRONE, CATEGORY_MODULE, CATEGORY_SHIP, FLAG_DRONE_BAY, FLAG_HI_SLOT_FIRST, GROUP_COMMAND_BURST_CHARGE_ARMOR};

fn key(id: i64) -> RuntimeSourceKey { RuntimeSourceKey { producer: RuntimeProducerNamespace::new(9), source_id: id, incarnation: 1 } }
fn src(receiver: RuntimeReceiverKey, collection_id: i32, key: RuntimeSourceKey, value: f64) -> RuntimeModifierSource {
    RuntimeModifierSource { receiver, collection_id, source: key, value }
}
async fn sde() -> (crate::db::Db, Arc<Sde>) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    (db, sde)
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn runtime_sources_coexist_per_receiver_collection_and_source() {
    let (_db, sde) = sde().await;
    let mut fit = Fit::for_space_type(sde, 1, Vec::new());
    let (a, b) = (key(1), key(2));
    for s in [src(RuntimeReceiverKey::Ship(1), 5, a, 1.0), src(RuntimeReceiverKey::Ship(1), 5, b, 2.0), src(RuntimeReceiverKey::Character(7), 6, a, 3.0)] {
        assert!(fit.replace_runtime_modifier_source(s));
    }
    assert!(!fit.replace_runtime_modifier_source(src(RuntimeReceiverKey::Ship(1), 5, a, 1.0)));
    assert!(fit.remove_runtime_modifier_source(RuntimeReceiverKey::Ship(1), 5, a));
    assert_eq!(fit.runtime_modifier_sources(), &[src(RuntimeReceiverKey::Ship(1), 5, b, 2.0), src(RuntimeReceiverKey::Character(7), 6, a, 3.0)]);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn collection_fold_applies_sde_aggregate_through_the_evaluator() {
    let (db, sde) = sde().await;
    let (maximum, minimum) = (sde.dbuffs.get(2468).unwrap(), sde.dbuffs.get(2466).unwrap());
    let (group, skill, attr_a, attr_b) = (maximum.modifiers.iter().find_map(|m| m.modifier.group).unwrap(),
        minimum.modifiers.iter().find_map(|m| m.modifier.skill).unwrap(),
        maximum.modifiers[0].modifier.modified, minimum.modifiers[0].modifier.modified);
    let ty = |pred: &dyn Fn(&crate::sde::types::TypeData) -> bool| sde.types.iter().find(|t| pred(t)).unwrap().type_id;
    let (hull, m_ty, s_ty) = (ty(&|t| t.category_id == Some(CATEGORY_SHIP)),
        ty(&|t| t.category_id == Some(CATEGORY_MODULE) && t.group_id == Some(group)),
        ty(&|t| t.category_id == Some(CATEGORY_MODULE) && ATTR_REQUIRED_SKILLS.iter().any(|a| t.attribute(*a) == Some(f64::from(skill)))));
    let f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[
        ShipItemSpec { type_id: m_ty, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] },
        ShipItemSpec { type_id: s_ty, flag_id: FLAG_HI_SLOT_FIRST + 1, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let (m, s2) = (f.items[0], f.items[1]);
    crate::dao::items::dogma::set_override(&db, s2, attr_b, 100.0).await.unwrap();
    let mut fit = Fit::load(&db, sde, f.character, f.ship).await.unwrap();
    let (base_a, base_b, mass) = (fit.value(Key::Module(m), attr_a), fit.value(Key::Module(s2), attr_b), fit.value(Key::Ship, ATTR_MASS));
    // Collection 2468 modifies group 538 attribute 1909, which the selected
    // module type already authors, so the candidate exists before any source.
    assert!(fit.fitting_candidates(&fit).contains(&(Key::Module(m), attr_a)));
    let before = fit.fitting_values(&fit.fitting_candidates(&fit));
    for (k, v) in [(key(1), 10.0), (key(2), 5.0)] { fit.replace_runtime_modifier_source(src(RuntimeReceiverKey::Ship(f.ship), 2468, k, v)); }
    for (k, v) in [(key(1), 8.0), (key(2), 4.0)] { fit.replace_runtime_modifier_source(src(RuntimeReceiverKey::Ship(f.ship), 2466, k, v)); }
    assert_eq!(fit.value(Key::Module(m), attr_a), base_a + 10.0);
    assert_eq!(fit.value(Key::Module(s2), attr_b), base_b * (1.0 + 4.0 / crate::services::fit::PERCENT_SCALE));
    assert_eq!(fit.value(Key::Ship, ATTR_MASS), mass);
    assert!(fit.fitting_candidates(&fit).contains(&(Key::Module(m), attr_a)));
    assert!(fit.fitting_changes(&before).iter().any(|&(k, a, old, new)| k == Key::Module(m) && a == attr_a && new == old + 10.0));
    assert!(fit.remove_runtime_modifier_source(RuntimeReceiverKey::Ship(f.ship), 2468, key(1)));
    assert_eq!(fit.value(Key::Module(m), attr_a), base_a + 5.0);
    assert!(fit.remove_runtime_modifier_source(RuntimeReceiverKey::Ship(f.ship), 2468, key(2)));
    assert_eq!(fit.value(Key::Module(m), attr_a), base_a);
    cleanup_ship(&db, None, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn character_refresh_keeps_runtime_sources_and_owner_package_carries_them() {
    let (db, sde) = sde().await;
    let (hull, drone_ty) = (sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id,
        sde.types.iter().find(|t| t.category_id == Some(CATEGORY_DRONE)).unwrap().type_id);
    let f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[]).await.unwrap();
    let mut fit = Fit::load(&db, sde.clone(), f.character, f.ship).await.unwrap();
    for s in [src(RuntimeReceiverKey::Ship(f.ship), 1, key(1), 1.0), src(RuntimeReceiverKey::Module { ship: f.ship, module: 2 }, 2, key(1), 2.0),
        src(RuntimeReceiverKey::Character(f.character), 3, key(1), 3.0), src(RuntimeReceiverKey::Drone { ship: f.ship, drone: 8 }, 4, key(1), 4.0)] {
        fit.replace_runtime_modifier_source(s);
    }
    let inputs = fit.character_inputs();
    fit.apply_character_inputs(&inputs);
    assert!(!fit.expire_character_inputs(i64::MAX / 2));
    assert_eq!(fit.runtime_modifier_sources().len(), 4);
    let package = fit.owner_inputs().unwrap();
    let mut drone_fit = Fit::for_space_type(sde, hull, Vec::new());
    drone_fit.include_drone(7, drone_ty, Vec::new());
    drone_fit.replace_runtime_modifier_source(src(RuntimeReceiverKey::Drone { ship: 9, drone: 7 }, 5, key(2), 5.0));
    drone_fit.apply_owner_inputs(&package);
    let receivers: Vec<_> = drone_fit.runtime_modifier_sources().iter().map(|s| s.receiver).collect();
    assert!(receivers.len() == 4 && receivers.contains(&RuntimeReceiverKey::Drone { ship: 9, drone: 7 }));
    assert!(!receivers.contains(&RuntimeReceiverKey::Drone { ship: f.ship, drone: 8 }));
    drone_fit.strip_to_own_drone(7);
    assert_eq!(drone_fit.runtime_modifier_sources(), &[src(RuntimeReceiverKey::Drone { ship: 9, drone: 7 }, 5, key(2), 5.0)]);
    cleanup_ship(&db, None, f).await.unwrap();
}

#[test]
fn command_burst_guids_match_client_effect_names() {
    for (family, guids) in [
        (CommandBurstFamily::Armor, ("effects.WarfareLinkSphereArmor", "effects.WarfareLinkArmor")),
        (CommandBurstFamily::Information, ("effects.WarfareLinkSphereInformation", "effects.WarfareLinkInformation")),
        (CommandBurstFamily::Mining, ("effects.WarfareLinkSphereMining", "effects.WarfareLinkMining")),
        (CommandBurstFamily::Shield, ("effects.WarfareLinkSphereShield", "effects.WarfareLinkShield")),
        (CommandBurstFamily::Skirmish, ("effects.WarfareLinkSphereSkirmish", "effects.WarfareLinkSkirmish")),
    ] { assert_eq!(command_burst_guids(family), guids); }
}

#[test]
fn dbuff_payloads_encode_state_and_clear() {
    let ms = 5_000;
    assert_eq!(notify::command_burst_dbuff_updated(9, &[(5, 2.5, Some(ms)), (7, -1.0, None)]), vec![Value::Int(9), Value::list([
        Value::tuple([Value::Int(5), Value::tuple([Value::Float(2.5), Value::Int(crate::client::filetime_from_unix_ms(ms))])]),
        Value::tuple([Value::Int(7), Value::tuple([Value::Float(-1.0), Value::None])])])]);
    assert_eq!(notify::command_burst_dbuff_updated(9, &[]), vec![Value::Int(9), Value::list(Vec::new())]);
    assert_eq!(notify::command_burst_fleet_boosted(3, 4), vec![Value::Int(3), Value::Int(4)]);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn command_burst_effects_classify_by_family() {
    let (_db, sde) = sde().await;
    for (effect, family) in [
        (EFFECT_COMMAND_BURST_ARMOR, CommandBurstFamily::Armor), (EFFECT_COMMAND_BURST_SHIELD, CommandBurstFamily::Shield),
        (EFFECT_COMMAND_BURST_SKIRMISH, CommandBurstFamily::Skirmish), (EFFECT_COMMAND_BURST_INFO, CommandBurstFamily::Information),
        (EFFECT_COMMAND_BURST_MINING, CommandBurstFamily::Mining)] {
        assert_eq!(registry::kind(&sde, effect), Some(EffectKind::CommandBurst { family }));
    }
    let charge_bonus = sde.dogma.effects_by_name("chargeBonusWarfareCharge").next().expect("charge bonus effect").effect_id as i32;
    assert_eq!(registry::kind(&sde, charge_bonus), Some(EffectKind::Modifiers));
}

/// Shared burst fixture: a live ship with one fitted armor command burst and
/// an optional stack of armor charges. The resident empty fit is removed and
/// the same ball is admitted again so the actor cold-loads the fitted charges.
async fn burst_fixture(charges: i64) -> (crate::services::drones::tests::fixture::Fixture, i64, i32) {
    let f = crate::services::drones::tests::fixture::Fixture::new().await;
    let sde = f.sde.clone();
    let module_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_COMMAND_BURST_ARMOR)).expect("armor burst module").type_id;
    let module = fixture::item(&f.db, module_type, f.owner, f.ship, FLAG_HI_SLOT_FIRST, 1, true, true).await.unwrap();
    if charges > 0 {
        let charge_type = sde.types.iter().find(|t| t.group_id == Some(GROUP_COMMAND_BURST_CHARGE_ARMOR)).expect("armor burst charge").type_id;
        f.db.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1,$2,$3,$4)",
            &[&f.ship, &FLAG_HI_SLOT_FIRST, &charge_type, &charges]).await.unwrap();
    }
    let ball = f.world.remove_ball(f.system, f.ship).await.expect("resident burst ship");
    fixture::admit(&f.db, &f.world, f.system, ball).await;
    (f, module, module_type)
}

fn cycle(cx: &Ctx, module: i64, module_type: i32, effect: registry::Effect) -> Cycle {
    let generation = Arc::new(AtomicBool::new(false));
    let mut st = combat::state();
    st.active.insert(module, ActiveModule { char_id: cx.char_id, stop: generation.clone(), forced_inactive: false });
    st.activation_generation.insert(module, generation.clone());
    drop(st);
    Cycle { cx: cx.clone(), modules: vec![Module { id: module, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }], presented_effect: effect.id, effect, target: None, repeat: 1,
        stop: generation.clone(), owner: ActivationOwner { master: module, generation, armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
        started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
        source_membership: None, target_type: None, inputs: parameters::CycleInputs::default() }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn pulse_consumes_one_charge_and_lands_on_source_and_in_range_member() {
    let (mut f, module, module_type) = burst_fixture(2).await;
    let (member, member_ship) = f.add_pilot().await;
    let fleet = crate::services::drones::tests::resources::fleet::form_fleet(&mut f, member, member_ship).await.unwrap();
    let (member_tx, mut member_rx) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(member, member_tx);
    let drone_ty = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_DRONE)).unwrap().type_id;
    let drone_item = fixture::item(&f.db, drone_ty, member, member_ship, FLAG_DRONE_BAY, 1, true, false).await.unwrap();
    let drone_fit = Fit::load_drone(&f.db, f.sde.clone(), member, member_ship, drone_item, Vec::new(), Vec::new()).await.unwrap();
    f.world.with(f.system, move |s| {
        s.insert(fixture::ball(drone_item, drone_ty, member, None, 0.0));
        s.register_drone(drone_item, crate::space::drone::DroneSpec { type_id: drone_ty, bandwidth: 5.0, scan_resolution: 100.0,
            control_range: 1000.0, max_target_range: 1000.0, optimal_range: 100.0, orbit_range: 50.0, duration_ms: 1000.0, effect_id: Some(1) },
            Arc::new(|_, _| None), Some(member_ship), Some(member), false);
        let stats = ShipStats::from_fit(&drone_fit, Key::Drone(drone_item));
        s.install_condition_with_key(drone_item, Condition { now: stats.max, stats, shield_at: 0 }, drone_fit, Key::Drone(drone_item));
    }).await;
    let (far, far_ship) = f.add_pilot().await;
    let far_ty = f.world.ball(f.system, far_ship).await.unwrap().type_id;
    f.world.remove_ball(f.system, far_ship).await;
    fixture::admit(&f.db, &f.world, f.system, fixture::ball(far_ship, far_ty, far, Some(far), 1e9)).await;
    let (ftx, _frx) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(far, ftx);
    f.world.set_online_session(far, Some(infinity_core::session::OnlineSession { character_id: far, user_id: None, session_id: far,
        ship_id: Some(far_ship), station_id: None, system_id: Some(f.system), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
    let params = Value::Int(fleet);
    let kwargs = Value::None;
    let invite = crate::services::Call { service: "fleetObjectHandler", method: "Invite", args: &[Value::Int(far), Value::None, Value::None, Value::None], kwargs: &kwargs, bound: true, bind_params: Some(&params) };
    crate::services::fleets::dispatch(&mut f.client, &invite).await.unwrap().unwrap();
    f.client.session.apply(vec![("charid", Value::Int(far)), ("shipid", Value::Int(far_ship)), ("fleetid", Value::None), ("corpid", Value::Int(1)),
        ("allianceid", Value::None), ("warfactionid", Value::None), ("solarsystemid2", Value::Int(f.system)), ("role", Value::Int(0)), ("corprole", Value::Int(0))]);
    let accept = crate::services::Call { service: "fleetObjectHandler", method: "AcceptInvite", args: &[Value::Int(i64::from(far_ty))], kwargs: &kwargs, bound: true, bind_params: Some(&params) };
    crate::services::fleets::dispatch(&mut f.client, &accept).await.unwrap().unwrap();
    f.world.unregister_pilot_generation(far, None);
    assert!(crate::services::fleets::same_fleet(f.owner, far));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (dtx, mut drx) = tokio::sync::mpsc::unbounded_channel();
    f.world.observe(f.system, f.owner, f.ship, dtx).await.unwrap();
    crate::space::system::tests::events::initial_state(&mut drx);
    crate::space::system::tests::events::fleet_snapshot(&mut drx);
    let cx = Ctx { db: f.db.clone(), world: f.world.clone(), notify: tx, char_id: f.owner, ship_id: f.ship, system_id: f.system };
    let effect = registry::resolve(&f.sde, EFFECT_COMMAND_BURST_ARMOR).unwrap();
    let mut cycle = fixture::capture_cycle(cycle(&cx, module, module_type, effect)).await;
    let source_fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.unwrap();
    let collections: Vec<i32> = ATTR_WARFARE_BUFF_IDS.iter().map(|a| source_fit.value(Key::Module(module), *a) as i32).filter(|id| *id > 0).collect();
    let drone_attr = collections.iter().filter_map(|id| f.sde.dbuffs.get(*id)).flat_map(|c| c.modifiers.iter())
        .find(|m| m.modifier.func == "ItemModifier").map(|m| m.modifier.modified).expect("item modifier");
    let drone_before = f.world.with(f.system, move |s| s.with_combat_fit(drone_item, |fit, _, _| fit.value(Key::Ship, drone_attr))).await.flatten();
    assert_eq!(command_burst::start(&cx, &mut cycle).await.unwrap(), CycleOutcome::Continue);
    let (system, src_ship, m_ship, fa_ship, d_item) = (f.system, f.ship, member_ship, far_ship, drone_item);
    let (loaded, src_state, m_state, far_state, drone_result) = f.world.with(system, move |s| (
        s.loaded_effective(src_ship, module).map(|slot| slot.2),
        s.runtime_collection_state(RuntimeReceiverKey::Ship(src_ship)),
        s.runtime_collection_state(RuntimeReceiverKey::Ship(m_ship)),
        s.runtime_collection_state(RuntimeReceiverKey::Ship(fa_ship)),
        s.with_combat_fit(d_item, |fit, _, _| (fit.value(Key::Ship, drone_attr), fit.runtime_modifier_sources().iter().any(|s| s.receiver == RuntimeReceiverKey::Ship(m_ship)))))).await.unwrap();
    assert_eq!(loaded, Some(1));
    for id in &collections { assert!(src_state.iter().any(|(c, _, _)| c == id) && m_state.iter().any(|(c, _, _)| c == id)); }
    assert!(far_state.is_empty() && drone_result.map(|(_, has)| has) == Some(true) && drone_result.map(|(v, _)| v) != drone_before);
    let expected = notify::command_burst_dbuff_updated(m_ship, &m_state);
    let mut dbuff = false;
    while let Ok((name, args)) = member_rx.try_recv() {
        if name == "OnDbuffUpdated" { assert_eq!(args, expected); dbuff = true; }
    }
    assert!(dbuff);
    let boosted = rx.try_recv().into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok()))
        .find(|(name, _)| name.as_str() == "OnFleetBoosted");
    assert_eq!(boosted.map(|(_, a)| a), Some(notify::command_burst_fleet_boosted(module_type, 2)));
    let mut seen = (false, false);
    while let Ok(delivery) = drx.try_recv() {
        for event in crate::space::system::tests::events::destiny(delivery) {
            let Value::Tuple(event) = event else { continue };
            let Some(Value::Tuple(pair)) = event.into_iter().nth(1) else { continue };
            if pair.first().and_then(Value::as_bytes) != Some(b"OnSpecialFX".as_slice()) { continue; }
            let Some(Value::Tuple(args)) = pair.into_iter().nth(1) else { continue };
            seen.0 |= args[0].as_int() == Some(src_ship) && args[5].as_bytes() == Some(b"effects.WarfareLinkSphereArmor".as_slice());
            seen.1 |= args[0].as_int() == Some(m_ship) && args[3].as_int() == Some(src_ship) && args[5].as_bytes() == Some(b"effects.WarfareLinkArmor".as_slice());
        }
    }
    assert!(seen.0 && seen.1);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn pulse_without_charge_is_refused_before_capacitor() {
    let (f, module, module_type) = burst_fixture(0).await;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let cx = Ctx { db: f.db.clone(), world: f.world.clone(), notify: tx, char_id: f.owner, ship_id: f.ship, system_id: f.system };
    let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.unwrap();
    let effect = registry::resolve(&f.sde, EFFECT_COMMAND_BURST_ARMOR).unwrap();
    let activation = Activation { effect, module: Module { id: module, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }, target: None, aim: None };
    let Err(crate::services::CallError::User { msg, .. }) = behaviors::on_activate(&cx, &fit, &activation).await else { panic!("uncharged burst admitted") };
    assert_eq!(msg, infinity_core::messages::MSG_NO_CHARGES);
    assert!(f.world.with(f.system, move |s| s.runtime_collection_state(RuntimeReceiverKey::Ship(f.ship))).await.unwrap().is_empty());
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn stale_charge_token_stops_cycle_without_debit() {
    let (f, module, _module_type) = burst_fixture(2).await;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let cx = Ctx { db: f.db.clone(), world: f.world.clone(), notify: tx, char_id: f.owner, ship_id: f.ship, system_id: f.system };
    let (incarnation, type_id, membership) = f.world.with(f.system, move |s|
        (s.loaded_effective(f.ship, module).map(|slot| (slot.0, slot.1)), s.combat_membership_generation(f.ship))).await
        .map(|(slot, membership)| (slot.unwrap().0, slot.unwrap().1, membership.unwrap())).unwrap();
    let stale = ChargeToken { ship: f.ship, membership, module, incarnation: incarnation.wrapping_add(1), type_id };
    let permit = f.world.with(f.system, move |s| s.hold_loaded_use(f.ship, module)).await.flatten();
    assert_eq!(weapons::consume_charge(&cx, permit, Some(stale)).await, None);
    let quantity = f.world.with(f.system, move |s| s.loaded_effective(f.ship, module)).await.flatten();
    assert_eq!(quantity.map(|slot| slot.2), Some(2));
    f.cleanup().await;
}
