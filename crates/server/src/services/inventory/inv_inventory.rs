//! Bound `invInventory` requests from `eve/client/script/environment/invCache.py`
//! and `invCacheContainer`. Reads `game.items`, corporate roles and SDE types and
//! delegates movement, reachability and stack mutations. Soul binding and service
//! life damage are not modelled.

use super::access::{container_of, custody_allowed, may_view_corp_hangar, Container};
use super::movement;
use super::stacks::{merge_operations, stack_all};
use crate::client::{short, Client};
use crate::consts::{FLAG_CORP_DELIVERIES, FLAG_HANGAR, FLAG_PILOT, TYPE_CHARACTER, TYPE_PLASTIC_WRAP};
use crate::consts::dogma::ATTR_CAPACITY;
use crate::dao::items::loading::{self, GameItem, Item, ItemQuery};
use crate::db::executor::Executor;
use crate::messages;
use crate::sde::Sde;
use crate::services::{db_err, items::load_items, keyval, notify_error, value_id_list, Call, CallError, CallResult};
use marshal::Value;
use tracing::warn;
fn kw_or_arg<'a>(call: &'a Call<'_>, name: &str, idx: usize) -> Option<&'a Value> {
    call.kwargs.get(name).or_else(|| call.args.get(idx)).filter(|v| !v.is_none())
}

async fn list(c: &Client, container: &Container, flags: Option<Vec<i64>>) -> Result<Vec<Item>, CallError> {
    let flags: Option<Vec<i32>> = flags.map(|f| f.into_iter().filter_map(|x| i32::try_from(x).ok()).collect());
    let me = c.session.int("charid").unwrap_or(0);
    let items = match container {
        Container::Hangar { station_id, owner_id } => {
            if *owner_id != me || c.session.int("stationid") != Some(*station_id) { return Ok(vec![]); }
            let flags = flags.unwrap_or_else(|| vec![FLAG_HANGAR]);
            load_items(&c.db, &c.sde, ItemQuery::LocationOwnerFlags(*station_id, *owner_id, &flags)).await.map_err(db_err)?
        }
        Container::Deliveries { station_id, owner_id } => {
            if c.session.int("stationid") != Some(*station_id) || c.sde.map.station(*station_id).is_none()
                || !may_view_corp_hangar(&c.db, me, *owner_id, FLAG_CORP_DELIVERIES).await?
                || flags.as_ref().is_some_and(|f| !f.contains(&FLAG_CORP_DELIVERIES)) { return Ok(vec![]); }
            load_items(&c.db, &c.sde, ItemQuery::LocationOwnerFlags(*station_id, *owner_id, &[FLAG_CORP_DELIVERIES])).await.map_err(db_err)?
        }
        Container::Item(item_id) => match flags {
            Some(flags) => load_items(&c.db, &c.sde, ItemQuery::LocationFlags(*item_id, &flags)).await.map_err(db_err)?,
            None => load_items(&c.db, &c.sde, ItemQuery::Location(*item_id)).await.map_err(db_err)?,
        },
    };
    let mut visible = Vec::new();
    for item in items {
        if let Some(allowed) = custody_allowed(&c.db, &c.sde, item.item_id, me, c.session.int("stationid"), false).await? {
            if !allowed { continue; }
        } else if matches!(container, Container::Item(id) if c.sde.map.station(*id).is_some()) && item.owner_id != me {
            continue;
        }
        visible.push(item);
    }
    Ok(visible)
}

