//! Combat targeting and ammunition loading, mirroring eve/client/script/dogma/clientDogmaLocation.py.
//! Uses game.items, game.module_charges and game.pending_reloads for inventory and reservations.
//! Manual and empty-magazine ancillary reloads share SDE charge compatibility,
//! reservations and actor deadlines that do not precede the advertised FILETIME.
//! Manual reload and unload use current shared-bay access; consumed charges become
//! the pilot's reservation. Automatic reload searches only the pilot's ship cargo.
//! Shared unload viewers receive the atomic deposit's actual row and prior quantity,
//! distinguishing an existing empty stack from a newly inserted row.
//! Cold reload and unload collect their existing item writes and parent lineages before
//! the shared hull-first lock acquisition; prepared deposits never select a later stack.
//! Replacement reloads validate net cargo contents including reserved source space.
//! First insertion and a completion that finds the magazine empty prime the
//! complete charge sublocation; positive same-type refills send only quantity
//! changes. Completion counts the charge currently loaded plus surviving
//! reservations. A different charge type, or a module no longer at its original
//! ship and fitting slot for the reservation owner, rolls the completion back
//! and keeps the reservations. Cold insertions and removals publish fitted
//! topology, not shots.
//! Client behavior for these notifications has not yet been verified.
//! Reload timer markers are memory-only; newer starts return older reservations, and startup recovery returns pending charges to their sources rather than resuming timers.
//! Accepted targeting attempts cancel arrival jump cloak before the lock timer.
//! A jam clears completed and pending locks and refuses new lock transitions;
//! acquisition is possible again when the jam expires. Locks are not resumed
//! after restart.
//! Lock capacity is checked at acquisition and at completion; lock range and
//! live actor visibility are re-evaluated at admission, at the scheduled
//! completion and on every existing system tick, which drops completed locks
//! that no longer qualify.
//! Announced player locks remember their recipient until removal; passive locks
//! send no target-side acquisition or loss notification.
//! A queued lock failure keeps the failed UI attempt's identity: it ends that
//! wait past a different internal pending, but not a current UI or completed lock.
//! Cycle-owned attempts require an active, unstopped generation at acquisition;
//! completion only requires that generation not to have been stopped.
use super::*;
use super::charge_reservations::reserve_load;
use crate::services::effects::online::{self, OnlineChanged};
mod reloading;
pub(crate) mod completion;
pub(crate) mod locking;

// ---------------------------------------------------------------- targeting

/// The one cancellation publication for a discarded pending attempt: the
/// connection that started it through the targeting UI gets a single internal
/// failure request carrying the failed attempt's identity; attempts without a
/// UI owner end silently.
pub(super) fn publish_pending_failure(character: i64, target: i64, lock: &mut PendingLock) {
    if let Some(owner) = lock.ui_owner.take() {
        let _ = owner.send(crate::client::target_lock_failure(character, target, lock.attempt));
    }
}

/// Run `emit` only while the failed attempt's pair holds no completed lock and
/// no still-current pending attempt the answer could cancel: a queued failure
/// is dropped while the same attempt or a different UI-owned attempt is
/// pending, or a completed lock exists, but a different internal attempt does
/// not protect the old wait. Check and emit share one State lock hold.
pub(crate) fn with_pending_failure_current(failure: crate::client::TargetLockFailure, emit: impl FnOnce()) -> bool {
    let st = state();
    if st.targets.get(&failure.character).is_some_and(|targets| targets.contains(&failure.target))
        || st.locking.get(&(failure.character, failure.target))
            .is_some_and(|current| current.attempt == failure.attempt || current.ui_owner.is_some())
    {
        return false;
    }
    emit();
    true
}


pub(crate) fn targets_of(char_id: i64) -> Vec<i64> {
    state().targets.get(&char_id).map(|t| t.iter().copied().collect()).unwrap_or_default()
}

pub(in crate::services) fn cancel_target(character: i64, target: i64) {
    if let Some(mut lock) = state().locking.remove(&(character, target)) {
        publish_pending_failure(character, target, &mut lock);
    }
}

pub(crate) fn has_target(character: i64, target: i64) -> bool {
    state().targets.get(&character).is_some_and(|targets| targets.contains(&target))
}

