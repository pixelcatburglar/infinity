//! In-space repair of heat-damaged modules with nanite repair paste.
//! Mirrors `godma.py` `RepairModule`/`RepairManyModules`/`StopRepairModule`,
//! `baseDogmaLocation.py` `FindNumUnitsToRepairModule` and the module button's
//! repair menu (`shipmodulebutton.py`). Paste stacks come from `game.items`
//! cargo rows, module damage lives in `game.item_attributes` attribute 3, and
//! rates and prices come from the resident Fit and SDE.
//! Repairs in progress are process-local state: a departure settles the
//! elapsed share and a restart loses the entry entirely (the paste was
//! consumed at start). Tether, structure and NPC repair are not supported.
//! Completion uses an absolute deadline from the same FILETIME start as settlement.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use infinity_core::consts::{dogma::{ATTR_HULL_DAMAGE, ATTR_STRUCTURE_HP, EFFECT_ONLINE}, CATEGORY_MODULE, FILETIME_MINUTE, FLAG_CARGO, TYPE_NANITE_REPAIR_PASTE};
use infinity_core::messages;
use marshal::Value;
use tracing::{info, warn};
use crate::client::{filetime_now, unix_ms_from_filetime_ceil, Client};
use crate::dao::{items::{dogma, loading::{Item, ItemQuery}, ship}, trades};
use crate::db::{executor::Executor, outbox::{dispatch_sender, Outbox}, Db};
use crate::sde::Sde;
use crate::services::{combat, db_err, fit::{attributes::CommittedFitting, Key}, identity::Identity, inventory, items::load_items, market, notify_error, repair, user_error, CallError, CallResult};
use crate::services::effects::{fuel, lifecycle, notify, online::{self, OnlineChanged, OnlinePolicy}};
use crate::space::{minutes_to_seconds, seconds_to_ms_f64, System, World};

/// SDE dogma attribute moduleRepairRate (`dogma/const.py` `attributeModuleRepairRate`).
const ATTR_MODULE_REPAIR_RATE: i32 = 1267;
/// SDE dogma attribute shipBrokenModuleRepairCostMultiplier (`dogma/const.py`).
const ATTR_SHIP_BROKEN_MODULE_REPAIR_COST_MULTIPLIER: i32 = 1277;

/// One repair in progress. `paid_hp` bounds what the consumed paste covers, so
/// heat taken during the repair is not repaired for free.
struct Repair { owner: i64, ship: i64, system: i64, started_at: i64, paid_hp: f64, rate_hp_per_min: f64, token: u64 }

/// moduleID -> repair in progress; survives session changes until settlement.
fn state() -> MutexGuard<'static, HashMap<i64, Repair>> {
    static STATE: OnceLock<Mutex<HashMap<i64, Repair>>> = OnceLock::new();
    crate::poison::lock_or_exit(STATE.get_or_init(Default::default).lock())
}

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Whether a repair is in progress for the module.
pub(crate) fn repairing(module: i64) -> bool { state().contains_key(&module) }

fn invalid_rate() -> CallError { CallError::Internal(anyhow::anyhow!("invalid module repair rate")) }

/// `baseDogmaLocation.py` `FindNumUnitsToRepairModule`, in whole paste units.
fn paste_units(damage: f64, hp: f64, module_base_price: f64, multiplier: f64, paste_base_price: f64) -> Result<i64, CallError> {
    if !damage.is_finite() || !hp.is_finite() || !module_base_price.is_finite()
        || !multiplier.is_finite() || !paste_base_price.is_finite() {
        return Err(CallError::Internal(anyhow::anyhow!("invalid module repair inputs")));
    }
    if module_base_price == 0.0 || damage <= 0.0 { return Ok(0); }
    if hp <= 0.0 || paste_base_price <= 0.0 {
        return Err(CallError::Internal(anyhow::anyhow!("invalid module repair inputs")));
    }
    Ok((damage * (module_base_price / hp) * multiplier / paste_base_price).ceil() as i64)
}

