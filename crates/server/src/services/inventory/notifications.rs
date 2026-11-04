//! `OnItemChange` delivery for `eve/client/script/environment/invCache.py`.
//! Reads `game.items` and corporate custody and role records to queue changes;
//! foreign shared-bay permissions are not modelled.

use super::access::{corp_hangar_role, corporate_custody, may_view_corp_hangar};
use crate::consts::{IX_LOCATION_ID, LOCATION_JUNKYARD};
use crate::dao::items::loading::{Item, ItemQuery};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::sde::Sde;
use crate::services::{db_err, items::load_items, CallError};
use marshal::Value;

/// `OnItemChange(item, change, location)` for `item_id` as it is now in the database.
pub(in crate::services) async fn notify_item_change<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, item_id: i64, change: Vec<(i64, Value)>) -> Result<(), CallError> {
    if let Some(item) = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next() {
        let change = Value::Dict(change.into_iter().map(|(k, v)| (Value::Int(k), v)).collect());
        outbox.notify_client("OnItemChange", vec![item.row(), change, Value::None]);
    }
    Ok(())
}

/// Queue a provided corporate row to members authorized to view its division.
pub(in crate::services) async fn notify_corp_item_change<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item: &Item, change: Vec<(i64, Value)>) -> Result<(), CallError> {
    let parent = if item.location_id == LOCATION_JUNKYARD {
        change.iter().find_map(|(key, value)| (*key == IX_LOCATION_ID).then(|| value.as_int()).flatten()).unwrap_or(item.location_id)
    } else { item.location_id };
    let custody = if corp_hangar_role(item.flag_id, false).is_some() {
        Some((item.owner_id, parent, item.flag_id))
    } else {
        corporate_custody(ex, None, parent).await?
    };
    let Some((corp, _, flag)) = custody else { return Ok(()) };
    let change = Value::Dict(change.into_iter().map(|(k, v)| (Value::Int(k), v)).collect());
    for char_id in crate::dao::characters::members::active_ids(ex, corp).await.map_err(db_err)? {
        if may_view_corp_hangar(ex, char_id, corp, flag).await? {
            outbox.notify_pilot(char_id, "OnItemChange", vec![item.row(), change.clone(), Value::None]);
        }
    }
    Ok(())
}

/// Queue the standard junkyard notification for a removed item.
pub(in crate::services) fn notify_item_removed<X: Send>(outbox: &mut Outbox<X>, item: Item) {
    let old_location = item.location_id;
    let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
    let change = Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old_location))]);
    outbox.notify_client("OnItemChange", vec![gone.row(), change, Value::None]);
}

/// Queue deletion to corporate viewers using the item's captured pre-delete location.
pub(in crate::services) async fn notify_corp_item_removed<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item: &Item,
) -> Result<(), CallError> {
    let old = item.location_id;
    let gone = Item { location_id: LOCATION_JUNKYARD, ..item.clone() };
    notify_corp_item_change(ex, outbox, &gone, vec![(IX_LOCATION_ID, Value::Int(old))]).await
}