/// Ok(None): the character has no completed lock; Ok(Some(id)): exactly one;
/// Err(()): more than one. Pending lock attempts never count.
pub(crate) fn sole_target(character: i64) -> Result<Option<i64>, ()> {
    let state = state();
    let mut targets = state.targets.get(&character).into_iter().flatten();
    let Some(&target) = targets.next() else { return Ok(None) };
    if targets.next().is_some() { return Err(()) }
    Ok(Some(target))
}

impl State {
    pub(super) fn begin_lock(&mut self, ship: i64, pilot: i64, target: i64, attempt: u64, now: i64, ui_owner: Option<crate::space::NotifyTx>) -> bool {
        if jamming::jammed_in(self, ship, now) { return false; }
        self.locking.insert((pilot, target), PendingLock { attempt, ui_owner });
        true
    }

    pub(super) fn complete_lock(&mut self, ship: i64, pilot: i64, target: i64, attempt: u64, now: i64, present: bool) -> (bool, bool) {
        let wanted = self.locking.get(&(pilot, target)).is_some_and(|lock| lock.attempt == attempt);
        let removed = if wanted { self.locking.remove(&(pilot, target)) } else { None };
        let completed = wanted && present && !jamming::jammed_in(self, ship, now);
        if completed {
            self.targets.entry(pilot).or_default().insert(target);
        } else if let Some(mut lock) = removed { publish_pending_failure(pilot, target, &mut lock); }
        (wanted, completed)
    }
}


pub(in crate::services) fn targeters_of(ship: i64) -> Vec<i64> {
    targeters_in(&state(), ship)
}

fn targeters_in(st: &State, ship: i64) -> Vec<i64> {
    st.targets.iter().filter_map(|(&pilot, targets)| targets.contains(&ship).then_some(pilot)).collect()
}

/// AddTarget(targetID) -> (lockPending, targets). The lock completes after the lock
/// time from the ship's scan resolution and the target's signature radius, with
/// `OnTarget('add', targetID)`.
pub(in crate::services) async fn add_target(c: &mut Client, target: i64) -> CallResult {
    let Some(cx) = ctx(c) else { return Err(user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![])) };
    add_target_impl(&cx, target, false, None, Some(cx.notify.clone())).await
}

pub(crate) async fn add_target_ctx(cx: &Ctx, target: i64, silent: bool) -> CallResult {
    add_target_impl(cx, target, silent, None, None).await
}

pub(in crate::services) async fn add_target_for_cycle(cx: &Ctx, target: i64, silent: bool, module: i64, generation: Arc<AtomicBool>) -> CallResult {
    // A passive lock answers the client's own targeting request, so it owns the
    // UI's waiting state; an automatic cycle's attempt does not.
    add_target_impl(cx, target, silent, Some((module, generation)), silent.then(|| cx.notify.clone())).await
}

async fn add_target_impl(cx: &Ctx, target: i64, silent: bool, owner: Option<(i64, Arc<AtomicBool>)>, ui_owner: Option<crate::space::NotifyTx>) -> CallResult {
    crate::services::effects::cloak::before_lock(cx).await?;
    let request = locking::LockRequest {
        character: cx.char_id,
        ship: cx.ship_id,
        system: cx.system_id,
        target,
        silent,
        owner,
        ui_owner,
        notify: cx.notify.clone(),
    };
    let sde = cx.world.sde().clone();
    // Ball positions, scene membership and the resident fitted locking limits
    // are read in one actor turn, which also registers the completion timer.
    cx.world.with(cx.system_id, move |s| locking::add_in_system(s, &sde, request))
        .await
        .ok_or_else(|| user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![]))?
}

fn lock_owner_valid(st: &State, owner: Option<&(i64, Arc<AtomicBool>)>) -> bool {
    owner.is_none_or(|(module, generation)| !generation.load(Ordering::Relaxed)
        && st.active.get(module).is_some_and(|active| !active.forced_inactive && Arc::ptr_eq(&active.stop, generation)))
}

fn finish_lock(st: &mut State, ship: i64, character: i64, target: i64, attempt: u64, now: i64, valid: bool, limit: usize, owner: Option<&(i64, Arc<AtomicBool>)>) -> (bool, bool) {
    let valid = valid && !owner.is_some_and(|(_, generation)| generation.load(Ordering::Relaxed))
        && st.targets.get(&character).is_none_or(|targets| targets.len() < limit.max(1) && !targets.contains(&target));
    st.complete_lock(ship, character, target, attempt, now, valid)
}

