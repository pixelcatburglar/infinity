//! Shared inventory subtree validation and destruction, extracted from ordinary
//! `invbroker.TrashItems` so the impound office lifecycle can reuse it. The ordinary
//! trash RPC, impound release/trash and GM space debris cleanup each perform their
//! own authorization and capture, then pass the permitted roots here; closure
//! structure, locking, reservation/job/in-use safety and the transactional runtime
//! cleanup are shared. Impound operations deny pending-reload intersections
//! outright while ordinary trash keeps its established discard-refund semantics.
//! The GM debris custody grants no ownership at all: it only revalidates that each
//! captured root is still a singleton wreck or ordinary container sitting directly
//! in the captured solar system, and it shares the impound reservation refusal.
//! Enacted corporation blueprint locks and pending vote-case reservations also
//! refuse destruction once for the whole verified closure.
use super::access::{corporate_custody, custody_allowed};
use super::destination;
use super::is_fitting_flag;
use super::movement::MoveEffect;
use super::notifications::{notify_corp_item_removed, notify_item_removed};
use super::reachability::{Endpoint, Reachability};
use crate::dao::items::{combat as combat_dao, inventory as inventory_dao, loading::{Item, ItemQuery}, parent_locks::{LockRequest, LockStrength, LockedItems}, pending_reloads};
use crate::dao::insurance;
use crate::db::executor::Executor;
use crate::db::outbox::Outbox;
use crate::sde::Sde;
use crate::services::combat::{banks, reload_recovery, targeting_charges};
use crate::services::effects::online::{self, OnlinePolicy};
use crate::services::{db_err, items::load_items, user_error, CallError};
use infinity_core::consts::{CATEGORY_BLUEPRINT, CATEGORY_MODULE, CATEGORY_SHIP, FLAG_CORP_DELIVERIES, GROUP_WRECK};
use infinity_core::messages;
use marshal::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Which subtree operation the caller is performing; destruction-only checks such
/// as the untrashable list apply to `Destroy`, shared safety checks to both.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::services) enum Operation {
    Destroy,
    Release,
}

/// The per-item authorization the caller proved out of band.
#[derive(Clone, Copy)]
pub(in crate::services) enum CustodyRule {
    /// Ordinary trash: corporation hangar Take, personal ownership or an in-space
    /// shared bay. `me` is the acting character.
    Ordinary { me: i64 },
    /// Impound operations: every descendant must remain owned by the office's
    /// corporation; any personal or foreign descendant fails closed.
    Corporate { corporation: i64 },
    /// GM space debris cleanup: the caller bound each permitted root to a live
    /// system ball out of band, so no per-item ownership is required. `system` is
    /// the solar system every root must still sit in directly; all shared safety
    /// checks keep applying, and pending-reload intersections refuse like impound.
    GmDebris { system: i64 },
}

/// The shared debris root taxonomy: an SDE wreck group member or an ordinary
/// cargo container, used by both the actor selector and post-lock revalidation.
pub(super) fn is_debris_type(sde: &Sde, type_id: i32) -> bool {
    sde.types.get(type_id).and_then(|ty| ty.group_id) == Some(GROUP_WRECK)
        || super::hold_storage::is_ordinary_container(sde, type_id)
}

/// Everything an operation captured before its transaction; the verified
/// post-lock closure must reproduce it exactly or the operation refuses.
#[derive(Clone)]
pub(in crate::services) struct Prepared {
    /// Sorted, deduplicated permitted roots.
    pub roots: Vec<i64>,
    /// The captured descendant closure, sorted by item id.
    pub snapshots: Vec<Item>,
    /// Captured lineage of each snapshot parent location.
    pub lineages: HashMap<i64, Vec<crate::dao::items::deliveries::Ancestor>>,
    /// Physical terminal every captured lineage must resolve to: the trash outer
    /// location for ordinary operations, the office's station for impound ones.
    pub outer: i64,
    /// An ancestor every snapshot lineage must contain, locked into the request
    /// set: the retained office item for impound operations, `None` for ordinary
    /// trash.
    pub boundary: Option<i64>,
    /// Physical endpoints requiring a fresh reachability proof; empty skips it.
    pub endpoints: Vec<Endpoint>,
    pub reach: Option<Reachability>,
    /// Items whose ordinary access flows through an in-space shared bay.
    pub shared_ids: HashSet<i64>,
    /// The acting client's own ship, when it has one; destruction refuses the
    /// closure containing it (whole-closure DB check adds every piloted hull).
    pub active_ship: Option<i64>,
    /// The acting client's station, used by the surviving-hull online policy.
    pub station: Option<i64>,
    /// The acting client's notify channel when the caller is a client; resource
    /// callers pass `None` and notifications route to their pilots.
    pub notify: Option<crate::space::NotifyTx>,
}

