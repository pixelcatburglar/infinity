//! `invbroker` binding, stack requests, authorized content reads and open
//! validation for `eve/client/script/environment/invCache.py`. Uses `game.items`,
//! office custody, corporate roles, trade reservations, loaded charges and SDE
//! data. Unsupported container types retain generic binding; structure inventories
//! are unsupported for content reads and direct-trade PLEX is refused.

use super::{CONTAINER_CORP_MARKET, CONTAINER_HANGAR, INVENTORY};
use super::stacks::{mutation_corporate, split_stack};
use super::crystals;
use super::access::may_view_corp_hangar;
use super::notifications::{notify_corp_item_change, notify_item_change};
use super::{labels, trashing};
use crate::client::Client;
use infinity_core::consts::{FLAG_CORP_DELIVERIES, IX_LOCATION_ID, IX_STACK_SIZE};
use crate::dao::items::loading::{Item, ItemQuery};
use crate::db::outbox::{dispatch_client, Outbox};
use infinity_core::messages;
use crate::services::{db_err, items::load_items, notify_error, value_id_list, Call, CallError, CallResult};
use marshal::Value;
use tracing::warn;

/// Calls on the inventory manager (`invbroker` bound to a location).
pub(super) async fn broker(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let char_id = c.session.int("charid").unwrap_or(0);
    Some(Ok(match call.method {
        "GetContainerContents" => return Some(super::reads::contents(c, call).await),
        "ValidateItemListCanBeOpened" => return Some(super::reads::validate(c, call).await),
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
                match may_view_corp_hangar(&c.db, char_id, corp, station, FLAG_CORP_DELIVERIES).await {
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
            let qty = call.args.get(2).and_then(|v| v.as_int()).unwrap_or(0);
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
            let endpoint = super::reachability::Endpoint { location: item.location_id, flag: item.flag_id };
            let reach = match super::reachability::Reachability::capture(
                &super::super::identity::Identity::of_client(c), &c.db, &c.world,
                c.session.int("solarsystemid"), &[endpoint],
            ).await { Ok(reach) => reach, Err(error) => return Some(Err(error)) };
            let audience = match super::notifications::Audience::capture(
                &super::super::identity::Identity::of_client(c), &c.db, &c.world,
                c.session.int("solarsystemid"), &[endpoint],
            ).await { Ok(audience) => audience, Err(error) => return Some(Err(error)) };
            let parents = match super::stacks::capture_parents(&c.db, &c.sde, &[endpoint]).await {
                Ok(parents) => parents, Err(error) => return Some(Err(error)),
            };
            let hull = match load_items(&c.db, &c.sde, ItemQuery::Id(endpoint.location)).await.map_err(db_err) {
                Ok(items) => items.into_iter().find(|row| row.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP)),
                Err(error) => return Some(Err(error)),
            };
            let r = db.transaction(
                move |tx, outbox: &mut Outbox| {
                    let item = item.clone();
                    let sde = sde.clone();
                    let reach = reach.clone();
                    let audience = audience.clone();
                    let parents = parents.clone();
                    let hull = hull.clone();
                    Box::pin(async move {
                        let _locks = super::stacks::lock_parents(tx, &sde, &parents, std::slice::from_ref(&item), &[item.item_id]).await?;
                        if let Some(hull) = &hull {
                            if !crate::dao::items::ship::lock_reload_ship(tx, hull.item_id).await.map_err(db_err)? {
                                return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                            }
                            let current = load_items(tx, &sde, ItemQuery::Id(hull.item_id)).await.map_err(db_err)?.into_iter().next()
                                .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                            if current.owner_id != hull.owner_id || current.location_id != hull.location_id || current.flag_id != hull.flag_id
                                || current.type_id != hull.type_id || current.singleton != hull.singleton || current.quantity != hull.quantity {
                                return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                            }
                        }
                        let checked = reach.inspect_many(tx, &sde, &[endpoint]).await?;
                        if !checked.admitted[0] {
                            return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                        }
                        let current = load_items(tx, &sde, ItemQuery::Id(item.item_id)).await.map_err(db_err)?.into_iter().next()
                            .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                        if current.owner_id != item.owner_id || current.location_id != item.location_id || current.flag_id != item.flag_id
                            || current.type_id != item.type_id || current.singleton != item.singleton || current.quantity != item.quantity {
                            return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                        }
                        let audience = audience.resolve(tx, &sde).await?;
                        let shared = checked.lineage(endpoint.location).is_some_and(|rows| super::movement::shared_lineage(&sde, endpoint, rows.len(), |id| rows.get(&id)));
                        let corporate = mutation_corporate(tx, &sde, &item, char_id, station, shared).await?;
                        let (owner, location, flag) = (item.owner_id, item.location_id, item.flag_id);
                        let id = split_stack(tx, &item, qty, owner, location, flag, false).await?;
                        let left = Item { quantity: item.quantity - qty, ..item.clone() };
                        let new = load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                        audience.item_change(outbox, &item, &left, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))], char_id);
                        let created = Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..new.clone() };
                        audience.item_change(outbox, &created, &new, vec![(IX_LOCATION_ID, Value::Int(0))], char_id);
                        if corporate {
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
        "TrashItems" => {
            let ids = value_id_list(call.args.first());
            let Some(location) = call.args.get(1).and_then(Value::as_int) else {
                return Some(Err(notify_error(messages::inventory::ITEM_WRONG_LOCATION)));
            };
            return Some(match trashing::trash_items(c, &ids, location).await {
                Ok(value) => Ok(value),
                Err(error) => trashing::broker_refusal_key(&error)
                    .map(|key| Ok(Value::list([Value::unicode(key)])))
                    .unwrap_or(Err(error)),
            });
        }
        // SetLabel(itemID, name): ship and container names.
        "SetLabel" => {
            let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let name = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            return Some(labels::set_label(c, item_id, name).await);
        }
        "WithdrawPlexFromVault" if call.args.get(2).and_then(Value::as_text).is_some_and(|v| v == "VaultToDirectTrade") => {
            return Some(Err(notify_error(messages::trade::TRADE_PLEX_UNSUPPORTED)));
        }
        "WithdrawPlexFromVault" => return Some(super::plex::convert(c, call, true).await),
        "DepositPlexToVault" => return Some(super::plex::convert(c, call, false).await),
        _ => return None,
    }))
}
