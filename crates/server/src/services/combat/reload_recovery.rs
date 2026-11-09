//! Return interrupted ammunition reload reservations during startup, repackaging,
//! or before shared-subtree custody changes.
//!
//! Mirrors reload handling in `eve/client/script/dogma/clientDogmaLocation.py`;
//! reads `game.pending_reloads` and `game.items`. Reload timers remain in memory and
//! are not resumed: surviving ships get charges back in the owned source hold or ship cargo.
//! A missing ship has lost its in-flight charges, so recovery deletes the reservation only.
//! Reload reservations belong to the taker once ammunition is reserved.

use std::{collections::BTreeMap, sync::Arc};

use infinity_core::consts::{FLAG_CARGO, FLAG_HANGAR};
use crate::dao::items::{loading::{self, ItemQuery}, pending_reloads, parent_locks::{LockRequest, LockStrength, LockedItems}};
use crate::db::{Db, executor::Executor, outbox::Outbox};
use infinity_core::messages;
use crate::sde::Sde;
use crate::services::{CallError, db_err, items::{deposit_prepared, DepositPreparation}, notify_error};

/// Untrusted pretransaction take inputs; authority is established separately.
pub(in crate::services) struct TakePreparation {
    root: i64,
    closure: Vec<i64>,
    pending: Vec<pending_reloads::PendingReload>,
    endpoints: Vec<crate::services::inventory::reachability::Endpoint>,
    lineages: BTreeMap<i64, Vec<crate::dao::items::deliveries::Ancestor>>,
    ancestors: Vec<i64>,
    refund: RefundPreparation,
}

impl TakePreparation {
    pub(in crate::services) fn fresh_attempt(&self) -> Self {
        Self { root: self.root, closure: self.closure.clone(), pending: self.pending.clone(),
            endpoints: self.endpoints.clone(), lineages: self.lineages.clone(),
            ancestors: self.ancestors.clone(), refund: self.refund.fresh_attempt() }
    }

    pub(in crate::services) async fn capture(db: &(impl Executor + ?Sized), sde: &Sde, root: i64) -> Result<Self, CallError> {
        use crate::services::inventory::reachability::Endpoint;
        let closure = crate::dao::items::inventory::descendants(db, &[root]).await.map_err(db_err)?;
        let pending = pending_reloads::intersecting(db, &closure).await.map_err(db_err)?;
        let mut endpoints = Vec::new();
        for row in &pending {
            endpoints.push(Endpoint { location: row.source_location_id, flag: row.source_flag });
            endpoints.push(Endpoint { location: row.source_location_id, flag: FLAG_HANGAR });
            endpoints.push(Endpoint { location: row.ship_id, flag: FLAG_CARGO });
        }
        let mut locations = std::collections::BTreeSet::from([root]);
        for row in &pending { locations.extend([row.ship_id, row.source_location_id]); }
        let mut lineages = BTreeMap::new();
        let mut ancestors = std::collections::BTreeSet::new();
        for location in locations {
            let rows = crate::dao::items::deliveries::ancestry(db, location).await.map_err(db_err)?;
            ancestors.extend(rows.iter().map(|row| row.item_id));
            lineages.insert(location, rows);
        }
        let refund = RefundPreparation::capture_pending(db, sde, &pending).await?;
        Ok(Self { root, closure, pending, endpoints, lineages, ancestors: ancestors.into_iter().collect(), refund })
    }

    pub(in crate::services) fn endpoints(&self) -> &[crate::services::inventory::reachability::Endpoint] { &self.endpoints }
    pub(in crate::services) fn closure(&self) -> &[i64] { &self.closure }
    pub(in crate::services) fn pending(&self) -> &[pending_reloads::PendingReload] { &self.pending }
    pub(in crate::services) fn ancestors(&self) -> &[i64] { &self.ancestors }
    pub(in crate::services) fn refund(&self) -> &RefundPreparation { &self.refund }

    /// Recheck all captured reservation facts and the closure after acquiring locks.
    pub(in crate::services) async fn revalidate(&self, ex: &(impl Executor + ?Sized)) -> Result<(), CallError> {
        if crate::dao::items::inventory::descendants(ex, &[self.root]).await.map_err(db_err)?.as_slice() != self.closure()
            || pending_reloads::intersecting(ex, self.closure()).await.map_err(db_err)?.as_slice() != self.pending() {
            return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
        for (&location, captured) in &self.lineages {
            if crate::dao::items::deliveries::ancestry(ex, location).await.map_err(db_err)? != *captured {
                return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
            }
        }
        Ok(())
    }
}

async fn refund_destination(tx: &(impl Executor + ?Sized), sde: &Sde, row: &pending_reloads::PendingReload) -> Result<(i64, i32, bool), CallError> {
    if sde.map.station(row.source_location_id).is_some() {
        return Ok((row.source_location_id, FLAG_HANGAR, false));
    }
    if let Some(item) = loading::load_items(tx, ItemQuery::Id(row.source_location_id)).await.map_err(db_err)?
        .into_iter().find(|item| item.owner_id == row.owner_id) {
        let non_hull = sde.types.get(item.type_id).is_some_and(|ty| ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP));
        return Ok((row.source_location_id, row.source_flag, non_hull));
    }
    Ok((row.ship_id, FLAG_CARGO, false))
}

