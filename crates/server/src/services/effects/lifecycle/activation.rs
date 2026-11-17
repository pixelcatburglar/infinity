//! The shared activation constructor and deactivation path.
//! Moved verbatim from `lifecycle.rs`; `super::super` paths reach sibling
//! modules inside `crate::services::effects` one level up.
use super::{ActivationOwner, Cycle, Module, NativeCombatGeneration, cycle_start, stop_generation, subject,
    MIN_MODULE_DURATION_MS, UE_TYPEID};
use super::super::behaviors::{Activation, on_activate};
use super::super::notify::ship_effect;
use super::super::online::OnlinePolicy;
use super::super::registry::EffectAttribute;
use super::super::{EffectKind, registry, unsupported};
use crate::client::filetime_now;
use crate::dao::items::loading::ItemQuery;
use crate::db::Db;
use crate::db::outbox::Notification;
use crate::sde::Sde;
use crate::services::combat::{state, ActiveModule, Ctx};
use crate::services::combat::targeting_charges::{bank, charge_key};
use crate::services::fit::{Fit, Key};
use crate::services::identity::Identity;
use crate::services::items::load_items;
use crate::services::{db_err, notify_error, user_error, CallError, CallResult};
use crate::space::{NotifyTx, World};
use infinity_core::consts::{dogma::ATTR_DISALLOW_REPEATING_ACTIVATION, dogma::EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER, dogma::EFFECT_ONLINE};
use infinity_core::messages;
use marshal::Value;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tracing::info;

/// Existing server module duration fallback, in milliseconds.
const DEFAULT_MODULE_DURATION_MS: f64 = 1000.0;