/// The locked, verified closure inside the operation's transaction.
pub(in crate::services) struct Verified<'tx> {
    pub locks: LockedItems<'tx>,
    /// Current closure items, sorted by item id.
    pub items: Vec<Item>,
    pub closure: Vec<i64>,
    /// item id -> owning hull for every ship or fitted module in the closure.
    owning_ship: HashMap<i64, i64>,
    /// Pending reloads touching closure hulls (ordinary trash refunds them).
    pending: Vec<pending_reloads::PendingReload>,
    refund: reload_recovery::RefundPreparation,
    /// Gate-captured loaded rows the owning business settles inside the same
    /// transaction before the closure deletes them; empty for cold callers.
    pub loaded_writes: Vec<crate::dao::items::charges::LoadedWrite>,
}

fn refuse() -> CallError {
    user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])
}

/// Lock the complete request set (hulls first through `lock_items`), then prove
/// the captured closure, custody and every shared safety check still hold.
pub(in crate::services) async fn verify_closure<'tx>(
    tx: &'tx (impl Executor + ?Sized),
    sde: &Arc<Sde>,
    prepared: &Prepared,
    custody: &CustodyRule,
    operation: Operation,
) -> Result<Verified<'tx>, CallError> {
    let mut ancestors: Vec<i64> = prepared.lineages.values().flat_map(|rows| rows.iter().map(|row| row.item_id)).collect();
    ancestors.sort_unstable();
    ancestors.dedup();
    let captured = inventory_dao::descendants_and_ancestors(tx, &prepared.roots, &ancestors).await.map_err(db_err)?;
    let mut ship_ids = std::collections::BTreeSet::new();
    for item in &prepared.snapshots {
        if item.category_id == i64::from(CATEGORY_SHIP) {
            ship_ids.insert(item.item_id);
        } else if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) {
            ship_ids.insert(item.location_id);
        }
    }
    let closure_member = |row: &pending_reloads::PendingReload| {
        prepared.snapshots.iter().any(|item| (item.category_id == i64::from(CATEGORY_SHIP) && item.item_id == row.ship_id)
            || (item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) && item.item_id == row.module_id))
    };
    let mut pending = Vec::new();
    for &ship in &ship_ids {
        pending.extend(pending_reloads::for_ship(tx, ship).await.map_err(db_err)?.into_iter().filter(|row| closure_member(row)));
    }
    pending.sort_by_key(|row| row.reload_id);
    let refund = match custody {
        CustodyRule::Ordinary { .. } => reload_recovery::RefundPreparation::discard(tx, sde, &pending).await?,
        // Impound and GM debris operations never settle a reload inside the
        // removed trees: an intersecting reservation refuses the operation below.
        CustodyRule::Corporate { .. } | CustodyRule::GmDebris { .. } => reload_recovery::RefundPreparation::discard(tx, sde, &[]).await?,
    };
    let mut requests: Vec<_> = captured.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
    if let Some(boundary) = prepared.boundary {
        requests.push(LockRequest { item_id: boundary, strength: LockStrength::Update });
    }
    requests.extend_from_slice(refund.requests());
    let locks = destination::lock_items(tx, sde, &requests, &[]).await?;
    let mut current_pending = Vec::new();
    for &ship in &ship_ids {
        current_pending.extend(pending_reloads::for_ship(tx, ship).await.map_err(db_err)?.into_iter().filter(|row| closure_member(row)));
    }
    current_pending.sort_by_key(|row| row.reload_id);
    if current_pending.iter().any(|row| !pending.contains(row)) {
        return Err(refuse());
    }
    let closure = inventory_dao::descendants(tx, &prepared.roots).await.map_err(db_err)?;
    if prepared.roots.iter().any(|id| !locks.is_write_locked(*id))
        || closure.iter().any(|id| !locks.is_write_locked(*id)) {
        return Err(refuse());
    }
    let mut impound_scope = closure.clone();
    if let Some(boundary) = prepared.boundary {
        impound_scope.push(boundary);
    }
    if inventory_dao::has_clone_child(tx, &impound_scope).await.map_err(db_err)? {
        return Err(refuse());
    }
    if matches!(custody, CustodyRule::Corporate { .. } | CustodyRule::GmDebris { .. })
        && !pending_reloads::intersecting(tx, &impound_scope).await.map_err(db_err)?.is_empty() {
        return Err(refuse());
    }
    if let Some(boundary) = prepared.boundary {
        if !locks.is_write_locked(boundary)
            || prepared.lineages.values().any(|rows| !rows.iter().any(|row| row.item_id == boundary)) {
            return Err(refuse());
        }
        if let CustodyRule::Corporate { corporation } = custody {
            if !locks.row(boundary).is_some_and(|row| row.owner_id == *corporation
                && row.location_id == prepared.outer && row.jump_clone_id.is_none()) {
                return Err(refuse());
            }
        }
    }
    for (&parent, captured) in &prepared.lineages {
        if crate::dao::items::deliveries::ancestry(tx, parent).await.map_err(db_err)? != *captured {
            return Err(refuse());
        }
        let by_id: HashMap<_, _> = captured.iter().map(|row| (row.item_id, row)).collect();
        let mut seen = HashSet::new();
        let mut cursor = parent;
        while let Some(row) = by_id.get(&cursor) {
            if !seen.insert(cursor) { return Err(refuse()); }
            cursor = row.location_id;
        }
        if cursor != prepared.outer || seen.len() != captured.len() {
            return Err(refuse());
        }
    }
    let mut items = load_items(tx, sde, ItemQuery::Ids(&closure)).await.map_err(db_err)?;
    items.sort_by_key(|i| i.item_id);
    if items.len() != closure.len() {
        return Err(refuse());
    }
    let snapshots_ids: Vec<i64> = prepared.snapshots.iter().map(|item| item.item_id).collect();
    if closure != snapshots_ids || items.iter().zip(&prepared.snapshots).any(|(a, b)| a.item_id != b.item_id
        || a.owner_id != b.owner_id || a.location_id != b.location_id || a.flag_id != b.flag_id
        || a.type_id != b.type_id || a.singleton != b.singleton || a.quantity != b.quantity) {
        return Err(refuse());
    }
    if let CustodyRule::GmDebris { system } = custody {
        // The selector bound each root to a live system ball; under the locks and
        // the freshly loaded rows every root must still be a singleton debris type
        // sitting directly in the captured system. A moved, stacked or retyped
        // root refuses the whole batch like any other staleness.
        if operation != Operation::Destroy || prepared.outer != *system {
            return Err(refuse());
        }
        for root in &prepared.roots {
            let current = items.binary_search_by_key(root, |item| item.item_id)
                .map(|index| &items[index]).map_err(|_| refuse())?;
            if current.location_id != *system || !current.singleton || !is_debris_type(sde, current.type_id) {
                return Err(refuse());
            }
        }
    }
    if let Some(reach) = &prepared.reach {
        if reach.check_many(tx, sde, &prepared.endpoints).await?.into_iter().any(|allowed| !allowed) {
            return Err(refuse());
        }
    }
    if !crate::dao::trades::reserved(tx, &closure).await.map_err(db_err)?.is_empty()
        || crate::dao::industry::has_active_blueprint_job(tx, &closure).await.map_err(db_err)? {
        return Err(refuse());
    }
    // Enacted corporation locks and pending vote-case reservations freeze blueprint
    // originals: destruction checks the whole verified closure once here, while
    // impound release checks each moving root inside `move_core` instead.
    if operation == Operation::Destroy {
        let candidates: Vec<i64> = items.iter()
            .filter(|item| item.category_id == i64::from(CATEGORY_BLUEPRINT))
            .map(|item| item.item_id).collect();
        crate::services::corps::item_locks::ensure_mutable(tx, &candidates).await?;
    }
    let mut owning_ship = HashMap::new();
    for item in &items {
        let ship = if item.category_id == i64::from(CATEGORY_SHIP) {
            item.item_id
        } else if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) {
            let parent = match items.iter().find(|parent| parent.item_id == item.location_id) {
                Some(parent) => parent.clone(),
                None => load_items(tx, sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next()
                    .ok_or_else(refuse)?,
            };
            if parent.category_id != i64::from(CATEGORY_SHIP) || !parent.singleton {
                return Err(refuse());
            }
            parent.item_id
        } else {
            continue;
        };
        owning_ship.insert(item.item_id, ship);
    }
    let hulls: Vec<i64> = ship_ids.iter().copied().filter(|ship| closure.contains(ship)).collect();
    if !inventory_dao::actively_piloted(tx, &hulls).await.map_err(db_err)?.is_empty() {
        return Err(refuse());
    }
    for item in &items {
        if Some(item.item_id) == prepared.active_ship { return Err(refuse()); }
        if operation == Operation::Destroy
            && sde.typelists.is_untrashable(item.type_id, item.group_id as i32, item.category_id as i32) {
            return Err(refuse());
        }
        match custody {
            CustodyRule::Ordinary { me } => {
                let custody = if prepared.station.is_none() && prepared.shared_ids.contains(&item.item_id) { None }
                    else { corporate_custody(tx, Some(sde), item.item_id).await? };
                if let Some((_, _, flag)) = custody {
                    if flag == FLAG_CORP_DELIVERIES {
                        return Err(refuse());
                    }
                    if !custody_allowed(tx, sde, item.item_id, *me, Some(prepared.outer), true).await?.unwrap_or(false) { return Err(refuse()); }
                } else if item.owner_id != *me && !prepared.shared_ids.contains(&item.item_id) {
                    return Err(refuse());
                }
            }
            CustodyRule::Corporate { corporation } => {
                if item.owner_id != *corporation {
                    return Err(refuse());
                }
            }
            // GM debris grants no ownership: the per-root revalidation above is
            // its whole custody check; shared safety still ran for every item.
            CustodyRule::GmDebris { .. } => {}
        }
    }
    Ok(Verified { locks, items, closure, owning_ship, pending, refund, loaded_writes: Vec::new() })
}