/// Client `godma.py` `RepairModule_thread` delay: damage / repair rate minutes.
fn duration_ms(paid_hp: f64, rate: f64) -> Result<u64, CallError> {
    if !rate.is_finite() || rate <= 0.0 { return Err(invalid_rate()); }
    Ok(seconds_to_ms_f64(minutes_to_seconds(paid_hp / rate)).ceil() as u64)
}

/// HP covered by the elapsed share of the paid repair: never negative and
/// never above what the consumed paste bought.
fn repaired_hp(paid_hp: f64, rate: f64, elapsed_filetime: i64) -> f64 {
    (rate * elapsed_filetime as f64 / FILETIME_MINUTE as f64).clamp(0.0, paid_hp)
}

/// Claim the settlement exactly once; a stale timer cannot settle a newer repair.
fn claim(module: i64, token: Option<u64>) -> Option<Repair> {
    let mut state = state();
    if !matches!(state.get(&module), Some(entry) if token.is_none_or(|token| token == entry.token)) { return None; }
    state.remove(&module)
}

/// A module or its bank master already running an activation cannot start a repair.
async fn running(c: &Client, cx: &combat::Ctx, module: i64) -> Result<bool, CallError> {
    let master = combat::targeting_charges::master_for_member(&c.db, cx.ship_id, module).await?;
    let state = combat::state();
    Ok(state.active.contains_key(&module) || state.active.contains_key(&master))
}

fn short_paste() -> CallError { user_error(messages::MSG_NOT_ENOUGH_REPAIR_MATERIAL_TO_FINISH_ALL_REPAIRS, vec![]) }
fn not_found() -> CallError { notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION) }
fn paste_base(sde: &Sde) -> f64 { sde.types.get(TYPE_NANITE_REPAIR_PASTE).and_then(|ty| ty.base_price).unwrap_or(0.0) }

/// Cargo paste stacks, their open-trade reservations and the unreserved
/// units; the client's `GetBaysToCheck` consumes from the cargo hold only.
async fn stacks(ex: &(impl Executor + ?Sized), sde: &Sde, ship: i64, owner: i64) -> Result<(Vec<Item>, Vec<i64>, i64), CallError> {
    let items = load_items(ex, sde, ItemQuery::LocationOwnerFlags(ship, owner, &[FLAG_CARGO])).await.map_err(db_err)?;
    let paste = |item: &&Item| item.type_id == TYPE_NANITE_REPAIR_PASTE && !item.singleton && item.quantity > 0;
    let ids: Vec<i64> = items.iter().filter(|item| paste(item)).map(|item| item.item_id).collect();
    let reserved = trades::reserved(ex, &ids).await.map_err(db_err)?;
    let available = items.iter().filter(|item| paste(item) && reserved.binary_search(&item.item_id).is_err()).map(|item| item.quantity).sum();
    Ok((items, reserved, available))
}

/// Plan and apply a paste spend inside the transaction with the paste refusal.
async fn spend(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox, items: Vec<Item>, reserved: &[i64], owner: i64, units: i64) -> Result<(), CallError> {
    let changes = fuel::plan(&[(TYPE_NANITE_REPAIR_PASTE, units)], &items, reserved, sde, owner).map_err(|_| short_paste())?;
    fuel::spend(ex, sde, outbox, items, &changes, short_paste).await
}

/// The module's fitted context and current damage inside the transaction.
async fn fitted_damage(ex: &(impl Executor + ?Sized), sde: &Sde, policy: &OnlinePolicy, ship: i64, module: i64) -> Result<(Item, f64), CallError> {
    let (_, item) = lifecycle::subject_core(ex, sde, policy, ship, module, EFFECT_ONLINE as i32, false).await?;
    let damage = dogma::overrides_for(ex, &[module]).await.map_err(db_err)?.iter()
        .find(|v| v.item_id == module && v.attribute_id == ATTR_HULL_DAMAGE).map(|v| v.value).unwrap_or(0.0);
    Ok((item, damage))
}