fn notify_lock(target: i64, ship: i64, silent: bool, owner: impl FnOnce(Vec<Value>), other: impl FnOnce(Vec<Value>)) {
    owner(vec![Value::str("add"), Value::Int(target)]);
    if !silent { other(vec![Value::str("otheradd"), Value::Int(ship)]); }
}

pub(crate) fn drop_targets(identity: &Identity, world: &World, space_system: Option<i64>, targets: &[i64], mut notify: impl FnMut(&str, Vec<Value>)) {
    let char_id = identity.char_id;
    if char_id == 0 { return; }
    let (removed, pending, notices) = {
        let mut st = state();
        let mut pending = Vec::new();
        for target in targets {
            if let Some(mut lock) = st.locking.remove(&(char_id, *target)) {
                publish_pending_failure(char_id, *target, &mut lock);
                pending.push(*target);
            }
        }
        let removed: Vec<_> = targets.iter().copied().filter(|t| st.targets.get_mut(&char_id).is_some_and(|set| set.remove(t))).collect();
        let notices = take_announced(&mut st, |ch, target| ch == char_id && targets.contains(&target));
        (removed, pending, notices)
    };
    notify_lost(world, notices);
    remove_target_projections(identity, world, space_system, removed);
    for t in targets {
        if !pending.contains(t) {
            notify("OnTarget", vec![Value::str("lost"), Value::Int(*t), Value::None]);
        }
    }
}

/// Remove outgoing effects for locks removed by an explicit targeting operation.
pub(in crate::services) fn remove_target_projections(identity: &Identity, world: &World, space_system: Option<i64>, targets: Vec<i64>) {
    let (Some(system), Some(ship)) = (space_system, identity.ship_id) else { return };
    if targets.is_empty() { return; }
    world.post(system, move |s| remove_target_projections_in_system(s, ship, &targets));
}

fn remove_target_projections_in_system(s: &mut crate::space::System, ship: i64, targets: &[i64]) {
    for &target in targets { s.remove_projections_between(ship, target); }
}

fn take_announced(st: &mut State, mut matches: impl FnMut(i64, i64) -> bool) -> Vec<(i64, i64)> {
    let mut notices = Vec::new();
    st.announced_locks.retain(|&(character, target), entry| {
        if matches(character, target) { notices.push(*entry); false } else { true }
    });
    notices
}

fn notify_lost(world: &World, notices: Vec<(i64, i64)>) {
    for (ship, pilot) in notices {
        world.notify_pilot(pilot, "OnTarget", vec![Value::str("otherlost"), Value::Int(ship), Value::None]);
    }
}

pub(super) fn clear_character_lock_state(st: &mut State, character: i64) -> (HashSet<i64>, Vec<(i64, i64)>) {
    st.locking.retain(|&(ch, target), lock| {
        if ch == character { publish_pending_failure(ch, target, lock); false } else { true }
    });
    let targets = st.targets.remove(&character).unwrap_or_default();
    let notices = take_announced(st, |ch, _| ch == character);
    (targets, notices)
}

pub(in crate::services) fn clear_character_locks(world: &World, character: i64) -> Vec<i64> {
    let (targets, notices) = clear_character_lock_state(&mut state(), character);
    notify_lost(world, notices);
    targets.into_iter().collect()
}

/// Clear incoming and outgoing locks without discarding ship condition or capacitor.
pub(in crate::services) fn clear_locks_on(world: &World, system: i64, ship: i64, owner: i64) {
    let (incoming, outgoing, notices) = {
        let mut st = state();
        let (incoming, outgoing) = clear_lock_state(&mut st, ship, owner);
        let notices = take_announced(&mut st, |ch, target| ch == owner || target == ship);
        (incoming, outgoing, notices)
    };
    notify_lost(world, notices);
    world.post(system, move |s| {
        for target in outgoing {
            s.remove_projections_between(ship, target);
            s.notify_pilot(owner, "OnTarget", vec![Value::str("lost"), Value::Int(target), Value::None]);
        }
        for character in incoming {
            if let Some(source) = s.pilot_ball(character).map(|ball| ball.id) {
                s.remove_projections_between(source, ship);
            }
            s.notify_pilot(character, "OnTarget", vec![Value::str("lost"), Value::Int(ship), Value::None]);
        }
    });
}

