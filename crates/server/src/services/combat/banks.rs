//! Weapon bank RPC policy mirrored from `eve/client/script/dogma/clientDogmaLocation.py`.
//! Membership, charges and pending reloads come from `game.weapon_banks`,
//! `game.module_charges` and `game.pending_reloads`; fitting and online state use items.
//! Requested changes return their map without notifications. Offlining any member
//! destroys its whole bank; unfit removes a slave or destroys a master's bank.
//! Committed changes stop affected cycles after their current cycle and notify the
//! initiating client for offline/unfit. Slave weapon activation resolves to the master.
//! Membership persists; runtime cycles are not persisted or resumed after restart.
//! Bank mutation and activation are serialized by the single live session in one server process.
use super::*;
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, GROUPABLE_GROUP_IDS};
use crate::dao::items::{charges, combat, dogma, pending_reloads, ship as ship_dao};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::services) enum Operation {
    LinkWeapons, LinkAllWeapons, UnlinkModule, UnlinkAllModules,
    MergeModuleGroups, PeelAndLink, DestroyWeaponBank, GetWeaponBankInfoForShip,
}

/// A captured activation generation, stopped only after the transaction commits.
pub(in crate::services) struct BankStop(Arc<AtomicBool>);
crate::progress::leaf_effect!(BankStop);

pub(in crate::services) fn apply_stop(stop: BankStop) {
    crate::services::effects::lifecycle::stop_generation(&stop.0);
}

pub(in crate::services) fn capture_stop(item: i64) -> Option<BankStop> {
    state().active.get(&item).map(|active| BankStop(active.stop.clone()))
}

pub(in crate::services) struct BankSnapshot {
    rows: Vec<(i64, i64)>,
    active: HashMap<i64, Arc<AtomicBool>>,
}

pub(in crate::services) async fn snapshot(ex: &(impl Executor + ?Sized), ship: i64, extra: &[i64]) -> Result<BankSnapshot, CallError> {
    let rows: Vec<_> = charges::banks(ex, ship).await.map_err(db_err)?.into_iter().map(|r| (r.master_id, r.slave_id)).collect();
    let st = state();
    let active = rows.iter().flat_map(|&(m, s)| [m, s]).chain(extra.iter().copied())
        .filter_map(|id| st.active.get(&id).map(|a| (id, a.stop.clone()))).collect();
    Ok(BankSnapshot { rows, active })
}

pub(in crate::services) async fn changes(ex: &(impl Executor + ?Sized), ship: i64, before: BankSnapshot) -> Result<Vec<BankStop>, CallError> {
    let after: Vec<_> = charges::banks(ex, ship).await.map_err(db_err)?.into_iter().map(|r| (r.master_id, r.slave_id)).collect();
    let old: HashSet<_> = before.rows.iter().copied().collect();
    let new: HashSet<_> = after.iter().copied().collect();
    let mut affected = HashSet::new();
    for &(master, slave) in old.symmetric_difference(&new) {
        let old_master = before.rows.iter().find(|&&(_, s)| s == slave).map_or(slave, |&(m, _)| m);
        affected.insert(old_master);
        affected.insert(master);
    }
    Ok(affected.into_iter().filter_map(|id| before.active.get(&id).cloned().map(BankStop)).collect())
}

pub(in crate::services) async fn remove_member(ex: &(impl Executor + ?Sized), ship: i64, item: i64, master: i64, whole: bool) -> Result<bool, CallError> {
    let count = if whole || item == master { combat::destroy(ex, ship, master).await } else { combat::remove_slave(ex, ship, item).await };
    Ok(count.map_err(db_err)? != 0)
}

pub(in crate::services) fn dispatch<T>(value: T, outbox: Outbox<BankStop>, c: &mut Client) -> Result<T, CallError> {
    let world = c.world.clone();
    for entry in outbox.into_entries() {
        match entry {
            crate::db::outbox::Effect::Operation(stop) => apply_stop(stop),
            crate::db::outbox::Effect::Notification(n) => n.dispatch(&world, Some(&mut *c))?,
        }
    }
    Ok(value)
}

#[derive(Clone)]
struct Module {
    item: Item,
    online: bool,
    charge: Option<(i32, i128)>,
}