/// Offline a fitted module whose hull is deleted with it, or through the ordinary
/// policy when its hull survives. Bank, charge, heat and fitting cleanup follow
/// in this order: a destroyed ship clears its weapon banks, deletes its charges,
/// queues the bank stops and uninsures; a deleted module offlines first, an
/// already-offline one leaves its bank and queues heat and fitting cleanup, and
/// its charge row is deleted last.
async fn clear_deleted_runtime(
    tx: &(impl Executor + ?Sized),
    sde: &Arc<Sde>,
    outbox: &mut Outbox<MoveEffect>,
    fences: &online::OnlineFences,
    item: &Item,
    hull: Option<&Item>,
    ship: i64,
    active_ship: Option<i64>,
    station: Option<i64>,
    locks: &LockedItems<'_>,
    notify: Option<&crate::space::NotifyTx>,
) -> Result<(), CallError> {
    if item.category_id == i64::from(CATEGORY_SHIP) {
        let before = banks::snapshot(tx, ship, &[]).await?;
        combat_dao::clear_banks(tx, ship).await.map_err(db_err)?;
        for charge in crate::dao::items::charges::all(tx, ship).await.map_err(db_err)? {
            combat_dao::delete_charge(tx, ship, charge.flag_id).await.map_err(db_err)?;
        }
        for stop in banks::changes(tx, ship, before).await? {
            outbox.operation(MoveEffect::Bank(stop));
        }
        insurance::uninsure(tx, ship, item.owner_id).await.map_err(db_err)?;
    } else if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) {
        let online_now = crate::dao::items::dogma::online(tx, ship).await.map_err(db_err)?
            .into_iter().any(|module| module.item_id == item.item_id);
        if online_now && hull.is_some() {
            let hull = hull.ok_or_else(refuse)?;
            online::offline_for_destruction(tx, outbox, fences, hull, item, locks, notify).await?;
        } else if online_now {
            // Only ordinary trash reaches a surviving hull; its caller supplies
            // the client's notify channel.
            let notify = notify.ok_or_else(refuse)?.clone();
            let policy = OnlinePolicy { owner: item.owner_id, active_ship, station, notify };
            online::set_online_core(tx, sde, &policy, outbox, fences, ship, item.item_id, false).await?;
        } else {
            let before = banks::snapshot(tx, ship, &[item.item_id]).await?;
            let master = targeting_charges::master_for_member(tx, ship, item.item_id).await?;
            let changed = banks::remove_member(tx, ship, item.item_id, master, false).await?;
            for stop in banks::changes(tx, ship, before).await? {
                outbox.operation(MoveEffect::Bank(stop));
            }
            if changed {
                outbox.notify_client("OnWeaponBanksChanged", vec![Value::Int(ship), crate::services::combat::banks(tx, ship).await?]);
            }
            online::queue_cleanup(outbox, fences, ship, targeting_charges::bank(tx, ship, master).await?);
        }
        combat_dao::delete_charge(tx, ship, item.flag_id).await.map_err(db_err)?;
    }
    Ok(())
}