/// The container's own item row (`GetSelfInvItem`).
async fn self_item(c: &Client, container: &Container) -> Result<Value, CallError> {
    let id = match container {
        Container::Hangar { station_id, .. } | Container::Deliveries { station_id, .. } => *station_id,
        Container::Item(id) => *id,
    };
    if let Some(item) = load_items(&c.db, &c.sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next() {
        return Ok(item.row());
    }
    if Some(id) == c.session.int("charid") {
        return Ok(Item {
            item_id: id,
            type_id: TYPE_CHARACTER,
            owner_id: id,
            location_id: c.session.int("shipid").unwrap_or(0),
            flag_id: FLAG_PILOT,
            quantity: 1,
            singleton: true,
            group_id: 1,
            category_id: 1,
            custom_info: None,
        }
        .row());
    }
    // A station (or other static location) as an item.
    let item = c.sde.map.celestial(id).and_then(|celestial| {
        let type_id = i32::try_from(celestial.type_id?).ok()?;
        let data = c.sde.types.get(type_id)?;
        Some(Item {
            item_id: id, type_id,
            owner_id: c.sde.map.station(id).and_then(|station| station.owner_id).unwrap_or(1),
            location_id: celestial.solar_system_id.unwrap_or(0),
            flag_id: 0, quantity: 1, singleton: true,
            group_id: celestial.group_id.unwrap_or(0),
            category_id: i64::from(data.category_id.unwrap_or(0)),
            custom_info: None,
        }.row())
    });
    Ok(item.unwrap_or(Value::None))
}

pub(super) fn item_capacity(items: &[GameItem], id: i64, sde: &Sde) -> Result<(f64, f64), CallError> {
    let Some(parent) = items.iter().find(|item| item.item_id == id) else { return Ok((0.0, 0.0)) };
    let used = items.iter().filter(|item| item.location_id == parent.item_id).try_fold(0.0_f64, |total, item| {
        Ok::<_, CallError>(total + super::super::item_volume::item_volume(sde, item.type_id, item.singleton, i64::from(item.quantity), None)?)
    })?;
    let data = sde.types.get(parent.type_id).ok_or_else(|| anyhow::anyhow!("missing SDE type {}", parent.type_id))?;
    let capacity = if parent.type_id == TYPE_PLASTIC_WRAP { used } else { data.attribute(ATTR_CAPACITY).or(data.capacity).unwrap_or(0.0) };
    Ok((capacity, used))
}

pub(super) async fn item_capacity_reply(ex: &(impl Executor + ?Sized), sde: &Sde, id: i64) -> Result<Value, CallError> {
    let items = loading::load_items(ex, ItemQuery::IdOrLocation(id)).await.map_err(db_err)?;
    let (cap, used) = item_capacity(&items, id, sde)?;
    Ok(keyval([("capacity", Value::Float(cap)), ("used", Value::Float(used))]))
}

async fn capacity(c: &Client, container: &Container) -> Result<Value, CallError> {
    let (cap, used) = match container {
        Container::Hangar { .. } | Container::Deliveries { .. } => (1e12, 0.0),
        Container::Item(id) => return item_capacity_reply(&c.db, &c.sde, *id).await,
    };
    Ok(keyval([("capacity", Value::Float(cap)), ("used", Value::Float(used))]))
}
/// Calls on a bound inventory.
pub(super) async fn inventory(c: &mut Client, call: &Call<'_>) -> CallResult {
    let Some(container) = container_of(call.bind_params) else {
        return Err(CallError::Internal(anyhow::anyhow!("bad inventory bind params {:?}", call.bind_params)));
    };
    if let Container::Deliveries { station_id, owner_id } = &container {
        let me = c.session.int("charid").unwrap_or(0);
        if c.session.int("stationid") != Some(*station_id) || c.sde.map.station(*station_id).is_none()
            || !may_view_corp_hangar(&c.db, me, *owner_id, FLAG_CORP_DELIVERIES).await? {
            return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
    }
    if let Container::Item(id) = &container {
        if super::super::trade::is_trade_container(*id) {
            return Err(notify_error(messages::trade::TRADE_CONTAINER_RESERVED));
        }
        if !crate::dao::trades::reserved(&c.db, &[*id]).await.map_err(db_err)?.is_empty() {
            return Err(notify_error(messages::trade::TRADE_CONTAINER_RESERVED));
        }
        let me = c.session.int("charid").unwrap_or(0);
        if custody_allowed(&c.db, &c.sde, *id, me, c.session.int("stationid"), false).await? == Some(false) {
            return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
    }
    match call.method {
        "List" => {
            let flag = kw_or_arg(call, "flag", 0).and_then(|v| v.as_int());
            let items = list(c, &container, flag.map(|f| vec![f])).await?;
            Ok(Value::List(items.iter().map(Item::row).collect()))
        }
        "ListByFlags" => {
            let flags = kw_or_arg(call, "flags", 0).and_then(|v| v.items()).map(|f| f.iter().filter_map(|x| x.as_int()).collect());
            let items = list(c, &container, flags).await?;
            Ok(Value::List(items.iter().map(Item::row).collect()))
        }
        "GetSelfInvItem" => self_item(c, &container).await,
        "GetItem" => self_item(c, &container).await,
        "GetCapacity" => capacity(c, &container).await,
        "GetAvailableTurretSlots" => Ok(Value::list([])),
        // Add(itemID, sourceID, flag=, qty=) -> itemID now in this container
        "Add" => {
            let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let flag = call.kwargs.get("flag").and_then(|v| v.as_int()).map(|f| f as i32);
            let qty = call.kwargs.get("qty").or_else(|| call.kwargs.get("quantity")).and_then(|v| v.as_int());
            movement::move_item(&super::super::identity::Identity::of_client(c), &c.db, &c.world, &c.sde, c.session.int("solarsystemid"), c.notify_tx.clone(), &container, item_id, call.args.get(1).and_then(Value::as_int), flag, qty, |n| super::super::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await.map(|id| id.map(Value::Int).unwrap_or(Value::None))
        }
        // MultiAdd(itemIDs, sourceID, flag=)
        "MultiAdd" => {
            let flag = call.kwargs.get("flag").and_then(|v| v.as_int()).map(|f| f as i32);
            for item_id in value_id_list(call.args.first()) {
                movement::move_item(&super::super::identity::Identity::of_client(c), &c.db, &c.world, &c.sde, c.session.int("solarsystemid"), c.notify_tx.clone(), &container, item_id, call.args.get(1).and_then(Value::as_int), flag, None, |n| super::super::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await?;
            }
            Ok(Value::None)
        }
        // MultiMerge([(sourceID, destID, qty)], sourceContainerID)
        "MultiMerge" => {
            let ops: Vec<(i64, i64, i64)> = call
                .args
                .first()
                .and_then(|v| v.items())
                .unwrap_or_default()
                .iter()
                .filter_map(|op| match op.items()? {
                    [s, d, q] => Some((s.as_int()?, d.as_int()?, q.as_int()?)),
                    _ => None,
                })
                .collect();
            merge_operations(c, &container, call.args.get(1).and_then(Value::as_int), ops).await?;
            Ok(Value::None)
        }
        "StackAll" => {
            let flag = kw_or_arg(call, "flag", 0).and_then(|v| v.as_int());
            stack_all(c, &container, flag).await.map(|_| Value::None)
        }
        // {itemID: ...} maps; nothing soul-bound or service-life damaged.
        "ListSoulBound" | "ListServiceLifeDamage" => Ok(Value::Dict(vec![])),
        _ => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED inventory::{} args={} kwargs={}",
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}

#[cfg(test)]
#[path = "tests/deliveries.rs"]
mod deliveries;