/// (rate, multiplier) plus per-module HP read once from the resident combat
/// fit; the vector follows `modules` order.
async fn fit_inputs(cx: &combat::Ctx, modules: &[i64]) -> Result<(f64, f64, Vec<f64>), CallError> {
    let ship = cx.ship_id;
    let modules = modules.to_vec();
    let (rate, multiplier, health) = cx.world.with(cx.system_id, move |s| s.with_combat_fit(ship, |fit, _, _| (
        fit.value(Key::Char, ATTR_MODULE_REPAIR_RATE),
        fit.value(Key::Char, ATTR_SHIP_BROKEN_MODULE_REPAIR_COST_MULTIPLIER),
        modules.iter().map(|&module| fit.value(Key::Module(module), ATTR_STRUCTURE_HP)).collect(),
    ))).await.flatten().ok_or_else(not_found)?;
    if !rate.is_finite() || rate <= 0.0 { return Err(invalid_rate()); }
    Ok((rate, multiplier, health))
}

/// `dogmaIM.InitiateModuleRepair`: `godma.py` `RepairModule` returns `True`
/// when the repair is running; refusals raise.
pub(in crate::services) async fn initiate(c: &mut Client, args: &[Value]) -> CallResult {
    let module = match args {
        [Value::Int(module)] if *module > 0 => *module,
        _ => return Err(not_found()),
    };
    let cx = combat::ctx(c).ok_or_else(not_found)?;
    if repairing(module) { return Ok(Value::Bool(true)); }
    if running(c, &cx, module).await? { return Err(notify_error(messages::heat::REPAIR_MODULE_ACTIVE)); }
    let ship = cx.ship_id;
    let (rate, multiplier, health) = fit_inputs(&cx, &[module]).await?;
    let hp = health.first().copied().unwrap_or(0.0);
    let owner = cx.char_id;
    let policy = OnlinePolicy::from_identity(&Identity::of_client(c), c.notify_tx.clone())?;
    let sde = c.sde.clone();
    let world = cx.world.clone();
    let sender = cx.notify.clone();
    let _lock = market::lock().await;
    let damage = c.db.transaction(|tx, outbox: &mut Outbox| {
        let (sde, policy) = (sde.clone(), policy.clone());
        Box::pin(async move {
            if !ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(not_found()); }
            let (module_item, damage) = fitted_damage(tx, &sde, &policy, ship, module).await?;
            if damage <= 0.0 { return Err(notify_error(messages::heat::REPAIR_NOT_DAMAGED)); }
            let units = paste_units(damage, hp, repair::computed_base_price(&sde, module_item.type_id), multiplier, paste_base(&sde))?;
            if units > 0 {
                let (items, reserved, _) = stacks(tx, &sde, ship, owner).await?;
                spend(tx, &sde, outbox, items, &reserved, owner, units).await?;
            }
            Ok(damage)
        })
    }, |damage, outbox| async move { dispatch_sender(damage, outbox, &world, Some(&sender)) }).await?;
    register(&cx, &c.db, owner, module, damage, rate, filetime_now())?;
    Ok(Value::Bool(true))
}