/// Destroy the verified closure: settle the closure's reloads, run the existing
/// bank/charge/insurance/online cleanup, delete every row under the write token
/// and queue the per-item removal notifications. Surviving parent ships receive
/// one final fitting reconcile; ships inside the closure get none.
pub(in crate::services) async fn destroy(
    tx: &(impl Executor + ?Sized),
    sde: &Arc<Sde>,
    outbox: &mut Outbox<MoveEffect>,
    prepared: &Prepared,
    custody: &CustodyRule,
    audience: Option<&super::notifications::ResolvedAudience>,
    fences: &online::OnlineFences,
    actor: i64,
    verified: Verified<'_>,
) -> Result<(), CallError> {
    crate::services::combat::loaded::settle_in(tx, &verified.loaded_writes).await?;
    let Verified { locks, items, closure, owning_ship, pending, mut refund, loaded_writes: _ } = verified;
    let mut boundaries = BTreeMap::new();
    for item in &items {
        if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) {
            let ship = owning_ship[&item.item_id];
            if !closure.contains(&ship) && !boundaries.contains_key(&ship) {
                let hull = load_items(tx, sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next()
                    .ok_or_else(refuse)?;
                let before = online::fitting::capture(tx, sde, fences, hull.owner_id, ship).await?;
                boundaries.insert(ship, (hull.owner_id, before));
            }
        }
    }
    if matches!(custody, CustodyRule::Ordinary { .. }) {
        let reload_ids: Vec<_> = pending.iter().map(|row| row.reload_id).collect();
        reload_recovery::return_pending(tx, sde, outbox, &reload_ids, false, &locks, &mut refund).await?;
    }
    let mut heat = HashMap::<i64, Vec<i64>>::new();
    for item in &items {
        let ship = owning_ship.get(&item.item_id).copied().unwrap_or(item.item_id);
        if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) {
            heat.entry(ship).or_default().push(item.item_id);
        }
        if item.category_id == i64::from(CATEGORY_SHIP) || (item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id)) {
            let hull = items.iter().find(|hull| hull.item_id == ship);
            clear_deleted_runtime(tx, sde, outbox, fences, item, hull, ship, prepared.active_ship, prepared.station, &locks, prepared.notify.as_ref()).await?;
        }
    }
    if inventory_dao::delete_guarded(tx, &closure, &locks).await.map_err(db_err)? != closure.len() as u64 {
        return Err(refuse());
    }
    for (ship, modules) in heat {
        online::queue_cleanup(outbox, fences, ship, modules);
    }
    for item in items {
        let gone = Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..item.clone() };
        if let Some(audience) = audience {
            audience.item_change(outbox, &item, &gone, vec![(infinity_core::consts::IX_LOCATION_ID, Value::Int(item.location_id))], actor);
        }
        notify_item_removed(outbox, item.clone());
        if prepared.station.is_some() || !prepared.shared_ids.contains(&item.item_id) {
            notify_corp_item_removed(tx, outbox, &item).await?;
        }
    }
    for (ship, (owner, before)) in boundaries {
        let fit = online::reconcile_core(tx, sde, outbox, fences, owner, ship).await?;
        online::fitting::finish(tx, before, fit, outbox).await?;
    }
    Ok(())
}

#[cfg(all(test, feature = "tests-inventory"))]
mod tests;
