//! Contract item custody for contractProxy, mirroring
//! `eve/client/script/ui/shared/neocom/contracts/contracts.py` and contract details.
//! Uses `game.contracts` and `game.items` through shared inventory and transfer APIs.
//! Corporate receipts use station Deliveries; as a server policy, requested corporate
//! exchange items also come from Deliveries. A lost source office falls back to Deliveries.
//! Transactions and courier package authorization remain with lifecycle callers; structures
//! are not supported.

use infinity_core::consts::{FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, FLAG_HANGAR, IX_FLAG, IX_LOCATION_ID, IX_OWNER_ID, LOCATION_JUNKYARD};
use crate::dao::{contracts::Contract, items::{inventory as item_inventory, loading::{Item, ItemQuery}, quantities::{self, QuantityChange}, transfers}};
use crate::db::{executor::Executor, outbox::Outbox};
use infinity_core::messages;
use crate::sde::Sde;
use crate::services::{corps, db_err, inventory, items::load_items, notify_error, offices, CallError};
use marshal::Value;
use std::collections::BTreeMap;

pub(super) struct Preparation<'tx> {
    pub locks: crate::dao::items::parent_locks::LockedItems<'tx>,
    pub closure: Vec<i64>,
}

pub(super) async fn prepare<'tx>(
    ex: &'tx (impl Executor + ?Sized), sde: &Sde, roots: &[i64], extra: &[i64],
) -> Result<Preparation<'tx>, CallError> {
    use crate::dao::items::{deliveries, parent_locks::{self, LockRequest, LockStrength}};
    let closure = item_inventory::descendants(ex, roots).await.map_err(db_err)?;
    let mut writes = closure.clone();
    writes.extend_from_slice(extra);
    writes.sort_unstable();
    writes.dedup();
    let captured = parent_locks::capture(ex, &writes).await.map_err(db_err)?;
    let lineage = deliveries::ancestry_many(ex, &writes).await.map_err(db_err)?;
    let mut requests: Vec<_> = writes.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
    requests.extend(lineage.iter().filter(|row| writes.binary_search(&row.ancestor.item_id).is_err()).map(|row| LockRequest {
        item_id: row.ancestor.item_id,
        strength: if sde.types.get(row.ancestor.type_id).is_some_and(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)) {
            LockStrength::Update
        } else { LockStrength::Share },
    }));
    let locks = inventory::destination::lock_items(ex, sde, &requests, &[]).await?;
    let stale = || notify_error(messages::CONTRACT_ITEM_MISSING);
    if roots.iter().any(|id| closure.binary_search(id).is_err()) || captured.len() != writes.len()
        || captured.iter().any(|old| !locks.row(old.item_id).is_some_and(|row|
            row.type_id == old.type_id && row.owner_id == old.owner_id && row.location_id == old.location_id
            && row.flag_id == old.flag_id && row.quantity == old.quantity && row.singleton == old.singleton
            && row.jump_clone_id == old.jump_clone_id))
        || item_inventory::descendants(ex, roots).await.map_err(db_err)? != closure
        || deliveries::ancestry_many(ex, &writes).await.map_err(db_err)? != lineage {
        return Err(stale());
    }
    Ok(Preparation { locks, closure })
}

#[derive(Clone, Copy)]
pub(super) struct Custody { pub owner: i64, pub location: i64, pub flag: i32 }

pub(super) async fn issuer_source(ex: &(impl Executor + ?Sized), actor: i64, corp: i64, for_corp: bool, station: i64, requested_flag: i32) -> Result<Custody, CallError> {
    if !for_corp { return Ok(Custody { owner: actor, location: station, flag: FLAG_HANGAR }); }
    let flag = if requested_flag == FLAG_HANGAR { FLAG_CORP_SAG1 } else { requested_flag };
    if !corps::is_player_corp(corp) || !inventory::access::may_take_corp_hangar(ex, actor, corp, station, flag).await? {
        return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE));
    }
    let location = if flag == FLAG_CORP_DELIVERIES { station } else {
        offices::office_at(ex, corp, station).await?.ok_or_else(|| notify_error(messages::NOT_YOUR_CORPORATION_HANGAR))?
    };
    Ok(Custody { owner: corp, location, flag })
}

pub(super) fn receiving(owner: i64, station: i64) -> Custody {
    Custody { owner, location: station, flag: if corps::is_player_corp(owner) { FLAG_CORP_DELIVERIES } else { FLAG_HANGAR } }
}