pub(in crate::services) struct RefundPreparation {
    pending: Vec<pending_reloads::PendingReload>,
    destinations: BTreeMap<i64, (i64, i32)>,
    deposits: BTreeMap<(i64, i64, i32, i32), DepositPreparation>,
    parents: BTreeMap<i64, crate::services::inventory::destination::ParentSnapshot>,
    sources: BTreeMap<i64, Option<crate::dao::items::loading::Item>>,
    requests: Vec<LockRequest>,
}

impl RefundPreparation {
    fn fresh_attempt(&self) -> Self {
        Self { pending: self.pending.clone(), destinations: self.destinations.clone(),
            deposits: self.deposits.iter().map(|(key, prep)| (*key, prep.fresh_attempt())).collect(),
            parents: self.parents.clone(), sources: self.sources.clone(), requests: self.requests.clone() }
    }

    pub(in crate::services) async fn discard(ex: &(impl Executor + ?Sized), sde: &Sde, pending: &[pending_reloads::PendingReload]) -> Result<Self, CallError> {
        Self::capture(ex, sde, pending, false).await
    }

    pub(in crate::services) async fn capture_pending(ex: &(impl Executor + ?Sized), sde: &Sde, pending: &[pending_reloads::PendingReload]) -> Result<Self, CallError> {
        Self::capture(ex, sde, pending, true).await
    }

