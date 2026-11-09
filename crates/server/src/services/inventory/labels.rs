//! Authoritative `invbroker.SetLabel` validation and post-commit location-cache updates.
//!
//! Client limits come from `eve/common/script/ui/services/shipNameDialog.py`
//! (ship names) and `eve/common/script/inventory/menuFunctions.py`
//! (container labels). Foreign ship ancestors require current bay access; naming
//! guest-owned items without taking them is not supported.
use super::access::{corporate_custody, custody_allowed};
use infinity_core::{consts::{CARGO_CONTAINER_GROUPS, CATEGORY_SHIP}, messages};
use crate::{client::Client, dao::items::{inventory as inventory_dao, loading::ItemQuery}, db::outbox::{dispatch_client, Outbox}, services::{db_err, items::load_items, market, notify_error, CallError}, services::config};
use super::reachability::{Endpoint, Reachability};
use crate::services::identity::Identity;
use marshal::Value;

/// `shipNameDialog` maximum ship-name length.
const SHIP_NAME_LIMIT: usize = 32;
/// `menuFunctions` maximum container-label length.
const CONTAINER_NAME_LIMIT: usize = 100;

fn valid_name(name: &str, limit: usize) -> bool { name.chars().count() <= limit && !name.chars().any(char::is_control) }

pub(crate) async fn set_label(c: &mut Client, item_id: i64, name: String) -> Result<Value, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    if !valid_name(&name, CONTAINER_NAME_LIMIT) { return Err(notify_error(messages::inventory::NAME_INVALID)); }
    let _lock = market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let observed = load_items(&db, &sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let endpoint = Endpoint { location: observed.location_id, flag: observed.flag_id };
    let identity = Identity::of_client(c);
    let reach = Reachability::capture(&identity, &db, &c.world, identity.system_id, &[endpoint]).await?;
    db.transaction(move |tx, _outbox: &mut Outbox| {
        let sde = sde.clone(); let name = name.clone();
        let reach = reach.clone();
        Box::pin(async move {
            if !crate::dao::trades::reserved(tx, &[item_id]).await.map_err(db_err)?.is_empty() { return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED)); }
            let item = load_items(tx, &sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
            let ship = item.category_id == i64::from(CATEGORY_SHIP);
            if !ship && !CARGO_CONTAINER_GROUPS.contains(&(item.group_id as i32)) { return Err(notify_error(messages::inventory::NAME_NOT_RENAMEABLE)); }
            if ship && name.chars().count() > SHIP_NAME_LIMIT { return Err(notify_error(messages::inventory::NAME_INVALID)); }
            let endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
            let ancestors = crate::dao::items::deliveries::ancestry(tx, endpoint.location).await.map_err(db_err)?;
            if ancestors.iter().any(|row| row.owner_id != me && sde.types.get(row.type_id).is_some_and(|ty| ty.category_id == Some(CATEGORY_SHIP))) {
                let checked = reach.inspect_many(tx, &sde, &[endpoint]).await?;
                if !checked.admitted[0] { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
            }
            let custody = corporate_custody(tx, Some(&sde), item_id).await?;
            if let Some(false) = custody_allowed(tx, &sde, item_id, me, None, true).await? { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
            if custody.is_none() && item.owner_id != me { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
            let limit = if ship { SHIP_NAME_LIMIT as i32 } else { CONTAINER_NAME_LIMIT as i32 };
            if !inventory_dao::rename(tx, item_id, &name, item.owner_id, item.type_id, item.location_id, item.flag_id, limit).await.map_err(db_err)? { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
            Ok(config::location_row(item_id, &name))
        })
    }, |row, mut outbox| async move {
        outbox.notify_client("OnCfgDataChanged", vec![Value::str("evelocations"), Value::list([row])]);
        dispatch_client(Value::None, outbox, c)
    }).await
}

#[cfg(test)]
mod tests;