fn clear_lock_state(st: &mut State, ship: i64, owner: i64) -> (Vec<i64>, HashSet<i64>) {
    st.locking.retain(|&(character, target), lock| {
        if character == owner || target == ship { publish_pending_failure(character, target, lock); false } else { true }
    });
    let outgoing = st.targets.remove(&owner).unwrap_or_default();
    let incoming = st.targets.iter_mut()
        .filter_map(|(&character, targets)| targets.remove(&ship).then_some(character)).collect();
    (incoming, outgoing)
}

/// A destroyed ball is no longer anyone's target.
pub(super) fn forget_target(world: &World, target: i64) {
    let (holders, notices) = {
        let mut st = state();
        st.locking.retain(|&(character, t), lock| {
            if t == target { publish_pending_failure(character, t, lock); false } else { true }
        });
        let holders: Vec<i64> = st.targets.iter_mut().filter_map(|(ch, set)| set.remove(&target).then_some(*ch)).collect();
        let notices = take_announced(&mut st, |_, t| t == target);
        (holders, notices)
    };
    notify_lost(world, notices);
    for ch in holders {
        world.notify_pilot(ch, "OnTarget", vec![Value::str("lost"), Value::Int(target), Value::str("Exploding")]);
    }
}

// ---------------------------------------------------------------- charges

/// The charge sublocation key `(shipID, flagID, typeID)`.
pub(crate) fn charge_key(ship: i64, flag: i32, type_id: i32) -> Value {
    Value::tuple([Value::Int(ship), Value::Int(flag as i64), Value::Int(type_id as i64)])
}

/// `OnModuleAttributeChanges` for a charge sublocation's quantity (clientDogmaLocation
/// fits/unfits the sublocation from it).


/// `{shipID: {flagID: row(locationID, flagID, typeID, quantity)}}` for the ship state.


pub(crate) async fn loaded(db: &(impl Executor + ?Sized), ship: i64, flag: i32) -> Result<Option<(i32, i64)>, CallError> {
    Ok(crate::dao::items::charges::loaded(db, ship, flag)
        .await
        .map_err(db_err)?
        .map(|r| (r.type_id, r.quantity)))
}

use crate::services::items::{deposit_prepared, is_static_location, DepositPreparation};
use crate::dao::items::parent_locks::{LockRequest, LockStrength, LockedItems};
use crate::services::inventory::destination::{self, ParentGuard, ParentSnapshot};

/// Modules of a weapon group, master first. The input must be the master.
pub(crate) async fn bank(db: &(impl Executor + ?Sized), ship: i64, master: i64) -> Result<Vec<i64>, CallError> {
    let rows = crate::dao::items::charges::slaves(db, ship, master).await.map_err(db_err)?;
    Ok(std::iter::once(master).chain(rows.iter().map(|r| r.slave_id)).collect())
}

/// Resolve a bank member to its master, or return an ungrouped module unchanged.
pub(in crate::services) async fn master_for_member(ex: &(impl Executor + ?Sized), ship: i64, member: i64) -> Result<i64, CallError> {
    crate::dao::items::charges::bank_of(ex, ship, member).await.map_err(db_err)
}