/// Activate an effect through the shared lifecycle.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn activate(
    identity: &Identity, db: &Db, world: &Arc<World>, sde: &Arc<Sde>, space_system: Option<i64>,
    sender: NotifyTx, login_user: i64, item_id: i64, effect_name: &str, target: Option<i64>, repeat: i64,
    notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> CallResult {
    activate_inner(identity, db, world, sde, space_system, sender, login_user, item_id, effect_name, target, repeat, None, notify).await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn activate_inner(
    identity: &Identity, db: &Db, world: &Arc<World>, sde: &Arc<Sde>, space_system: Option<i64>,
    sender: NotifyTx, login_user: i64, mut item_id: i64, effect_name: &str, target: Option<i64>, repeat: i64,
    aim: Option<super::super::parameters::SuperWeaponAim>,
    notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> CallResult {
    let Some(cx) = Ctx::from_identity(identity, db, world, space_system, sender.clone()) else { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)) };
    let module = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let mut matches = cx.world.sde().dogma.effects_by_name(effect_name);
    let Some(row) = matches.next() else { return Err(unsupported::refusal(None, effect_name, module.type_id)) };
    if matches.next().is_some() { return Err(CallError::Internal(anyhow::anyhow!("multiple effects named {effect_name}"))); }
    drop(matches);
    let id = row.effect_id;
    // Fitting mutations and non-online activation share this lock through registration and FX queuing.
    // Online activation takes it inside set_online instead.
    let _lock = if id != EFFECT_ONLINE as i32 { Some(crate::services::market::lock().await) } else { None };
    let policy = OnlinePolicy::from_identity(identity, sender.clone())?;
    let mut module = subject(db, sde, &policy, cx.ship_id, item_id, id, id != EFFECT_ONLINE as i32).await?;
    let Some(effect) = registry::resolve(cx.world.sde(), id) else {
        return Err(unsupported::refusal(Some(id), effect_name, module.type_id));
    };
    let kind = effect.kind;
    let target = if kind == EffectKind::BurstJammer { None } else { target };
    let consumer = super::super::active_cycle::consumer(i64::from(id));
    if id == EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER || !registry::activation_in_scope(module.category_id, module.group_id, kind) || !effect.manually_activatable() {
        return Err(unsupported::refusal(Some(id), effect_name, module.type_id));
    }
    // Bank selection is serialized with fitting mutations by the shared lock above.
    if matches!(kind, EffectKind::Turret | EffectKind::Missile) {
        let master = crate::services::combat::targeting_charges::master_for_member(db, cx.ship_id, item_id).await?;
        if master != item_id {
            module = subject(db, sde, &policy, cx.ship_id, master, id, true).await?;
            if !registry::activation_in_scope(module.category_id, module.group_id, kind) {
                return Err(unsupported::refusal(Some(id), effect_name, module.type_id));
            }
            item_id = master;
        }
    }
    match kind {
        EffectKind::Hacking => {
            super::super::cloak::before_activate(&cx, item_id, kind).await?;
            return crate::services::hacking::start(identity, db, world, sde, space_system, sender, login_user, item_id, target, notify).await;
        }
        EffectKind::Online => {
            super::super::cloak::before_activate(&cx, item_id, kind).await?;
            return super::super::online::set_online(identity, db, world, sde, sender, login_user, cx.ship_id, item_id, true, notify).await;
        }
        EffectKind::Modifiers | EffectKind::Turret | EffectKind::Missile |
        EffectKind::LocalRepair { .. } | EffectKind::FueledLocalRepair { .. } |
        EffectKind::Propulsion | EffectKind::Mining | EffectKind::Projected { .. } | EffectKind::CapTransfer |
        EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster | EffectKind::RemoteRepair { .. } |
        EffectKind::Ecm | EffectKind::BurstJammer | EffectKind::SuperWeapon |
        EffectKind::Smartbomb | EffectKind::Vorton | EffectKind::Cloak |
        EffectKind::MicroJumpDrive | EffectKind::Salvage | EffectKind::Tractor |
        EffectKind::CargoScan | EffectKind::ShipScan | EffectKind::SurveyScan |
        EffectKind::AutoTarget | EffectKind::PassiveTarget | EffectKind::Bomb | EffectKind::Compression | EffectKind::Entosis => {}
        EffectKind::ProbeLaunch => return Err(unsupported::refusal(Some(id), effect_name, module.type_id)),
    }
    super::super::cloak::before_activate(&cx, item_id, kind).await?;
    if state().reloading.get(&item_id).is_some_and(|until| *until > filetime_now()) {
        return Err(user_error(messages::MSG_CANT_ACTIVATE_RELOADING_MODULE, vec![("moduleTypeID".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(module.type_id as i64)]))]));
    }
    let fit = Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, cx.ship_id).await?;
    let activation = Activation { effect: effect.clone(), module: Module { id: item_id, type_id: module.type_id, flag: module.flag_id }, target, aim };
    if state().active.contains_key(&item_id) {
        return Ok(Value::None);
    }
    let duration_ms = effect.attribute(EffectAttribute::Duration).map(|a| fit.value(Key::Module(item_id), a)).unwrap_or(DEFAULT_MODULE_DURATION_MS).max(MIN_MODULE_DURATION_MS);
    // The group fires together.
    let mut modules = vec![Module { id: item_id, type_id: module.type_id, flag: module.flag_id }];
    if matches!(kind, EffectKind::Turret | EffectKind::Missile) {
        for slave in bank(&cx.db, cx.ship_id, item_id).await?.into_iter().skip(1) {
            let m = subject(db, sde, &policy, cx.ship_id, slave, id, true).await?;
            modules.push(Module { id: m.item_id, type_id: m.type_id, flag: m.flag_id });
        }
    }
    for module in &modules { super::super::heat::damage::validate(&fit, module.id)?; }
    let masters: Vec<i64> = state().active.iter().filter(|(_, active)| active.char_id == cx.char_id && !active.forced_inactive).map(|(&id, _)| id).collect();
    let mut active = Vec::new();
    for master in masters { active.extend(bank(&cx.db, cx.ship_id, master).await?); }
    active.sort_unstable();
    active.dedup();
    let activating: Vec<i64> = modules.iter().map(|module| module.id).collect();
    super::super::fitting::validate_active(&cx.db, sde.clone(), &fit, cx.ship_id, &active, &activating).await?;
    on_activate(&cx, &fit, &activation).await?;
    // Runtime cooldown: the check here is admission-only; the recheck inside
    // the registration critical section below is authoritative.
    if consumer { super::super::active_cycle::admission(&mut state(), item_id, module.type_id)?; }
    // disallowRepeatingActivation (attribute 1014) is read from the effective
    // module fit: the three consumers never honour a client repeat request.
    let repeat = if consumer && fit.value(Key::Module(item_id), ATTR_DISALLOW_REPEATING_ACTIVATION) > 0.0 { 0 } else { repeat };
    let mut illegal = false;
    if matches!(kind, EffectKind::Turret | EffectKind::Missile | EffectKind::Neutralizer | EffectKind::Nosferatu) {
        let t = target.unwrap_or(0);
        let victim = cx.world.with(cx.system_id, move |s| s.ball(t).and_then(|b| b.char_id)).await.flatten();
        if let Some(victim) = victim {
            illegal = crate::services::crimewatch::aggress(&cx.db, &cx.world, cx.system_id, cx.char_id, victim).await?;
        }
    }
    let (ship, system, module_item) = (cx.ship_id, cx.system_id, item_id);
    let other = cx.world.with(system, move |s| s.loaded_effective(ship, module_item)).await.flatten()
        .map(|(_, type_id, _, _)| charge_key(ship, module.flag_id, type_id));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        if st.active.contains_key(&item_id) { return Ok(Value::None); }
        if st.reloading.get(&item_id).is_some_and(|until| *until > filetime_now()) {
            return Err(user_error(messages::MSG_CANT_ACTIVATE_RELOADING_MODULE, vec![("moduleTypeID".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(module.type_id as i64)]))]));
        }
        if consumer { super::super::active_cycle::admission(&mut st, item_id, module.type_id)?; }
        st.active.insert(item_id, ActiveModule { char_id: cx.char_id, stop: stop.clone(), forced_inactive: false });
        st.activation_generation.insert(item_id, stop.clone());
        if consumer { super::super::active_cycle::register(&mut st, item_id, cx.ship_id, &stop); }
    }
    let ship = cx.ship_id;
    cx.world.post(cx.system_id, move |s| {
        s.drop_jump_cloak(ship);
        if matches!(kind, EffectKind::Cloak | EffectKind::MicroJumpDrive) {
            s.refresh_projection_target(ship);
        }
    });
    let combat_generations = modules.iter().map(|module| NativeCombatGeneration {
        module_id: module.id, generation: crate::space::targeting::next_generation(), sequence: 0,
    }).collect();
    let owner = ActivationOwner { master: item_id, generation: stop.clone(), armed: true,
        combat_generations, pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id };
    if kind != EffectKind::Entosis && !consumer {
        ship_effect(&cx, item_id, effect.id, target, other.clone(), true, filetime_now(), duration_ms, repeat);
    }
    info!(user = login_user, "module {item_id} activated {effect_name} on {target:?} (x{}, {duration_ms:.0} ms, repeat {repeat})", modules.len());
    let world = cx.world.clone();
    let mut inputs = super::super::parameters::CycleInputs::default();
    inputs.aim = activation.aim;
    let cycle = Cycle { cx, modules, effect, target, repeat, stop, owner, started: filetime_now(), charge_type: None, cycles: 0, duration_ms, nominal_start: 0, other, repair_amount: 0.0, illegal, source_membership: None, target_type: None, inputs };
    world.post(cycle.cx.system_id, move |s| {
        let now = s.now_ms();
        cycle_start(s, cycle, now);
    });
    Ok(Value::None)
}