    async fn capture(ex: &(impl Executor + ?Sized), sde: &Sde, pending: &[pending_reloads::PendingReload], deposit: bool) -> Result<Self, CallError> {
        let mut deposits = BTreeMap::new();
        let mut destinations = BTreeMap::new();
        let mut parents = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut requests = Vec::new();
        for row in pending {
            if !crate::services::items::is_static_location(sde, row.source_location_id) && !sources.contains_key(&row.source_location_id) {
                let item = crate::services::items::load_items(ex, sde, ItemQuery::Id(row.source_location_id)).await.map_err(db_err)?.into_iter().next();
                if let Some(source) = &item {
                    if crate::services::inventory::hold_storage::is_ordinary_container(sde, source.type_id) {
                        let ancestry = crate::dao::items::deliveries::ancestry(ex, source.item_id).await.map_err(db_err)?;
                        requests.extend(crate::services::inventory::destination::parent_requests(sde,
                            ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?);
                        requests.push(LockRequest { item_id: source.item_id, strength: LockStrength::Update });
                        parents.insert(source.item_id, crate::services::inventory::destination::ParentSnapshot { item: source.clone(), ancestry });
                    }
                }
                sources.insert(row.source_location_id, item);
            }
            if !deposit { continue; }
            let (location, flag, non_hull) = refund_destination(ex, sde, row).await?;
            destinations.insert(row.reload_id, (location, flag));
            let destination = crate::services::inventory::destination::Destination::owned(row.owner_id, location, flag);
            let key = (row.owner_id, location, flag, row.type_id);
            if !deposits.contains_key(&key) {
                let prep = DepositPreparation::capture(ex, sde, &destination, row.type_id).await?;
                if let Some(request) = prep.candidate_request() { requests.push(request); }
                deposits.insert(key, prep);
            }
            if non_hull && !deposits[&key].is_excluded() && !parents.contains_key(&location) {
                if let Some(item) = crate::services::items::load_items(ex, sde, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next() {
                    let ancestry = crate::dao::items::deliveries::ancestry(ex, location).await.map_err(db_err)?;
                    requests.extend(crate::services::inventory::destination::parent_requests(sde,
                        ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?);
                    if crate::services::inventory::hold_storage::is_ordinary_container(sde, item.type_id) {
                        requests.push(LockRequest { item_id: location, strength: LockStrength::Update });
                    }
                    parents.insert(location, crate::services::inventory::destination::ParentSnapshot { item, ancestry });
                }
            }
        }
        Ok(Self { pending: pending.to_vec(), deposits, destinations, parents, sources, requests })
    }

    pub(in crate::services) fn requests(&self) -> &[LockRequest] { &self.requests }
    pub(in crate::services) fn candidate_ids(&self) -> Vec<i64> {
        self.deposits.values().filter_map(|prep| prep.candidate_request().map(|request| request.item_id)).collect()
    }
    pub(in crate::services) fn mandatory_ids(&self) -> Vec<i64> {
        self.parents.values().flat_map(|snapshot| snapshot.ancestry.iter().map(|row| row.item_id)).collect()
    }

    pub(in crate::services) async fn revalidate(&self, ex: &(impl Executor + ?Sized), sde: &Sde, locks: &LockedItems<'_>) -> Result<(), CallError> {
        let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
        for (&id, captured) in &self.sources {
            let current = crate::services::items::load_items(ex, sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next();
            if current.as_ref().map(|item| (item.item_id, item.type_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton))
                != captured.as_ref().map(|item| (item.item_id, item.type_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton)) { return Err(refuse()); }
        }
        for row in &self.pending {
            if let Some(expected) = self.destinations.get(&row.reload_id) {
                let (location, flag, _) = refund_destination(ex, sde, row).await?;
                if *expected != (location, flag) { return Err(refuse()); }
            }
        }
        for snapshot in self.parents.values() {
            if crate::dao::items::deliveries::ancestry(ex, snapshot.item.item_id).await.map_err(db_err)? != snapshot.ancestry {
                return Err(refuse());
            }
        }
        let snapshots: Vec<_> = self.parents.values().cloned().collect();
        crate::services::inventory::destination::ParentGuard::revalidate(locks, &snapshots)?;
        Ok(())
    }
}

/// The caller supplies the complete held item set and captured refund destinations.
pub(in crate::services) async fn return_pending<X: Send>(tx: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>, reload_ids: &[i64], ship_exists: bool, locks: &LockedItems<'_>, refund: &mut RefundPreparation) -> Result<Vec<crate::services::items::DepositReceipt>, CallError> {
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    refund.revalidate(tx, sde, locks).await?;
    let mut ids = reload_ids.to_vec();
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) { return Err(refuse()); }
    let rows = pending_reloads::lock_ids(tx, &ids).await.map_err(db_err)?;
    return_locked(tx, sde, outbox, rows, ship_exists, locks, refund).await
}

async fn return_locked<X: Send>(tx: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>, rows: Vec<pending_reloads::PendingReload>, ship_exists: bool, locks: &LockedItems<'_>, refund: &mut RefundPreparation) -> Result<Vec<crate::services::items::DepositReceipt>, CallError> {
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    if rows.iter().any(|row| !refund.pending.contains(row)) { return Err(refuse()); }
    let mut groups = BTreeMap::<(i64, i64, i64, i32, i32), Vec<pending_reloads::PendingReload>>::new();
    for row in rows {
        if ship_exists {
            let (location, flag, _) = refund_destination(tx, sde, &row).await?;
            groups.entry((row.owner_id, row.ship_id, location, flag, row.type_id)).or_default().push(row);
        } else if pending_reloads::take(tx, row.reload_id).await.map_err(db_err)?.as_ref() != Some(&row) {
            return Err(refuse());
        }
    }
    let mut receipts = Vec::new();
    let snapshots: Vec<_> = refund.parents.values().cloned().collect();
    let guard = crate::services::inventory::destination::ParentGuard::revalidate(locks, &snapshots)?;
    for ((owner, ship, location, flag, type_id), rows) in groups {
        let total = rows.iter().try_fold(0_i128, |sum, row| sum.checked_add(i128::from(row.quantity)))
            .ok_or_else(|| notify_error(messages::INSUFFICIENT_AMMUNITION))?;
        let quantity = i64::try_from(total).ok().filter(|quantity| *quantity > 0)
            .ok_or_else(|| notify_error(messages::INSUFFICIENT_AMMUNITION))?;
        let group_ids: Vec<_> = rows.iter().filter(|row| {
            refund.sources.get(&location).and_then(Option::as_ref).is_some_and(|source|
                source.owner_id == row.owner_id && row.source_location_id == location && (crate::services::inventory::hold_storage::is_ordinary_container(sde, source.type_id)
                    || (source.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP) && row.source_flag == flag)))
        }).map(|row| row.reload_id).collect();
        let prep = refund.deposits.get_mut(&(owner, location, flag, type_id)).ok_or_else(refuse)?;
        let parent = guard.contains_parent(location).then_some(&guard);
        let replaced = crate::services::inventory::hold_storage::Replaced { items: &[], reloads: &group_ids };
        receipts.push(deposit_prepared(tx, sde, outbox, prep, quantity, ship, Some(locks), parent, replaced, None).await?);
        for row in rows {
            if pending_reloads::take(tx, row.reload_id).await.map_err(db_err)?.as_ref() != Some(&row) { return Err(refuse()); }
        }
    }
    Ok(receipts)
}

/// Settle reservations touching a subtree while its previous owners still hold it.
/// Call inside the take transaction after locking and revalidating its prepared
/// closure, endpoint ancestors and authorization. A supplied audience must have
/// been resolved from the same transaction after the pretransaction capture.
/// Refund direct-client notices are routed to their original reservation owner.
pub(in crate::services) async fn settle_subtree<X: Send>(
    tx: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>, root: i64,
    _destination: i64, actor: i64, audience: Option<&crate::services::inventory::notifications::ResolvedAudience>,
    locks: &LockedItems<'_>, take: &mut TakePreparation,
) -> Result<Vec<crate::services::items::DepositReceipt>, CallError> {
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    if root != take.root { return Err(refuse()); }
    take.revalidate(tx).await?;
    take.refund.revalidate(tx, sde, locks).await?;
    if take.closure.iter().any(|id| !locks.is_write_locked(*id))
        || take.ancestors.iter().any(|id| !locks.is_write_locked(*id)) { return Err(refuse()); }
    let ids: Vec<_> = take.pending.iter().map(|row| row.reload_id).collect();
    let pending = pending_reloads::lock_ids(tx, &ids).await.map_err(db_err)?;
    if pending.iter().any(|row| !take.pending.contains(row)) { return Err(refuse()); }
    let mut groups = BTreeMap::<i64, Vec<pending_reloads::PendingReload>>::new();
    for row in pending { groups.entry(row.owner_id).or_default().push(row); }
    let mut all_receipts = Vec::new();
    for (owner, rows) in groups {
        let mut child = Outbox::default();
        let receipts = return_locked(tx, sde, &mut child, rows, true, locks, &mut take.refund).await?;
        outbox.append_routed(child, |notification| crate::services::inventory::notifications::route_target(notification, owner, Some(actor)));
        if let Some(audience) = audience {
            for row in receipts.iter().flat_map(|receipt| receipt.rows()) {
                let new = &row.item;
                let (old, change) = match row.old_quantity {
                    Some(quantity) => (crate::dao::items::loading::Item { quantity, ..new.clone() }, vec![(infinity_core::consts::IX_STACK_SIZE, marshal::Value::Int(quantity))]),
                    None => (crate::dao::items::loading::Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..new.clone() },
                        vec![(infinity_core::consts::IX_LOCATION_ID, marshal::Value::Int(infinity_core::consts::LOCATION_JUNKYARD))]),
                };
                audience.item_change(outbox, &old, new, change, owner);
            }
        }
        all_receipts.extend(receipts);
    }
    Ok(all_receipts)
}

pub(crate) async fn recover_pending_reloads(db: &Db, sde: &Arc<Sde>) -> Result<(), CallError> {
    let _lock = crate::services::market::lock().await;
    for row in pending_reloads::all(db).await.map_err(db_err)? {
        let (reload_id, ship_id, source_location) = (row.reload_id, row.ship_id, row.source_location_id);
        let result = db.transaction(
            |tx, outbox: &mut Outbox| {
                let sde = Arc::clone(sde);
                let row = row.clone();
                Box::pin(async move {
                    let captured = crate::dao::items::parent_locks::capture(tx, &[ship_id, source_location]).await.map_err(db_err)?;
                    let exists = captured.iter().any(|item| item.item_id == ship_id);
                    let mut refund = if exists {
                        RefundPreparation::capture_pending(tx, &sde, std::slice::from_ref(&row)).await?
                    } else { RefundPreparation::discard(tx, &sde, std::slice::from_ref(&row)).await? };
                    let mut requests: Vec<_> = captured.iter().map(|item| LockRequest { item_id: item.item_id, strength: LockStrength::Update }).collect();
                    requests.extend_from_slice(refund.requests());
                    let optional: Vec<_> = if exists { refund.candidate_ids().into_iter().filter(|id|
                        !captured.iter().any(|row| row.item_id == *id)
                            && !refund.parents.values().any(|snapshot| snapshot.ancestry.iter().any(|row| row.item_id == *id))
                    ).collect() } else { Vec::new() };
                    let locks = crate::services::inventory::destination::lock_items(tx, &sde, &requests, &optional).await?;
                    return_pending(tx, &sde, outbox, &[reload_id], exists, &locks, &mut refund).await.map(|_| ())
                })
            },
            |(), _outbox| async move { Ok(()) },
        ).await;
        match result {
            Ok(()) => {}
            Err(CallError::User { msg, .. }) => tracing::warn!(reload_id, ship_id, %msg, "reload recovery refused; reservation retained"),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)] mod tests;