/// Empty a module's charges into `location`/`flag` (cargo by default).
pub(in crate::services) async fn unload<X: Send>(db: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>, owner: i64, ship: i64, module: &Item, location: i64, flag: i32, prep: &mut DepositPreparation, locks: &LockedItems<'_>) -> Result<(), CallError> {
    let destination = destination::Destination::owned(owner, location, flag);
    if prep.destination != destination { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
    unload_to(db, sde, outbox, owner, ship, module, prep, locks, None).await.map(|_| ())
}

async fn unload_to<X: Send>(db: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>, owner: i64, ship: i64, module: &Item, prep: &mut DepositPreparation, locks: &LockedItems<'_>, parent: Option<&ParentGuard<'_, '_>>) -> Result<Option<crate::services::items::DepositReceipt>, CallError> {
    let Some((type_id, qty)) = loaded(db, ship, module.flag_id).await? else { return Ok(None) };
    if prep.type_id != type_id { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
    crate::services::inventory::hold_storage::check_destination(db, sde, prep.destination.capacity_owner, prep.destination.location, prep.destination.flag,
        &[crate::services::inventory::hold_storage::Addition { type_id, singleton: false, quantity: i64::from(qty) }], Default::default(), crate::services::inventory::hold_storage::CapacitySource::Current).await?;
    crate::dao::items::combat::delete_charge(db, ship, module.flag_id).await.map_err(db_err)?;
    outbox.notify_client("OnModuleAttributeChanges", vec![quantity_change(owner, ship, module.flag_id, type_id, 0, qty as i64)]);
    deposit_prepared(db, sde, outbox, prep, qty, ship, Some(locks), parent, Default::default(), crate::services::inventory::hold_storage::CapacitySource::Current).await.map(Some)
}

// Unlike value_ids, scalar module IDs accept Bool through as_int; lists take precedence.
pub(super) fn module_ids(v: Option<&Value>) -> Vec<i64> {
    match v {
        Some(v) if v.items().is_some() => v.items().unwrap().iter().filter_map(|m| m.as_int()).collect(),
        Some(v) => v.as_int().into_iter().collect(),
        None => vec![],
    }
}

/// LoadAmmo(shipID, moduleID | [moduleIDs], [chargeItemIDs], chargeLocationID): fill each
/// module (and its group) with charges from the given stacks, swapping out other types.
/// In space the charges go in after the module's reload time (OnChargeBeingLoadedToModule
/// and nextActivationTime drive the client's reload animation); docked it is immediate.
pub(in crate::services) async fn load_ammo(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let modules = module_ids(call.args.get(1));
    let charge_ids = value_id_list(call.args.get(2));
    let charge_location = call.args.get(3).and_then(|v| v.as_int());
    load_ammo_core(c, ship, modules, charge_ids, charge_location).await
}

pub(in crate::services) async fn load_ammo_core(c: &mut Client, ship: i64, modules: Vec<i64>, charge_ids: Vec<i64>, charge_location: Option<i64>) -> CallResult {
    load_ammo_impl(c, ship, modules, charge_ids, charge_location, None).await.map(|_| Value::None)
}

/// Generate a charge stack in the same transaction that reserves it for reload.
pub(in crate::services) async fn gm_load_ammo(c: &mut Client, ship: i64, modules: Vec<i64>, source: i64, type_id: i32, quantity: i64) -> Result<usize, CallError> {
    load_ammo_impl(c, ship, modules, Vec::new(), Some(source), Some((type_id, quantity))).await
}

async fn load_ammo_impl(c: &mut Client, ship: i64, modules: Vec<i64>, charge_ids: Vec<i64>, charge_location: Option<i64>, generated: Option<(i32, i64)>) -> Result<usize, CallError> {
    let db = c.db.clone();
    let world = c.world.clone();
    let notify = c.notify_tx.clone();
    let sde = c.sde.clone();
    let owner = c.session.int("charid");
    let active_ship = c.session.int("shipid");
    let station = c.session.int("stationid");
    let system = c.session.int("solarsystemid");
    reloading::reload(&db, &world, &notify, sde, ship, modules, charge_ids,
        charge_location, active_ship, station, owner, system, false, None, generated, Some(c)).await
}

/// A provisional reload marker blocks activation while automatic reservation awaits SQL.
/// The actual actor deadline replaces it on success; cancellation and refusals clear it.
struct AutoReloadMarker(i64);

impl Drop for AutoReloadMarker {
    fn drop(&mut self) {
        clear_reload_markers(&mut state().reloading, &[self.0], i64::MAX);
    }
}

/// Reload an empty ancillary magazine from compatible cargo after its cycle stops.
pub(in crate::services) async fn auto_reload(cx: &Ctx, module_id: i64) -> Result<(), CallError> {
    {
        let mut st = state();
        if st.active.contains_key(&module_id) || st.reloading.contains_key(&module_id) { return Ok(()); }
        st.reloading.insert(module_id, i64::MAX);
    }
    let _marker = AutoReloadMarker(module_id);
    let ms = Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, cx.ship_id).await?
        .value(Key::Module(module_id), ATTR_RELOAD_TIME);
    let reload_ms = if ms > 0.0 { ms } else { DEFAULT_RELOAD_MS };
    reloading::reload(&cx.db, &cx.world, &cx.notify, cx.world.sde().clone(), cx.ship_id, vec![module_id],
        Vec::new(), Some(cx.ship_id), Some(cx.ship_id), None, Some(cx.char_id),
        Some(cx.system_id), true, Some(reload_ms), None, None).await.map(|_| ())
}

/// SDE dogma attribute names for module-compatible charge groups.
const CHARGE_GROUP_ATTRIBUTES: [&str; 5] = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"];

fn charge_matches(mut groups: impl Iterator<Item = f64>, group: Option<i32>, module_size: Option<f64>, charge_size: Option<f64>) -> bool {
    group.is_some_and(|group| groups.any(|value| value == f64::from(group)))
        && match module_size {
            Some(size) if size > 0.0 => charge_size == Some(size),
            _ => true,
        }
}

fn compatible_charge(sde: &Sde, module_type: i32, charge: &Item) -> bool {
    let Some(module) = sde.types.get(module_type) else { return false };
    let Some(charge_type) = sde.types.get(charge.type_id) else { return false };
    let groups = CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name))
        .filter_map(|attr| module.attribute(attr.attribute_id));
    let size = sde.dogma.attributes_by_name("chargeSize").next().map(|attr| attr.attribute_id);
    charge.category_id == CATEGORY_CHARGE as i64 && charge_matches(groups, charge_type.group_id,
        size.and_then(|id| module.attribute(id)), size.and_then(|id| charge_type.attribute(id)))
}