fn effective_charge(current: Option<(i32, i64)>, pending: Option<(i32, i64)>) -> Option<(i32, i128)> {
    let current = current.filter(|(_, quantity)| *quantity > 0);
    match pending {
        Some((kind, reserved)) => {
            let have = current.filter(|(current_kind, _)| *current_kind == kind).map_or(0, |(_, quantity)| i128::from(quantity));
            let quantity = have + i128::from(reserved);
            (quantity > 0).then_some((kind, quantity))
        }
        None => current.map(|(kind, quantity)| (kind, i128::from(quantity))),
    }
}

fn validate_pair(a: &Module, b: &Module) -> Result<(), CallError> {
    if !a.online || !b.online { return Err(user_error(messages::MSG_CANT_LINK_MODULE_NOT_ONLINE, vec![])); }
    if a.item.type_id != b.item.type_id { return Err(notify_error(messages::combat::TYPE_MISMATCH)); }
    if !GROUPABLE_GROUP_IDS.contains(&a.item.group_id) || !GROUPABLE_GROUP_IDS.contains(&b.item.group_id) {
        return Err(notify_error(messages::combat::NOT_GROUPABLE));
    }
    if a.charge != b.charge { return Err(notify_error(messages::combat::CHARGE_MISMATCH)); }
    Ok(())
}

async fn modules(ex: &(impl Executor + ?Sized), sde: &Sde, ship: i64, owner: i64) -> Result<Vec<Module>, CallError> {
    let mut items = load_items(ex, sde, ItemQuery::LocationOwner(ship, owner)).await.map_err(db_err)?;
    items.retain(|i| i.singleton && i.category_id == i64::from(CATEGORY_MODULE) && HI_SLOTS.contains(&i.flag_id));
    items.sort_unstable_by_key(|i| (i.flag_id, i.item_id));
    let online = dogma::online(ex, ship).await.map_err(db_err)?;
    let charges = charges::all(ex, ship).await.map_err(db_err)?;
    let flags: Vec<_> = items.iter().map(|i| i.flag_id).collect();
    let pending = pending_reloads::for_flags(ex, ship, &flags).await.map_err(db_err)?;
    Ok(items.into_iter().map(|item| {
        let current = charges.iter().find(|c| c.flag_id == item.flag_id).map(|c| (c.type_id, c.quantity));
        let reload = pending.iter().rev().find(|r| r.flag_id == item.flag_id && r.module_id == item.item_id).map(|r| (r.type_id, r.quantity));
        let online = online.iter().any(|r| r.item_id == item.item_id);
        Module { item, online, charge: effective_charge(current, reload) }
    }).collect())
}

fn module(items: &[Module], id: i64) -> Result<&Module, CallError> {
    items.iter().find(|m| m.item.item_id == id).ok_or_else(|| notify_error(messages::combat::INVALID_MODULE))
}

fn grouped(items: &[Module], rows: &[charges::Bank]) -> Result<Vec<(i64, Vec<i64>)>, CallError> {
    let mut groups: BTreeMap<(i32, Option<(i32, i128)>), Vec<&Module>> = BTreeMap::new();
    for m in items.iter().filter(|m| GROUPABLE_GROUP_IDS.contains(&m.item.group_id)) {
        if !m.online { return Err(user_error(messages::MSG_CANT_LINK_MODULE_NOT_ONLINE, vec![])); }
        groups.entry((m.item.type_id, m.charge)).or_default().push(m);
    }
    Ok(groups.into_values().filter(|group| group.len() > 1).map(|group| {
        let master = group.iter().find(|m| rows.iter().any(|r| r.master_id == m.item.item_id))
            .unwrap_or(&group[0]).item.item_id;
        (master, group.iter().filter_map(|m| (m.item.item_id != master).then_some(m.item.item_id)).collect())
    }).collect())
}