/// `dogmaIM.InitiateModuleRepairMany`: `godma.py` `RepairManyModules` returns
/// `(success, missingCharges, failed)`; per-module refusals join the lists.
pub(in crate::services) async fn initiate_many(c: &mut Client, args: &[Value]) -> CallResult {
    let mut seen = std::collections::HashSet::new();
    let mut requested = crate::services::value_id_list(args.first());
    requested.retain(|module| *module > 0 && seen.insert(*module));
    if requested.is_empty() { return Err(not_found()); }
    let cx = combat::ctx(c).ok_or_else(not_found)?;
    let ship = cx.ship_id;
    let (rate, multiplier, health) = fit_inputs(&cx, &requested).await?;
    // An entry already running settles nothing: it counts as started.
    let (mut success, mut failed, mut start) = (Vec::new(), Vec::new(), Vec::new());
    for (&module, &hp) in requested.iter().zip(&health) {
        if repairing(module) { success.push(module); }
        else if running(c, &cx, module).await? { failed.push(module); }
        else { start.push((module, hp)); }
    }
    let owner = cx.char_id;
    let (paid, missing, failed) = if start.is_empty() { (Vec::new(), Vec::new(), failed) } else {
        let policy = OnlinePolicy::from_identity(&Identity::of_client(c), c.notify_tx.clone())?;
        let sde = c.sde.clone();
        let world = cx.world.clone();
        let sender = cx.notify.clone();
        let _lock = market::lock().await;
        c.db.transaction(|tx, outbox: &mut Outbox| {
            let (sde, policy, start, mut failed) = (sde.clone(), policy.clone(), start.clone(), failed.clone());
            Box::pin(async move {
                if !ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(not_found()); }
                let (items, reserved, mut remaining) = stacks(tx, &sde, ship, owner).await?;
                let paste_base = paste_base(&sde);
                let (mut paid, mut missing, mut total) = (Vec::new(), Vec::new(), 0i64);
                for (module, hp) in &start {
                    let (module_item, damage) = match fitted_damage(tx, &sde, &policy, ship, *module).await {
                        Ok(pair) if pair.1 > 0.0 => pair,
                        Ok(_) | Err(CallError::User { .. }) => { failed.push(*module); continue; }
                        Err(error) => return Err(error),
                    };
                    let units = paste_units(damage, *hp, repair::computed_base_price(&sde, module_item.type_id), multiplier, paste_base)?;
                    if units > remaining { missing.push(*module); continue; }
                    remaining -= units;
                    total += units;
                    paid.push((*module, damage));
                }
                if total > 0 { spend(tx, &sde, outbox, items, &reserved, owner, total).await?; }
                Ok((paid, missing, failed))
            })
        }, |result, outbox| async move { dispatch_sender(result, outbox, &world, Some(&sender)) }).await?
    };
    let started_at = filetime_now();
    for (module, paid_hp) in &paid {
        register(&cx, &c.db, owner, *module, *paid_hp, rate, started_at)?;
    }
    success.extend(paid.iter().map(|(module, _)| *module));
    // The three lists preserve request order.
    let list = |ids: &[i64]| Value::list(requested.iter().filter(|module| ids.contains(module)).map(|module| Value::Int(*module)));
    Ok(Value::tuple([list(&success), list(&missing), list(&failed)]))
}

/// Insert the repair and schedule its settlement timer on the system actor.
fn register(cx: &combat::Ctx, db: &Db, owner: i64, module: i64, paid_hp: f64, rate: f64, started_at: i64) -> Result<(), CallError> {
    let delay = duration_ms(paid_hp, rate)?;
    // The actor's last tick can precede started_at; a relative timer can settle early.
    let due = unix_ms_from_filetime_ceil(started_at).saturating_add(i64::try_from(delay).unwrap_or(i64::MAX));
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    state().insert(module, Repair { owner, ship: cx.ship_id, system: cx.system_id, started_at, paid_hp, rate_hp_per_min: rate, token });
    let settle = timer(module, token, db.clone(), Arc::downgrade(&cx.world));
    cx.world.post(cx.system_id, move |s| s.schedule_at(due, settle));
    Ok(())
}

/// `dogmaIM.StopModuleRepair`: `godma.py` `StopRepairModule` is idempotent;
/// `False` reports nothing in progress for this caller.
pub(in crate::services) async fn stop(c: &mut Client, args: &[Value]) -> CallResult {
    let Some(module) = args.first().and_then(Value::as_int) else { return Ok(Value::Bool(false)) };
    let Some(owner) = c.session.int("charid") else { return Ok(Value::Bool(false)) };
    if state().get(&module).is_none_or(|repair| repair.owner != owner) { return Ok(Value::Bool(false)); }
    Ok(Value::Bool(finish(&c.db, &c.world, module, None).await?))
}

