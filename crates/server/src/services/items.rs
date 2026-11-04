//! Shared item transfer policy for market and combat services, not a direct client module mirror.
//! Uses game.items and game.characters through the item DAO and types from the loaded SDE.
//! Callers own transactions; this helper does not implement general inventory policy.
use super::{db_err, notify_error, CallError};
use super::inventory::notifications::notify_item_change;
use crate::consts::{FLAG_HANGAR, IX_FLAG, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::dao::items::{inventory, transfers};
use crate::dao::items::loading::{self, Item, ItemQuery};
use crate::db::{self, executor::Executor, outbox::Outbox};
use crate::sde::Sde;
use marshal::Value;

/// Load game rows in DAO order, retaining the former type inner join.
pub(crate) async fn load_items(ex: &(impl Executor + ?Sized), sde: &Sde, query: ItemQuery<'_>) -> Result<Vec<Item>, db::Error> {
    Ok(loading::load_items(ex, query).await?.into_iter().filter_map(|row| {
        let data = sde.types.get(row.type_id)?;
        Some(Item {
            item_id: row.item_id, type_id: row.type_id, owner_id: row.owner_id,
            location_id: row.location_id, flag_id: row.flag_id, quantity: row.quantity,
            singleton: row.singleton, group_id: i64::from(data.group_id.unwrap_or(0)),
            category_id: i64::from(data.category_id.unwrap_or(0)), custom_info: row.custom_info,
        })
    }).collect())
}

pub(crate) async fn take_items<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item: Item, quantity: i32) -> Result<(), CallError> {
    let available = super::item_volume::stack_size(item.singleton, i64::from(item.quantity));
    if quantity <= 0 || i64::from(quantity) > available || (item.singleton && quantity != 1) {
        return Err(notify_error(crate::messages::INSUFFICIENT_ITEM_QUANTITY));
    }
    let owner = item.owner_id;
    if i64::from(quantity) == available {
        inventory::delete(db, item.item_id).await.map_err(db_err)?;
        let old = item.location_id;
        let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
        outbox.notify_pilot(owner, "OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old))]), Value::None]);
    } else {
        if !transfers::take(db, item.item_id, quantity).await.map_err(db_err)? { return Err(notify_error(crate::messages::INSUFFICIENT_ITEM_QUANTITY)); }
        let left = Item { quantity: item.quantity - quantity, ..item };
        let change = Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int((left.quantity + quantity) as i64))]);
        outbox.notify_pilot(owner, "OnItemChange", vec![left.row(), change, Value::None]);
    }
    Ok(())
}
pub(super) async fn deliver<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, owner: i64, station: i64, type_id: i32, quantity: i32) -> Result<(), CallError> {
    let stack = transfers::stack(db, station, FLAG_HANGAR, type_id, owner).await.map_err(db_err)?;
    let (item_id, change) = match stack {
        Some(r) => { if !transfers::add(db, r.item_id, quantity).await.map_err(db_err)? { return Err(notify_error(crate::messages::INSUFFICIENT_ITEM_QUANTITY)); } (r.item_id, (IX_STACK_SIZE, r.quantity as i64)) }
        None => { let id = transfers::insert(db, type_id, owner, station, FLAG_HANGAR, quantity).await.map_err(db_err)?; (id, (IX_LOCATION_ID, 0)) }
    };
    let docked = transfers::docked(db, owner).await.map_err(db_err)?;
    if docked == Some(station) {
        if let Some(item) = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop() {
            let change = Value::Dict(vec![(Value::Int(change.0), Value::Int(change.1))]);
            outbox.notify_pilot(owner, "OnItemChange", vec![item.row(), change, Value::None]);
        }
    }
    Ok(())
}
pub(crate) async fn deposit<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, owner: i64, location: i64, flag: i32, type_id: i32, quantity: i32, from: i64) -> Result<(), CallError> {
    match transfers::stack(db, location, flag, type_id, owner).await.map_err(db_err)? {
        Some(r) => { if !transfers::add(db, r.item_id, quantity).await.map_err(db_err)? { return Err(notify_error(crate::messages::INSUFFICIENT_ITEM_QUANTITY)); } notify_item_change(db, sde, outbox, r.item_id, vec![(IX_STACK_SIZE, Value::Int(r.quantity as i64))]).await }
        None => { let id = transfers::insert(db, type_id, owner, location, flag, quantity).await.map_err(db_err)?; notify_item_change(db, sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(from)), (IX_FLAG, Value::Int(flag as i64))]).await }
    }
}