/// Whether the module type declares charge compatibility: any positive charge
/// group or a positive charge size. Unknown types conservatively require one.
pub(crate) fn requires_loaded_charge(sde: &Sde, module_type: i32) -> bool {
    let Some(module) = sde.types.get(module_type) else { return true };
    CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name))
        .any(|attr| module.attribute(attr.attribute_id).is_some_and(|value| value > 0.0))
        || sde.dogma.attributes_by_name("chargeSize").next()
            .and_then(|attr| module.attribute(attr.attribute_id)).is_some_and(|value| value > 0.0)
}

pub(super) type ReservedLoad = (i64, i32, i32, i64, i64);

#[derive(Clone)]
struct PendingLoad {
    owner: i64,
    reload_ids: Vec<i64>,
    external_sources: Vec<i64>,
    external_facts: Vec<crate::dao::items::pending_reloads::PendingReload>,
    load: ReservedLoad,
}

fn group_loads(loads: Vec<PendingLoad>, duration: impl Fn(i64) -> f64) -> BTreeMap<(i64, i32), Vec<(PendingLoad, f64)>> {
    let mut groups = BTreeMap::new();
    for load in loads {
        let ms = match duration(load.load.0) { ms if ms > 0.0 => ms, _ => DEFAULT_RELOAD_MS };
        groups.entry((filetime_ticks_from_ms(ms), load.load.2)).or_insert_with(Vec::new).push((load, ms));
    }
    groups
}

pub(super) fn clear_reload_markers(reloading: &mut HashMap<i64, i64>, ids: &[i64], until: i64) {
    for id in ids {
        if reloading.get(id) == Some(&until) { reloading.remove(id); }
    }
}

/// UnloadAmmo(shipID, moduleID | [moduleIDs], destination[, quantity]); destination is a
/// container ID or (containerID, ownerID, flag).
pub(in crate::services) async fn unload_ammo(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let modules = module_ids(call.args.get(1));
    let (location, flag) = match call.args.get(2) {
        Some(v) if v.items().is_some() => {
            let t = v.items().unwrap();
            (t.first().and_then(|x| x.as_int()).unwrap_or(ship), t.get(2).and_then(|x| x.as_int()).map(|f| f as i32))
        }
        Some(v) => (v.as_int().unwrap_or(ship), None),
        None => (ship, None),
    };
    let flag = flag.unwrap_or(if location == ship { FLAG_CARGO } else { FLAG_HANGAR });
    unload_ammo_core(c, ship, modules, location, flag).await
}

