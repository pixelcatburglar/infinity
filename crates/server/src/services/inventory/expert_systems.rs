//! Trust boundary for consuming expert-system inventory items.
//!
//! The client mirror is the inventory `OnItemChange` stream; authoritative
//! item data and quantities come from `game.items` and the SDE.  This module
//! is not a client service mirror and does not support transport-owned
//! transactions or World reads.
//!
//! Callers capture reachability before opening their transaction, then invoke
//! `validate_and_consume` inside that transaction.  This keeps the World read
//! outside the transaction while the item validation, guarded mutation, and
//! client outbox entry remain atomic with the caller's other work.

use std::collections::BTreeMap;

use marshal::Value;

use crate::consts::IX_STACK_SIZE;
use crate::dao::items::{loading::{Item, ItemQuery}, quantities::{self, QuantityChange}};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::messages;
use crate::sde::Sde;
use crate::services::{db_err, identity::Identity, notify_error};

use super::{notify_item_change, notify_item_removed};
use super::reachability::{Endpoint, Reachability};

/// Validate and consume one expert-system item in the caller's transaction.
///
/// `reachability` must have been captured before the transaction from the same
/// identity and the item's observed location/flag.  The returned item is the
/// pre-consumption snapshot; callers use its type and other fields to apply
/// their own lifecycle changes in the same transaction.
pub(in crate::services) async fn validate_and_consume<X: Send>(
    ex: &(impl Executor + ?Sized),
    identity: &Identity,
    sde: &Sde,
    reachability: &Reachability,
    item_id: i64,
    outbox: &mut Outbox<X>,
) -> Result<Item, crate::services::CallError> {
    let item = crate::services::items::load_items(ex, sde, ItemQuery::Id(item_id))
        .await
        .map_err(db_err)?
        .into_iter()
        .next()
        .ok_or_else(|| notify_error(messages::expert_systems::ITEM_NOT_FOUND))?;
    if identity.char_id == 0 || item.quantity < 1 {
        return Err(notify_error(messages::expert_systems::ITEM_NOT_OWNED));
    }
    if sde.expert_systems.get(item.type_id).is_none() {
        return Err(notify_error(messages::expert_systems::INVALID));
    }

    let endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
    reachability.check(ex, sde, endpoint).await?;
    if let Some(allowed) = super::custody_allowed(ex, sde, item.item_id, identity.char_id, identity.station_id, true).await? {
        if !allowed {
            return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE));
        }
    } else if item.owner_id != identity.char_id {
        return Err(notify_error(messages::expert_systems::ITEM_NOT_OWNED));
    }
    if !crate::dao::trades::reserved(ex, &[item.item_id]).await.map_err(db_err)?.is_empty() {
        return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
    }

    let change = QuantityChange::new(&item, (item.quantity > 1).then_some(item.quantity - 1));
    if !quantities::apply(ex, &BTreeMap::from([(item.item_id, change)])).await.map_err(db_err)? {
        return Err(notify_error(messages::expert_systems::CHANGED_RETRY));
    }
    if item.quantity > 1 {
        // The change value is the old stack size, as required by OnItemChange.
        notify_item_change(ex, sde, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))]).await?;
    } else {
        notify_item_removed(outbox, item.clone());
    }
    Ok(item)
}

#[cfg(test)]
mod tests;