/// Settle the repair: write `current - repaired` damage once, then publish it.
/// The map claim is the only mutual exclusion; losers return `Ok(false)`.
async fn finish(db: &Db, world: &Arc<World>, module: i64, token: Option<u64>) -> Result<bool, CallError> {
    let Some(repair) = claim(module, token) else { return Ok(false) };
    let repaired = repaired_hp(repair.paid_hp, repair.rate_hp_per_min, filetime_now() - repair.started_at);
    let (owner, ship) = (repair.owner, repair.ship);
    let fences = online::capture_online_fences(world, &[owner]).await;
    let sde = world.sde().clone();
    let _lock = market::lock().await;
    let settled = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let (sde, fences) = (sde.clone(), fences.clone());
        Box::pin(async move {
            // A module refit, moved or destroyed mid-repair drops the entry;
            // the consumed paste is not refunded.
            if !ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Ok(false); }
            let valid = load_items(tx, &sde, ItemQuery::Id(module)).await.map_err(db_err)?.into_iter().next()
                .is_some_and(|item| item.owner_id == owner && item.location_id == ship && item.singleton
                    && item.category_id == i64::from(CATEGORY_MODULE) && inventory::is_fitting_flag(item.flag_id));
            if !valid { return Ok(false); }
            let old = dogma::overrides_for(tx, &[module]).await.map_err(db_err)?.iter()
                .find(|v| v.item_id == module && v.attribute_id == ATTR_HULL_DAMAGE).map(|v| v.state());
            let current = old.map(|state| state.value).unwrap_or(0.0);
            let new = (current - repaired).max(0.0);
            if new != current {
                if !dogma::set_override_if(tx, module, ATTR_HULL_DAMAGE, old, new).await.map_err(db_err)? {
                    return Err(CallError::Internal(anyhow::anyhow!("paste repair damage context changed")));
                }
                let change = notify::attribute_change(owner, Value::Int(module), ATTR_HULL_DAMAGE, Value::Float(new), Value::Float(current));
                outbox.notify_pilot(owner, "OnModuleAttributeChanges", vec![Value::list([change])]);
                if let Some((system, generation)) = fences.membership(ship) {
                    let patch = CommittedFitting::overridden_attributes(vec![(Key::Module(module), ATTR_HULL_DAMAGE, Some(new))]);
                    outbox.operation(OnlineChanged::Fitting { system, ship, owner, generation, patch });
                }
            }
            Ok(true)
        })
    }, |settled, outbox| async move { online::dispatch_changed_world(settled, outbox, world) }).await?;
    if settled { info!(module, repaired, "module paste repair settled"); }
    else { warn!(module, system = repair.system, "paste repair dropped: module left its fitted context"); }
    Ok(settled)
}

/// Settle every repair owned by a departing pilot; the client's later
/// `StopModuleRepair` finds no entry. One-shot business writes, not timers.
pub(crate) fn settle_character(world: &World, owner: i64) {
    let modules: Vec<(i64, u64)> = state().iter()
        .filter(|(_, repair)| repair.owner == owner).map(|(&module, repair)| (module, repair.token)).collect();
    if modules.is_empty() { return; }
    let Some(shared) = world.shared() else {
        warn!(owner, "paste repair departure settlement dropped: world unavailable");
        return;
    };
    let db = world.db().clone();
    for (module, token) in modules {
        let (db, world) = (db.clone(), shared.clone());
        tokio::spawn(async move {
            if let Err(error) = finish(&db, &world, module, Some(token)).await {
                warn!(module, ?error, "paste repair departure settlement failed");
            }
        });
    }
}

/// Actor timer entry point; the deferred task settles through `finish`.
fn timer(module: i64, token: u64, db: Db, world: Weak<World>) -> impl FnOnce(&mut System) + Send + 'static {
    move |s: &mut System| {
        let Some(world) = world.upgrade() else { return };
        s.defer(async move {
            if let Err(error) = finish(&db, &world, module, Some(token)).await {
                warn!(module, ?error, "paste repair timer settlement failed");
            }
        });
    }
}

/// Drop an unfinished repair without settling; only tests abandon one.
#[cfg(all(test, feature = "tests-effects"))]
pub(crate) fn forget(module: i64) {
    state().remove(&module);
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