pub(in crate::services) async fn unload_ammo_core(c: &mut Client, ship: i64, modules: Vec<i64>, location: i64, flag: i32) -> CallResult {
    let identity = crate::services::identity::Identity { system_id: c.session.int("solarsystemid"), ..crate::services::identity::Identity::of_client(c) };
    let owner = identity.char_id;
    let refuse = || notify_error(messages::INSUFFICIENT_AMMUNITION);
    if owner == 0 { return Err(refuse()); }
    let db = c.db.clone();
    let sde = c.sde.clone();
    let container = if identity.station_id == Some(location) {
        crate::services::inventory::access::Container::Hangar { station_id: location, owner_id: owner }
    } else { crate::services::inventory::access::Container::Item(location) };
    let endpoint = crate::services::inventory::reachability::Endpoint { location, flag };
    let reach = crate::services::inventory::reachability::Reachability::capture(&identity, &db, &c.world, identity.system_id, &[endpoint]).await?;
    let audience = crate::services::inventory::notifications::Audience::capture(&identity, &db, &c.world, identity.system_id, &[endpoint]).await?;
    let ancestry = crate::dao::items::deliveries::ancestry(&db, location).await.map_err(db_err)?;
    let parent = if identity.station_id == Some(location) { None } else {
        load_items(&db, &sde, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next()
    };
    let fences = online::capture_online_fences(&c.world, &[owner]).await;
    let armed = super::loaded::LoadedHolds::arm(&c.world, &identity.system_id.map(|system| (system, ship)).into_iter().collect::<Vec<_>>(), notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let _market = crate::services::market::lock().await;
    let mut holds = super::loaded::LoadedHolds::lock(&c.world, armed, notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let settle = holds.settle_rows();
    let engaged_ship = holds.held(ship);
    let charge_view = holds.view(ship);
    let world = c.world.clone();
    let notify = c.notify_tx.clone();
    holds.may_execute();
    let result = db.transaction(
        |tx, outbox: &mut Outbox<OnlineChanged>| {
            let modules = modules.clone();
            let sde = sde.clone();
            let reach = reach.clone();
            let audience = audience.clone();
            let container = container.clone();
            let settle = settle.clone();
            let charge_view = charge_view.clone();
            let ancestry = ancestry.clone();
            let parent = parent.clone();
            let fences = fences.clone();
            Box::pin(async move {
                let destination = crate::services::inventory::movement::authorize_destination(tx, &sde, &identity, &reach, &container, Some(flag)).await?.ok_or_else(refuse)?;
                let mut selected = Vec::new();
                let mut banks = Vec::new();
                for &m in &modules {
                    let ids = bank(tx, ship, m).await?;
                    for &id in &ids {
                        if let Some(module) = load_items(tx, &sde, ItemQuery::IdAt(id, ship)).await.map_err(db_err)?.into_iter().next() {
                            if module.owner_id != owner || !super::super::inventory::is_fitting_flag(module.flag_id) { return Err(refuse()); }
                            let charge = if engaged_ship { charge_view.get(&module.flag_id).copied() } else { loaded(tx, ship, module.flag_id).await? };
                            let (prep, canonical) = if let Some((type_id, _)) = charge {
                                if let Some(index) = selected.iter().position(|(_, _, prep, _): &(Item, _, Option<DepositPreparation>, _)| prep.as_ref().is_some_and(|prep| prep.destination == destination && prep.type_id == type_id)) {
                                    (None, Some(index))
                                } else {
                                    (Some(DepositPreparation::capture(tx, &sde, &destination, type_id).await?), Some(selected.len()))
                                }
                            } else { (None, None) };
                            selected.push((module, charge, prep, canonical));
                        }
                    }
                    banks.push((m, ids));
                }
                let mut requests = vec![LockRequest { item_id: ship, strength: LockStrength::Update }];
                requests.extend(destination::parent_requests(&sde, ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?);
                if parent.as_ref().is_some_and(|item| crate::services::inventory::hold_storage::is_ordinary_container(&sde, item.type_id)) {
                    requests.push(LockRequest { item_id: location, strength: LockStrength::Update });
                }
                requests.extend(selected.iter().map(|(module, _, _, _)| LockRequest { item_id: module.item_id, strength: LockStrength::Share }));
                let mandatory: Vec<_> = requests.iter().map(|r| r.item_id).collect();
                let mut optional = Vec::new();
                for (_, _, prep, _) in &selected {
                    if let Some(prep) = prep {
                        if let Some(request) = prep.candidate_request() {
                            if !mandatory.contains(&request.item_id) { optional.push(request.item_id); }
                            requests.push(request);
                        }
                    }
                }
                let locks = destination::lock_items(tx, &sde, &requests, &optional).await?;
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
                if hull.owner_id != owner || hull.category_id != i64::from(infinity_core::consts::CATEGORY_SHIP)
                    || (identity.ship_id != Some(ship) && !(hull.singleton && identity.station_id == Some(hull.location_id) && hull.flag_id == FLAG_HANGAR)) { return Err(refuse()); }
                if crate::dao::items::deliveries::ancestry(tx, location).await.map_err(db_err)? != ancestry { return Err(refuse()); }
                let snapshot = parent.clone().filter(|item| item.category_id != i64::from(infinity_core::consts::CATEGORY_SHIP)
                    && selected.iter().filter_map(|(_, _, prep, _)| prep.as_ref()).any(|prep| !prep.is_excluded()))
                    .map(|item| ParentSnapshot { item, ancestry: ancestry.clone() });
                let guard = snapshot.as_ref().map(|snapshot| ParentGuard::revalidate(&locks, std::slice::from_ref(snapshot))).transpose()?;
                if !crate::dao::trades::reserved(tx, &[ship, location]).await.map_err(db_err)?.is_empty() { return Err(refuse()); }
                if crate::services::inventory::movement::authorize_destination(tx, &sde, &identity, &reach, &container, Some(flag)).await? != Some(destination) { return Err(refuse()); }
                for (m, ids) in &banks {
                    if bank(tx, ship, *m).await? != *ids { return Err(refuse()); }
                }
                // The charge-row drift reads below compare against the settled absolute.
                crate::services::combat::loaded::settle_in(tx, &settle).await?;
                for (module, charge, _, _) in &selected {
                    let current = load_items(tx, &sde, ItemQuery::IdAt(module.item_id, ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
                    if current.owner_id != module.owner_id || current.type_id != module.type_id || current.flag_id != module.flag_id
                        || current.quantity != module.quantity || current.singleton != module.singleton
                        || loaded(tx, ship, module.flag_id).await? != *charge { return Err(refuse()); }
                }
                let viewers = audience.resolve(tx, &sde).await?;
                let mut before = None;
                for index in 0..selected.len() {
                            let (earlier, current) = selected.split_at_mut(index);
                            let (module, charge, own, canonical) = &mut current[0];
                            if charge.is_some() && before.is_none() {
                                before = Some(online::fitting::capture(tx, &sde, &fences, owner, ship).await?);
                            }
                            if let Some(canonical) = *canonical {
                              let prep = if canonical == index { own.as_mut().unwrap() } else { earlier[canonical].2.as_mut().unwrap() };
                              if let Some(receipt) = unload_to(tx, &sde, outbox, owner, ship, module, prep, &locks, guard.as_ref()).await? {
                                for row in receipt.rows() {
                                    let new = &row.item;
                                    let (previous, change) = match row.old_quantity {
                                        Some(q) => (None, (IX_STACK_SIZE, Value::Int(q))),
                                        None => (Some(Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..new.clone() }), (IX_LOCATION_ID, Value::Int(ship))),
                                    };
                                    viewers.item_change(outbox, previous.as_ref().unwrap_or(new), new, vec![change], owner);
                                }
                              }
                            }
                }
                if let Some(before) = before {
                    let final_fit = Fit::load(tx, sde.clone(), owner, ship).await?;
                    online::fitting::finish(tx, before, final_fit, outbox).await?;
                }
                let touched: Vec<_> = selected.iter().filter(|(_, charge, _, _)| charge.is_some())
                    .map(|(module, _, _, _)| (module.item_id, module.flag_id)).collect();
                let seeds = super::loaded::seed_entries(tx, ship, &touched).await?;
                Ok((Value::None, seeds))
            })
        },
        |value, outbox| async move {
            online::dispatch_mixed_entries(value, outbox, &world, Ok, |_| unreachable!(),
                |n| online::dispatch_notification(n, &world, &notify, Some(owner)))
        },
    ).await;
    let seeds = match &result { Ok((_, seeds)) => vec![(ship, seeds.clone())], _ => Vec::new() };
    holds.finish(&c.db, &c.world, result, &seeds).await.map(|(value, _)| value)
}


#[cfg(all(test, any(feature = "tests-combat", feature = "tests-effects", feature = "tests-ships")))] pub(in crate::services) mod tests;