async fn operation(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<BankStop>, ship: i64, owner: i64, method: Operation, first: i64, second: i64) -> CallResult {
    if !ship_dao::lock_reload_ship(ex, ship).await.map_err(db_err)? { return Err(notify_error(messages::combat::NOT_YOUR_SHIP)); }
    let hull = load_items(ex, sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next();
    if !hull.is_some_and(|h| h.owner_id == owner && h.category_id == i64::from(CATEGORY_SHIP)) {
        return Err(notify_error(messages::combat::NOT_YOUR_SHIP));
    }
    if method == Operation::GetWeaponBankInfoForShip { return banks(ex, ship).await; }
    let extra: Vec<_> = if method == Operation::LinkAllWeapons {
        load_items(ex, sde, ItemQuery::LocationOwner(ship, owner)).await.map_err(db_err)?.into_iter().map(|i| i.item_id).collect()
    } else { vec![first, second] };
    let before = snapshot(ex, ship, &extra).await?;
    let result = match method {
        Operation::UnlinkAllModules => { combat::clear_banks(ex, ship).await.map_err(db_err)?; Ok(Value::None) }
        Operation::DestroyWeaponBank => {
            let master = charges::bank_of(ex, ship, first).await.map_err(db_err)?;
            combat::destroy(ex, ship, master).await.map_err(db_err)?;
            Ok(Value::None)
        }
        Operation::UnlinkModule => {
            let master = charges::bank_of(ex, ship, first).await.map_err(db_err)?;
            let peeled = combat::peel_first(ex, ship, master).await.map_err(db_err)?
                .ok_or_else(|| notify_error(messages::combat::BANK_NOT_FOUND))?;
            Ok(Value::Int(peeled))
        }
        Operation::LinkAllWeapons => {
            let items = modules(ex, sde, ship, owner).await?;
            let rows = charges::banks(ex, ship).await.map_err(db_err)?;
            let groups = grouped(&items, &rows)?;
            combat::clear_banks(ex, ship).await.map_err(db_err)?;
            for (master, slaves) in groups { for slave in slaves { combat::move_bank(ex, ship, master, slave).await.map_err(db_err)?; } }
            banks(ex, ship).await
        }
        Operation::LinkWeapons | Operation::MergeModuleGroups | Operation::PeelAndLink => {
            let target = charges::bank_of(ex, ship, first).await.map_err(db_err)?;
            let source = charges::bank_of(ex, ship, second).await.map_err(db_err)?;
            if target == source { return banks(ex, ship).await; }
            let items = modules(ex, sde, ship, owner).await?;
            let a = module(&items, target)?;
            let slaves = charges::slaves(ex, ship, source).await.map_err(db_err)?;
            if method == Operation::PeelAndLink {
                let Some(slave) = slaves.first() else { return banks(ex, ship).await; };
                validate_pair(a, module(&items, slave.slave_id)?)?;
                combat::remove_slave(ex, ship, slave.slave_id).await.map_err(db_err)?;
                combat::move_bank(ex, ship, target, slave.slave_id).await.map_err(db_err)?;
            } else if method == Operation::LinkWeapons && source != second {
                validate_pair(a, module(&items, second)?)?;
                combat::remove_slave(ex, ship, second).await.map_err(db_err)?;
                combat::move_bank(ex, ship, target, second).await.map_err(db_err)?;
            } else {
                validate_pair(a, module(&items, source)?)?;
                for slave in slaves { validate_pair(a, module(&items, slave.slave_id)?)?; }
                combat::move_bank(ex, ship, target, source).await.map_err(db_err)?;
            }
            banks(ex, ship).await
        }
        Operation::GetWeaponBankInfoForShip => unreachable!(),
    }?;
    for stop in changes(ex, ship, before).await? { outbox.operation(stop); }
    Ok(result)
}

fn authorize_ship(requested: Option<i64>, active: Option<i64>) -> Result<i64, CallError> {
    requested.filter(|ship| Some(*ship) == active).ok_or_else(|| notify_error(messages::combat::NOT_YOUR_SHIP))
}

pub(in crate::services) async fn weapon_groups(c: &mut Client, call: &Call<'_>, method: Operation) -> CallResult {
    let ship = authorize_ship(call.args.first().and_then(|v| v.as_int()), c.session.int("shipid"))?;
    let owner = c.session.int("charid").ok_or_else(|| notify_error(messages::combat::NOT_YOUR_SHIP))?;
    let first = call.args.get(1).and_then(|v| v.as_int()).unwrap_or_default();
    let second = call.args.get(2).and_then(|v| v.as_int()).unwrap_or_default();
    let sde = c.sde.clone();
    let db = c.db.clone();
    db.transaction(|tx, outbox: &mut Outbox<BankStop>| {
        let sde = sde.clone();
        Box::pin(async move { operation(tx, &sde, outbox, ship, owner, method, first, second).await })
    }, |value, outbox| async move { dispatch(value, outbox, c) }).await
}

#[cfg(test)] mod tests;