/// Stop after the current cycle. `login_user` is read only by the online/offline info log.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn deactivate(
    identity: &Identity, db: &Db, world: &Arc<World>, sde: &Arc<Sde>, space_system: Option<i64>,
    sender: NotifyTx, login_user: i64, item_id: i64, effect_name: &str,
    notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> CallResult {
    let Some(cx) = Ctx::from_identity(identity, db, world, space_system, sender.clone()) else { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)) };
    let module = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let name = effect_name;
    let mut rows = sde.dogma.effects_by_name(name);
    let Some(row) = rows.next() else { return Err(unsupported::refusal(None, name, module.type_id)) };
    if rows.next().is_some() { return Err(CallError::Internal(anyhow::anyhow!("multiple effects named {name}"))); }
    let id = row.effect_id;
    drop(rows);
    let policy = OnlinePolicy::from_identity(identity, sender.clone())?;
    let module = subject(db, sde, &policy, cx.ship_id, item_id, id, false).await?;
    let Some(kind) = registry::kind(sde, id) else { return Err(unsupported::refusal(Some(id), name, module.type_id)) };
    if !sde.types.effects(module.type_id).iter().any(|effect| effect.effect_id == id)
        || !registry::activation_in_scope(module.category_id, module.group_id, kind) {
        return Err(unsupported::refusal(Some(id), name, module.type_id));
    }
    if kind == EffectKind::Online { return super::super::online::set_online(identity, db, world, sde, sender, login_user, cx.ship_id, item_id, false, notify).await; }
    // A live superweapon cannot be stopped early: the accepted firing's full
    // SDE duration owns the cooldown. Source death and forced offline cancel
    // the pulse generation instead and never reach this refusal.
    if kind == EffectKind::SuperWeapon
        && state().active.get(&item_id).is_some_and(|active| !active.stop.load(Ordering::Relaxed)) {
        return Err(notify_error(messages::super_weapon::UNAVAILABLE));
    }
    if let Some(stop) = state().active.get(&item_id) {
        stop_generation(&stop.stop);
    }
    if kind == EffectKind::Cloak {
        let generation = state().active.get(&item_id).map(|active| active.stop.clone());
        let ship = cx.ship_id;
        cx.world.post(cx.system_id, move |s| {
            if let Some(generation) = generation { s.stop_module_cloak(ship, item_id, &generation); }
        });
    }
    if matches!(kind, EffectKind::Projected { .. }) {
        let generation = state().active.get(&item_id).map(|active| active.stop.clone());
        cx.world.post(cx.system_id, move |s| {
            if generation.as_ref().is_some_and(|generation| state().active.get(&item_id).is_some_and(|active| Arc::ptr_eq(&active.stop, generation))) {
                s.remove_projection(item_id);
            }
        });
    }
    Ok(Value::None)
}
