//! `invbroker` binding and stack requests from `eve/client/script/environment/invCache.py`.
//! Uses `game.items`, corporate roles, trade reservations and SDE data. Unsupported
//! container types retain the existing generic binding; direct-trade PLEX is refused.

use super::{CONTAINER_CORP_MARKET, CONTAINER_HANGAR, INVENTORY};
use super::crystals;
use super::access::may_view_corp_hangar;
use super::notifications::{notify_corp_item_change, notify_item_change};
use super::stacks::{mutation_corporate, split_stack, trash_items};
use crate::client::Client;
use crate::consts::{FLAG_CORP_DELIVERIES, IX_LOCATION_ID, IX_STACK_SIZE};
use crate::dao::items::{inventory as inventory_dao, loading::{Item, ItemQuery}};
use crate::db::outbox::{dispatch_client, Outbox};
use crate::messages;
use crate::services::{db_err, items::load_items, notify_error, value_id_list, Call, CallError, CallResult};
use marshal::Value;
use tracing::warn;

/// Calls on the inventory manager (`invbroker` bound to a location).
pub(super) async fn broker(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let char_id = c.session.int("charid").unwrap_or(0);
    Some(Ok(match call.method {
        "GetInventoryFromId" => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            if super::super::trade::is_trade_container(id) {
                return Some(Err(notify_error(messages::trade::TRADE_CONTAINER_RESERVED)));
            }
            match crate::dao::trades::reserved(&c.db, &[id]).await {
                Ok(ids) if !ids.is_empty() => return Some(Err(notify_error(messages::trade::TRADE_CONTAINER_RESERVED))),
                Err(error) => return Some(Err(db_err(error))),
                _ => {}
            }
            if super::super::exploration::container_locked(id) {
                let msg = Value::unicode(messages::CONTAINER_LOCKED_NEEDS_ANALYZER);
                return Some(Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), msg)] }));
            }
            c.bind(INVENTORY, Value::tuple([Value::Int(id)]))
        }
        "GetInventory" => {
            let container = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            if container == CONTAINER_HANGAR {
                let station = c.session.int("stationid").unwrap_or(0);
                c.bind(INVENTORY, Value::tuple([Value::Int(station), Value::Int(CONTAINER_HANGAR), Value::Int(char_id)]))
            } else if container == CONTAINER_CORP_MARKET {
                let station = c.session.int("stationid").unwrap_or(0);
                let corp = call.args.get(1).and_then(Value::as_int).unwrap_or(0);
                match may_view_corp_hangar(&c.db, char_id, corp, FLAG_CORP_DELIVERIES).await {
                    Ok(true) if c.sde.map.station(station).is_some() => c.bind(INVENTORY, Value::tuple([Value::Int(station), Value::Int(CONTAINER_CORP_MARKET), Value::Int(corp)])),
                    Ok(_) => return Some(Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))),
                    Err(error) => return Some(Err(error)),
                }
            } else {
                warn!(user = c.login.user_id, "invbroker::GetInventory for unsupported container {container}");
                c.bind(INVENTORY, Value::tuple([Value::Int(container)]))
            }
        }
        "GetItemDescriptor" => super::super::item_descriptor().to_value(),
        // inventorysvc.py:CrystalDamageTracker.PrimeCrystals reads a batch dict.
        "GetDamageForCrystals" => return Some(crystals::get_damage(c, call).await),
        "IsMyItemAtDockableLocation" => Value::Bool(true),
        // SplitStack(locationID, itemID, qty, ownerID): a new stack beside the old one.
        "SplitStack" => {
            let _lock = super::super::market::lock().await;
            let item_id = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            let qty = call.args.get(2).and_then(|v| v.as_int()).unwrap_or(0) as i32;
            let Some(item) = (match load_items(&c.db, &c.sde, ItemQuery::Id(item_id)).await.map_err(db_err) {
                Ok(items) => items.into_iter().next(),
                Err(e) => return Some(Err(e)),
            }) else {
                return Some(Ok(Value::None));
            };
            if item.singleton || qty <= 0 || qty >= item.quantity {
                return Some(Ok(Value::None));
            }
            let db = c.db.clone();
            let sde = c.sde.clone();
            let station = c.session.int("stationid");
            let r = db.transaction(
                move |tx, outbox: &mut Outbox| {
                    let item = item.clone();
                    let sde = sde.clone();
                    Box::pin(async move {
                        let corporate = mutation_corporate(tx, &sde, &item, char_id, station).await?;
                        let (owner, location, flag) = (item.owner_id, item.location_id, item.flag_id);
                        let id = split_stack(tx, &item, qty, owner, location, flag, false).await?;
                        if corporate {
                            let left = Item { quantity: item.quantity - qty, ..item.clone() };
                            let new = load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                            notify_corp_item_change(tx, outbox, &left, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
                            notify_corp_item_change(tx, outbox, &new, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                        } else {
                            notify_item_change(tx, &sde, outbox, item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
                            notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                        }
                        Ok(Value::Int(id))
                    })
                },
                |value, outbox| async move { dispatch_client(value, outbox, c) },
            ).await;
            return Some(r);
        }
        // TrashItems(itemIDs, locationID) -> [error messages]
        "TrashItems" => {
            let ids = value_id_list(call.args.first());
            let r = trash_items(c, &ids).await.map(|()| Value::list([]));
            return Some(r);
        }
        // SetLabel(itemID, name): ship and container names.
        "SetLabel" => {
            let _lock = super::super::market::lock().await;
            let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let name = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let db = c.db.clone();
            let r = db.transaction(
                move |tx, _outbox: &mut Outbox| {
                    let name = name.clone();
                    Box::pin(async move {
                        if !crate::dao::trades::reserved(tx, &[item_id]).await.map_err(db_err)?.is_empty() {
                            return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                        }
                        inventory_dao::rename(tx, item_id, &name, char_id).await.map_err(db_err)?;
                        Ok(Value::None)
                    })
                },
                |value, outbox| async move { dispatch_client(value, outbox, c) },
            ).await;
            return Some(r);
        }
        "WithdrawPlexFromVault" if call.args.get(2).and_then(Value::as_text).is_some_and(|v| v == "VaultToDirectTrade") => {
            return Some(Err(notify_error(messages::trade::TRADE_PLEX_UNSUPPORTED)));
        }
        _ => return None,
    }))
}
