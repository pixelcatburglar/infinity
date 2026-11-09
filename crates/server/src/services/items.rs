//! Shared item transfer policy for market and combat services, not a direct client module mirror.
//! Uses game.items and game.characters through the item DAO and types from the loaded SDE.
//! Callers own transactions; non-hull item destinations are locked and revalidated
//! before deposit, while general inventory authorization remains with callers.
//! Market callers deliver corporate purchases to station Deliveries and use the returned
//! actual stack metadata; structure market destinations are unsupported.
//! `OnItemChange` quantities are projected to the client display limit; moved and stored
//! quantities stay exact.
use super::{db_err, notify_error, CallError};
use super::inventory::access::corporate_custody;
use super::inventory::notifications::{item_change_dict, notify_corp_item_change, notify_corp_item_removed};
use super::inventory::destination::{Destination, ParentGuard};
use super::inventory::hold_storage::Replaced;
use infinity_core::consts::{FLAG_HANGAR, IX_FLAG, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::dao::items::{quantities::{self, QuantityChange}, transfers};
use crate::dao::items::loading::{self, Item, ItemQuery};
use crate::db::{self, executor::Executor, outbox::Outbox};
use crate::sde::Sde;
use std::collections::BTreeMap;
use std::sync::Arc;
use marshal::Value;

pub(in crate::services) fn enrich_item(sde: &Sde, row: loading::GameItem) -> Option<Item> {
    let data = sde.types.get(row.type_id)?;
    Some(Item {
        item_id: row.item_id, type_id: row.type_id, owner_id: row.owner_id,
        location_id: row.location_id, flag_id: row.flag_id, quantity: row.quantity,
        singleton: row.singleton, group_id: i64::from(data.group_id.unwrap_or(0)),
        category_id: i64::from(data.category_id.unwrap_or(0)), custom_info: row.custom_info,
    })
}

/// Load game rows in DAO order, retaining the former type inner join.
pub(crate) async fn load_items(ex: &(impl Executor + ?Sized), sde: &Sde, query: ItemQuery<'_>) -> Result<Vec<Item>, db::Error> {
    Ok(loading::load_items(ex, query).await?.into_iter().filter_map(|row| enrich_item(sde, row)).collect())
}

pub(crate) async fn take_items<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item: Item, quantity: i64) -> Result<(), CallError> {
    let available = super::item_volume::stack_size(item.singleton, item.quantity);
    if quantity <= 0 || quantity > available || (item.singleton && quantity != 1) {
        return Err(notify_error(infinity_core::messages::INSUFFICIENT_ITEM_QUANTITY));
    }
    let corporate = corporate_custody(db, None, item.item_id).await?.is_some();
    let full = quantity == available;
    let remaining = (!full).then_some(item.quantity - quantity);
    let changes = BTreeMap::from([(item.item_id, QuantityChange::new(&item, remaining))]);
    if !quantities::apply(db, &changes).await.map_err(db_err)? {
        return Err(notify_error(infinity_core::messages::INSUFFICIENT_ITEM_QUANTITY));
    }
    if full {
        if corporate {
            notify_corp_item_removed(db, outbox, &item).await?;
        } else {
            let old = item.location_id;
            let owner = item.owner_id;
            let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
            outbox.notify_pilot(owner, "OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old))]), Value::None]);
        }
    } else {
        let old_quantity = item.quantity;
        let left = Item { quantity: remaining.unwrap(), ..item };
        let change = vec![(IX_STACK_SIZE, Value::Int(i64::from(old_quantity)))];
        if corporate {
            notify_corp_item_change(db, outbox, &left, change).await?;
        } else {
            let change = item_change_dict(change);
            outbox.notify_pilot(left.owner_id, "OnItemChange", vec![left.row(), change, Value::None]);
        }
    }
    Ok(())
}

pub(in crate::services) fn is_static_location(sde: &Sde, location: i64) -> bool {
    sde.map.station(location).is_some() || sde.map.system(location).is_some()
}

/// Destination classification and any selected existing stack captured before item locks.
pub(in crate::services) struct DepositPreparation {
    pub(in crate::services) destination: Destination,
    pub(in crate::services) type_id: i32,
    classification: DestinationClassification,
    progress: DepositProgress,
}

#[derive(Clone)]
enum DestinationClassification {
    Excluded,
    Item(Item, Option<crate::dao::items::parent_locks::Parent>),
}

enum DepositProgress {
    Captured,
    Locked { expected_quantity: i64 },
    Created(crate::dao::items::parent_locks::Parent),
}

impl DepositPreparation {
    pub(in crate::services) async fn capture(
        ex: &(impl Executor + ?Sized), sde: &Sde, destination: &Destination, type_id: i32,
    ) -> Result<Self, CallError> {
        let classification = if is_static_location(sde, destination.location) {
            DestinationClassification::Excluded
        } else {
            let observed = load_items(ex, sde, ItemQuery::Id(destination.location)).await.map_err(db_err)?
                .pop().ok_or_else(|| notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
            if observed.category_id != i64::from(infinity_core::consts::CATEGORY_SHIP)
                && super::offices::office_owner(ex, destination.location).await? == Some(destination.capacity_owner)
                && observed.owner_id == destination.capacity_owner
            {
                DestinationClassification::Excluded
            } else {
                let candidate = transfers::deposit_candidate(ex, destination.location, destination.flag, type_id, destination.item_owner).await.map_err(db_err)?;
                DestinationClassification::Item(observed, candidate)
            }
        };
        Ok(Self { destination: *destination, type_id, classification, progress: DepositProgress::Captured })
    }

    pub(in crate::services) fn fresh_attempt(&self) -> Self {
        Self { destination: self.destination, type_id: self.type_id,
            classification: self.classification.clone(), progress: DepositProgress::Captured }
    }

    fn candidate(&self) -> Option<&crate::dao::items::parent_locks::Parent> {
        match &self.classification {
            DestinationClassification::Excluded => None,
            DestinationClassification::Item(_, candidate) => candidate.as_ref(),
        }
    }

    pub(in crate::services) fn is_excluded(&self) -> bool {
        matches!(self.classification, DestinationClassification::Excluded)
    }

    pub(in crate::services) fn candidate_request(&self) -> Option<crate::dao::items::parent_locks::LockRequest> {
        self.candidate().map(|row| crate::dao::items::parent_locks::LockRequest {
            item_id: row.item_id, strength: crate::dao::items::parent_locks::LockStrength::Update,
        })
    }

    pub(in crate::services) fn candidate_id(&self) -> Option<i64> {
        self.candidate().map(|row| row.item_id)
    }
}

pub(crate) struct DepositRow {
    pub item: Item,
    pub old_quantity: Option<i64>,
    pub added_quantity: i64,
}

pub(crate) struct DepositReceipt {
    pub merged: Option<DepositRow>,
    pub created: Option<DepositRow>,
}

impl DepositReceipt {
    pub(crate) fn rows(&self) -> impl Iterator<Item = &DepositRow> {
        self.merged.iter().chain(self.created.iter())
    }

    pub(crate) fn last(&self) -> &Item {
        &self.created.as_ref().or(self.merged.as_ref()).expect("validated deposit receipt").item
    }
}

pub(crate) fn enrich_deposit(sde: &Sde, stack: transfers::DepositStack, quantity: i64) -> Result<DepositReceipt, CallError> {
    let invalid = || CallError::Internal(anyhow::anyhow!("invalid deposit receipt"));
    let convert = |row: transfers::DepositedStack| -> Result<DepositRow, CallError> {
        if !row.visible || row.added_quantity <= 0 || row.item.singleton {
            return Err(invalid());
        }
        Ok(DepositRow { item: enrich_item(sde, row.item).ok_or_else(invalid)?,
            old_quantity: row.old_quantity, added_quantity: row.added_quantity })
    };
    let merged = stack.merged.map(convert).transpose()?;
    let created = stack.created.map(convert).transpose()?;
    if quantity <= 0 || (merged.is_none() && created.is_none()) { return Err(invalid()); }
    if let Some(row) = &merged {
        if row.old_quantity.is_none_or(|old| old < 0 || old.checked_add(row.added_quantity) != Some(row.item.quantity)) {
            return Err(invalid());
        }
    }
    if let Some(row) = &created {
        if row.old_quantity.is_some() || row.item.quantity != row.added_quantity { return Err(invalid()); }
    }
    if let (Some(a), Some(b)) = (&merged, &created) {
        if a.item.item_id == b.item.item_id || a.item.quantity != infinity_core::consts::MAX_STACK_QUANTITY { return Err(invalid()); }
    }
    if merged.as_ref().map_or(0, |row| i128::from(row.added_quantity))
        + created.as_ref().map_or(0, |row| i128::from(row.added_quantity)) != i128::from(quantity) {
        return Err(invalid());
    }
    Ok(DepositReceipt { merged, created })
}

pub(in crate::services) async fn deposit_prepared<X: Send>(
    db: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>,
    preparation: &mut DepositPreparation, quantity: i64, from: i64,
    locks: Option<&crate::dao::items::parent_locks::LockedItems<'_>>, parent: Option<&ParentGuard<'_, '_>>,
    replaced: Replaced<'_>, capacity: Option<f64>,
) -> Result<DepositReceipt, CallError> {
    let receipt = deposit_statement(db, sde, outbox, preparation, quantity, from, locks, parent, replaced, capacity).await?;
    if preparation.is_excluded() { return Ok(receipt); }
    let invalid = || CallError::Internal(anyhow::anyhow!("invalid prepared deposit receipt"));
    if let Some(merged) = &receipt.merged {
        let (id, expected) = match &preparation.progress {
            DepositProgress::Created(row) => (row.item_id, row.quantity),
            progress => {
                let candidate = preparation.candidate().ok_or_else(invalid)?;
                (candidate.item_id, match progress {
                    DepositProgress::Locked { expected_quantity } => *expected_quantity,
                    _ => candidate.quantity,
                })
            }
        };
        if merged.item.item_id != id || merged.old_quantity != Some(expected) { return Err(invalid()); }
    } else if let DepositProgress::Created(row) = &preparation.progress {
        if row.quantity != infinity_core::consts::MAX_STACK_QUANTITY { return Err(invalid()); }
    }
    if let Some(created) = &receipt.created {
        let item = &created.item;
        preparation.progress = DepositProgress::Created(crate::dao::items::parent_locks::Parent {
            item_id: item.item_id, type_id: item.type_id, owner_id: item.owner_id,
            location_id: item.location_id, flag_id: item.flag_id, quantity: item.quantity,
            singleton: false, jump_clone_id: None,
        });
    } else if let Some(merged) = &receipt.merged {
        match &mut preparation.progress {
            DepositProgress::Created(row) => row.quantity = merged.item.quantity,
            progress => *progress = DepositProgress::Locked { expected_quantity: merged.item.quantity },
        }
    }
    Ok(receipt)
}

async fn deposit_statement<X: Send>(
    db: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>,
    preparation: &DepositPreparation, quantity: i64, from: i64,
    locks: Option<&crate::dao::items::parent_locks::LockedItems<'_>>, parent: Option<&ParentGuard<'_, '_>>,
    replaced: Replaced<'_>, capacity: Option<f64>,
) -> Result<DepositReceipt, CallError> {
    let Destination { capacity_owner, location, flag, .. } = preparation.destination;
    let refuse = || notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    if let Some(guard) = parent {
        let locks = locks.ok_or_else(refuse)?;
        if !guard.uses_locks(locks) || !guard.contains_parent(location) || !guard.rows().iter().any(|row| row.item_id == location && row.owner_id == capacity_owner) {
            return Err(refuse());
        }
    }
    if let DestinationClassification::Item(observed, _) = &preparation.classification {
        if super::inventory::hold_storage::is_ordinary_container(sde, observed.type_id) {
            let locks = locks.ok_or_else(refuse)?;
            let row = locks.row(location).ok_or_else(refuse)?;
            if parent.is_none() || !locks.is_write_locked(location) || row.jump_clone_id.is_some()
                || row.type_id != observed.type_id || row.owner_id != observed.owner_id
                || row.location_id != observed.location_id || row.flag_id != observed.flag_id
                || row.quantity != observed.quantity || row.singleton != observed.singleton { return Err(refuse()); }
        }
    }
    super::inventory::hold_storage::check_destination(db, sde, capacity_owner, location, flag,
        &[super::inventory::hold_storage::Addition { type_id: preparation.type_id, singleton: false, quantity: i64::from(quantity) }], replaced, capacity).await?;
    let (candidate, write_locks) = match &preparation.classification {
        DestinationClassification::Excluded => (transfers::DepositCandidate::InStatement, None),
        DestinationClassification::Item(_, selected) => {
            let locks = locks.ok_or_else(refuse)?;
            let candidate = match &preparation.progress {
                DepositProgress::Created(row) => transfers::DepositCandidate::Created(row),
                progress => {
                    let candidate = match selected.as_ref() {
                        Some(old) if !locks.has_update_intent(old.item_id) => return Err(refuse()),
                        Some(old) => match locks.row(old.item_id) {
                            Some(row) if row.type_id != old.type_id => return Err(refuse()),
                            Some(_) => Some((old, match progress {
                                DepositProgress::Locked { expected_quantity } => *expected_quantity,
                                _ => old.quantity,
                            })),
                            None => None,
                        },
                        None => None,
                    };
                    transfers::DepositCandidate::Prepared(candidate)
                }
            };
 
            (candidate, Some(locks))
        }
    };
    deposit_write(db, sde, outbox, &preparation.destination, preparation.type_id, quantity, from,
        candidate, write_locks).await
}

/// Deposit into a hull locked by admission, without capturing or point-locking a stack.
pub(in crate::services) async fn deposit_held<X: Send>(
    db: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>,
    destination: &Destination, type_id: i32, quantity: i64, from: i64,
    held: Option<&crate::dao::items::parent_locks::LockedItems<'_>>, capacity: Option<f64>,
) -> Result<DepositReceipt, CallError> {
    let refuse = || notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    let admitted = super::inventory::hold_storage::check_destination(db, sde, destination.capacity_owner,
        destination.location, destination.flag,
        &[super::inventory::hold_storage::Addition { type_id, singleton: false, quantity: i64::from(quantity) }],
        Replaced::default(), capacity).await?.ok_or_else(refuse)?;
    let locks = held.unwrap_or(&admitted);
    let hull = locks.row(destination.location).ok_or_else(refuse)?;
    if !locks.is_write_locked(destination.location) || hull.owner_id != destination.capacity_owner
        || !hull.singleton || hull.quantity != 1 || super::inventory::is_fitting_flag(destination.flag)
        || sde.types.get(hull.type_id).and_then(|ty| ty.category_id) != Some(infinity_core::consts::CATEGORY_SHIP)
    {
        return Err(refuse());
    }
    deposit_write(db, sde, outbox, destination, type_id, quantity, from, transfers::DepositCandidate::InStatement, Some(locks)).await
}

async fn deposit_write<X: Send>(
    db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, destination: &Destination,
    type_id: i32, quantity: i64, from: i64, candidate: transfers::DepositCandidate<'_>,
    locks: Option<&crate::dao::items::parent_locks::LockedItems<'_>>,
) -> Result<DepositReceipt, CallError> {
    if quantity <= 0 { return Err(notify_error(infinity_core::messages::INSUFFICIENT_ITEM_QUANTITY)); }
    let Destination { item_owner, location, flag, .. } = *destination;
    let result = transfers::deposit_stack(db, location, flag, type_id, item_owner, quantity, candidate, locks).await.map_err(db_err)?;
    let receipt = enrich_deposit(sde, result, quantity)?;
    if receipt.rows().any(|row| row.item.type_id != type_id || row.item.owner_id != item_owner
        || row.item.location_id != location || row.item.flag_id != flag) {
        return Err(CallError::Internal(anyhow::anyhow!("invalid deposit destination")));
    }
    for row in receipt.rows() {
        let change = match row.old_quantity {
            Some(old) => vec![(IX_STACK_SIZE, Value::Int(old))],
            None => vec![(IX_LOCATION_ID, Value::Int(from)), (IX_FLAG, Value::Int(i64::from(flag)))],
        };
        if corporate_custody(db, Some(sde), row.item.item_id).await?.is_some() {
            notify_corp_item_change(db, outbox, &row.item, change).await?;
        } else {
            let change = item_change_dict(change);
            outbox.notify_client("OnItemChange", vec![row.item.row(), change, Value::None]);
        }
    }
    Ok(receipt)
}
pub(super) async fn deliver<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, owner: i64, station: i64, type_id: i32, quantity: i64) -> Result<DepositReceipt, CallError> {
    if quantity <= 0 { return Err(notify_error(infinity_core::messages::INSUFFICIENT_ITEM_QUANTITY)); }
    let result = transfers::deposit_stack(db, station, FLAG_HANGAR, type_id, owner, quantity, transfers::DepositCandidate::InStatement, None).await.map_err(db_err)?;
    let receipt = enrich_deposit(sde, result, quantity)?;
    if receipt.rows().any(|row| row.item.type_id != type_id || row.item.owner_id != owner
        || row.item.location_id != station || row.item.flag_id != FLAG_HANGAR) {
        return Err(CallError::Internal(anyhow::anyhow!("invalid deposit destination")));
    }
    let docked = transfers::docked(db, owner).await.map_err(db_err)?;
    if docked == Some(station) {
        for row in receipt.rows() {
            let change = item_change_dict([row.old_quantity.map_or((IX_LOCATION_ID, Value::Int(0)), |old| (IX_STACK_SIZE, Value::Int(old)))]);
            outbox.notify_pilot(owner, "OnItemChange", vec![row.item.row(), change, Value::None]);
        }
    }
    Ok(receipt)
}
pub(crate) async fn deposit<X: Send>(db: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>, destination: &Destination, type_id: i32, quantity: i64, from: i64) -> Result<DepositReceipt, CallError> {
    deposit_with_capacity(db, sde, outbox, destination, type_id, quantity, from, Replaced::default(), None).await
}

/// Returns every physical row changed by the atomic deposit.
pub(crate) async fn deposit_with_capacity<X: Send>(db: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>, destination: &Destination, type_id: i32, quantity: i64, from: i64, replaced: Replaced<'_>, capacity: Option<f64>) -> Result<DepositReceipt, CallError> {
    let preparation = DepositPreparation::capture(db, sde, destination, type_id).await?;
    deposit_planned(db, sde, outbox, &preparation, quantity, from, replaced, capacity, &[]).await
}


pub(in crate::services) async fn deposit_planned<X: Send>(
    db: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>,
    preparation: &DepositPreparation, quantity: i64, from: i64,
    replaced: Replaced<'_>, capacity: Option<f64>, extra_requests: &[crate::dao::items::parent_locks::LockRequest],
) -> Result<DepositReceipt, CallError> {
    use crate::dao::items::parent_locks::LockStrength;
    use super::inventory::destination::{lock_items, ParentSnapshot};
    let refuse = || notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    let destination = preparation.destination;
    let observed = match &preparation.classification {
        DestinationClassification::Excluded => return deposit_statement(db, sde, outbox, preparation, quantity, from, None, None, replaced, capacity).await,
        DestinationClassification::Item(observed, _) => observed,
    };
    let ancestry = crate::dao::items::deliveries::ancestry(db, destination.location).await.map_err(db_err)?;
    let mut requests = super::inventory::destination::parent_requests(sde,
        ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?;
    if super::inventory::hold_storage::is_ordinary_container(sde, observed.type_id) {
        requests.push(crate::dao::items::parent_locks::LockRequest { item_id: destination.location, strength: LockStrength::Update });
    }
    requests.extend_from_slice(extra_requests);
    let optional: Vec<_> = preparation.candidate_id().filter(|id| !requests.iter().any(|request| request.item_id == *id)).into_iter().collect();
    if let Some(request) = preparation.candidate_request() { requests.push(request); }
    let locks = lock_items(db, sde, &requests, &optional).await?;
    if crate::dao::items::deliveries::ancestry(db, destination.location).await.map_err(db_err)? != ancestry { return Err(refuse()); }
    let current = locks.row(destination.location).ok_or_else(refuse)?;
    if current.jump_clone_id.is_some() || current.type_id != observed.type_id || current.owner_id != observed.owner_id
        || current.location_id != observed.location_id || current.flag_id != observed.flag_id
        || current.quantity != observed.quantity || current.singleton != observed.singleton { return Err(refuse()); }
    let snapshot = (observed.category_id != i64::from(infinity_core::consts::CATEGORY_SHIP))
        .then(|| ParentSnapshot { item: observed.clone(), ancestry });
    let guard = snapshot.as_ref().map(|snapshot| ParentGuard::revalidate(&locks, std::slice::from_ref(snapshot))).transpose()?;
    deposit_statement(db, sde, outbox, preparation, quantity, from, Some(&locks), guard.as_ref(), replaced, capacity).await
}

#[cfg(test)] mod tests;