pub(super) async fn issuer_return(ex: &(impl Executor + ?Sized), contract: &Contract) -> Result<Custody, CallError> {
    if !contract.for_corp { return Ok(receiving(contract.issuer, contract.start)); }
    if contract.issuer_hangar_flag == FLAG_CORP_DELIVERIES || offices::office_owner(ex, contract.issuer_source_location).await? == Some(contract.issuer_corp) {
        return Ok(Custody { owner: contract.issuer_corp, location: contract.issuer_source_location, flag: contract.issuer_hangar_flag });
    }
    Ok(receiving(contract.issuer_corp, contract.start))
}

pub(super) async fn hand_over<X: Send>(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, roots: &[i64], destination: Custody, old_location: i64, preparation: &Preparation<'_>, created: &[i64]) -> Result<(), CallError> {
    let closure = item_inventory::descendants(ex, roots).await.map_err(db_err)?;
    if roots.iter().any(|id| closure.binary_search(id).is_err())
        || created.iter().any(|id| !roots.contains(id))
        || closure.iter().any(|id| preparation.closure.binary_search(id).is_err() && !created.contains(id)) {
        return Err(notify_error(messages::CONTRACT_ITEM_MISSING));
    }
    let old = load_items(ex, sde, ItemQuery::Ids(roots)).await.map_err(db_err)?;
    if old.len() != roots.len() || old.iter().any(|item| item.location_id != old_location) { return Err(notify_error(messages::CONTRACT_ITEM_MISSING)); }
    for item in &old {
        if corps::is_player_corp(item.owner_id) && item.location_id >= 0 {
            let gone = Item { location_id: LOCATION_JUNKYARD, ..item.clone() };
            inventory::notifications::notify_corp_item_change(ex, outbox, &gone, vec![(IX_LOCATION_ID, Value::Int(item.location_id))]).await?;
        }
        if !item_inventory::move_item(ex, item, destination.location, destination.flag, destination.owner, item.singleton).await.map_err(db_err)? {
            return Err(notify_error(messages::CONTRACT_ITEM_MISSING));
        }
    }
    let plan: Vec<_> = old.iter().map(|item| transfers::Transfer { item_id: item.item_id, owner: destination.owner, location: destination.location, flag: destination.flag }).collect();
    transfers::hand_over(ex, &plan, &closure, &preparation.locks, created).await.map_err(db_err)?;
    let docked = if corps::is_player_corp(destination.owner) { false } else { transfers::docked(ex, destination.owner).await.map_err(db_err)? == Some(destination.location) };
    for item in load_items(ex, sde, ItemQuery::Ids(roots)).await.map_err(db_err)? {
        let previous = old.iter().find(|old| old.item_id == item.item_id).ok_or_else(|| notify_error(messages::CONTRACT_ITEM_MISSING))?;
        let change = vec![(IX_LOCATION_ID, Value::Int(previous.location_id)), (IX_OWNER_ID, Value::Int(previous.owner_id)), (IX_FLAG, Value::Int(i64::from(previous.flag_id)))];
        if !corps::is_player_corp(previous.owner_id) && (previous.owner_id != destination.owner || !docked) && previous.location_id >= 0 {
            outbox.notify_pilot(previous.owner_id, "OnItemChange", vec![item.row(), Value::Dict(change.iter().map(|(k, v)| (Value::Int(*k), v.clone())).collect()), Value::None]);
        }
        if corps::is_player_corp(destination.owner) {
            inventory::notifications::notify_corp_item_change(ex, outbox, &item, change).await?;
        } else if docked {
            outbox.notify_pilot(destination.owner, "OnItemChange", vec![item.row(), Value::Dict(change.into_iter().map(|(k, v)| (Value::Int(k), v)).collect()), Value::None]);
        }
    }
    Ok(())
}

pub(super) async fn remove_package<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item: &Item) -> Result<(), CallError> {
    let gone = Item { location_id: LOCATION_JUNKYARD, ..item.clone() };
    let change = vec![(IX_LOCATION_ID, Value::Int(item.location_id))];
    if corps::is_player_corp(item.owner_id) {
        inventory::notifications::notify_corp_item_change(ex, outbox, &gone, change).await?;
    } else {
        outbox.notify_pilot(item.owner_id, "OnItemChange", vec![gone.row(), Value::Dict(change.into_iter().map(|(k, v)| (Value::Int(k), v)).collect()), Value::None]);
    }
    let changes = BTreeMap::from([(item.item_id, QuantityChange::new(item, None))]);
    if !quantities::apply(ex, &changes).await.map_err(db_err)? { return Err(notify_error(messages::CONTRACT_ITEM_MISSING)); }
    Ok(())
}

#[cfg(test)]
mod tests;
