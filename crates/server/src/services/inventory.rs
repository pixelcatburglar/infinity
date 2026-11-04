//! invbroker: inventory managers (bound per station / solar system) hand out
//! bound inventories, which list and describe the items in a container
//! (eve/client/script/environment/invCache.py, invCacheContainer).
//! Item quantities are stored in `game.items`; static item data comes from the loaded SDE.
//! Merge plans are guarded against stale snapshots. Staged trade items and their
//! descendants cannot be moved, split, merged, trashed, renamed or accessed here.
//! Item capacity and contents share one game-item snapshot; child volumes follow
//! `inventorycommon.util.GetItemVolume`, including packaged and assembled items.
//! Corporation deliveries are station-owned custody, viewable by authorized
//! members and received only through server business operations, never drops.
//! Player transfers and merges check physical reachability of their inventories.

use crate::dao::items::loading::{self, GameItem, Item, ItemQuery};
use super::items::load_items;
use crate::dao::items::inventory as inventory_dao;
use crate::dao::items::quantities::{self, QuantityChange};
use super::{db_err, keyval, notify_error, value_id_list, Call, CallError, CallResult};
use crate::client::{short, Client};
use crate::consts::{dogma::ATTR_CAPACITY, CATEGORY_SHIP, FLAG_CARGO, FLAG_FITTING_FIRST, FLAG_FITTING_LAST, FLAG_HANGAR, FLAG_PILOT, FLAG_RIG_FIRST, FLAG_RIG_LAST, FLAG_SUBSYSTEM_FIRST, FLAG_SUBSYSTEM_LAST, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD, TYPE_CHARACTER, TYPE_PLASTIC_WRAP};
use crate::consts::{CORP_HANGAR_DIVISIONS, FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, ROLE_DELIVERIES_TAKE, ROLE_DELIVERIES_VIEW, ROLE_DIRECTOR, ROLE_HANGAR_CAN_QUERY_1, ROLE_HANGAR_CAN_TAKE_1};
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_client, Outbox};
use crate::messages;
use crate::sde::Sde;
use marshal::Value;
use std::collections::BTreeMap;
use tracing::warn;

pub(super) mod gm;
pub(crate) mod movement;
pub(in crate::services) mod reachability;
use reachability::{Endpoint, Reachability};
pub(crate) mod expert_systems;

/// const.containerHangar: "my hangar in the current station".
const CONTAINER_HANGAR: i64 = 10004;
/// inventorycommon.const.containerCorpMarket: corporation station deliveries.
const CONTAINER_CORP_MARKET: i64 = 10012;
/// Internal bound-object name for a single inventory.
pub const INVENTORY: &str = "invInventory";

/// Which container a bound inventory lists.
#[derive(Clone)]
pub(crate) enum Container {
    /// The character's hangar in a station.
    Hangar { station_id: i64, owner_id: i64 },
    /// Corporation-owned deliveries at a station.
    Deliveries { station_id: i64, owner_id: i64 },
    /// Any item acting as a container (ship, character, can...).
    Item(i64),
}

fn container_of(params: Option<&Value>) -> Option<Container> {
    let p = params?.items()?;
    match p {
        [Value::Int(station), Value::Int(CONTAINER_HANGAR), Value::Int(owner)] => {
            Some(Container::Hangar { station_id: *station, owner_id: *owner })
        }
        [Value::Int(station), Value::Int(CONTAINER_CORP_MARKET), Value::Int(owner)] => {
            Some(Container::Deliveries { station_id: *station, owner_id: *owner })
        }
        [Value::Int(item)] => Some(Container::Item(*item)),
        _ => None,
    }
}

fn corp_hangar_role(flag: i32, take: bool) -> Option<i64> {
    if flag == FLAG_CORP_DELIVERIES {
        Some(if take { ROLE_DELIVERIES_TAKE } else { ROLE_DELIVERIES_VIEW })
    } else if (FLAG_CORP_SAG1..FLAG_CORP_SAG1 + CORP_HANGAR_DIVISIONS).contains(&flag) {
        let base = if take { ROLE_HANGAR_CAN_TAKE_1 } else { ROLE_HANGAR_CAN_QUERY_1 };
        Some(base << (flag - FLAG_CORP_SAG1))
    } else {
        None
    }
}

pub(in crate::services) async fn may_view_corp_hangar(ex: &(impl Executor + ?Sized), char_id: i64, corp_id: i64, flag: i32) -> Result<bool, CallError> {
    match corp_hangar_role(flag, false) {
        Some(role) => super::has_role_or_is_ceo(ex, char_id, corp_id, role | ROLE_DIRECTOR).await,
        None => Ok(false),
    }
}

pub(in crate::services) async fn may_take_corp_hangar(ex: &(impl Executor + ?Sized), char_id: i64, corp_id: i64, flag: i32) -> Result<bool, CallError> {
    match corp_hangar_role(flag, true) {
        Some(role) => super::has_role_or_is_ceo(ex, char_id, corp_id, role | ROLE_DIRECTOR).await,
        None => Ok(false),
    }
}

/// Resolve the office division or station Deliveries governing an item's ancestry.
pub(in crate::services) async fn corporate_custody(ex: &(impl Executor + ?Sized), sde: Option<&Sde>, id: i64) -> Result<Option<(i64, i64, i32)>, CallError> {
    for row in crate::dao::items::deliveries::ancestry(ex, id).await.map_err(db_err)? {
        if let Some(corp) = super::offices::office_owner(ex, row.location_id).await? {
            return Ok(Some((corp, row.location_id, row.flag_id)));
        }
        if row.flag_id == FLAG_CORP_DELIVERIES && sde.is_none_or(|sde| sde.map.station(row.location_id).is_some())
            && super::corps::is_player_corp(row.owner_id) {
            return Ok(Some((row.owner_id, row.location_id, row.flag_id)));
        }
    }
    Ok(None)
}

async fn custody_allowed(ex: &(impl Executor + ?Sized), sde: &Sde, id: i64, me: i64, station: Option<i64>, take: bool) -> Result<Option<bool>, CallError> {
    let Some((corp, location, flag)) = corporate_custody(ex, Some(sde), id).await? else { return Ok(None) };
    let role = if take { may_take_corp_hangar(ex, me, corp, flag).await? } else { may_view_corp_hangar(ex, me, corp, flag).await? };
    Ok(Some(role && (flag != FLAG_CORP_DELIVERIES || station == Some(location))))
}

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

fn item_capacity(items: &[GameItem], id: i64, sde: &Sde) -> Result<(f64, f64), CallError> {
    let Some(parent) = items.iter().find(|item| item.item_id == id) else { return Ok((0.0, 0.0)) };
    let used = items.iter().filter(|item| item.location_id == parent.item_id).try_fold(0.0_f64, |total, item| {
        Ok::<_, CallError>(total + super::item_volume::item_volume(sde, item.type_id, item.singleton, i64::from(item.quantity), None)?)
    })?;
    let data = sde.types.get(parent.type_id).ok_or_else(|| anyhow::anyhow!("missing SDE type {}", parent.type_id))?;
    let capacity = if parent.type_id == TYPE_PLASTIC_WRAP { used } else { data.attribute(ATTR_CAPACITY).or(data.capacity).unwrap_or(0.0) };
    Ok((capacity, used))
}

async fn item_capacity_reply(ex: &(impl Executor + ?Sized), sde: &Sde, id: i64) -> Result<Value, CallError> {
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

// ---------------------------------------------------------------- moving items


/// Fitting slot flags: low, medium, high, rig and subsystem slots.
pub fn is_fitting_flag(flag: i32) -> bool {
    (FLAG_FITTING_FIRST..=FLAG_FITTING_LAST).contains(&flag) || (FLAG_RIG_FIRST..=FLAG_RIG_LAST).contains(&flag) || (FLAG_SUBSYSTEM_FIRST..=FLAG_SUBSYSTEM_LAST).contains(&flag)
}

/// `OnItemChange(item, change, location)` for `item_id` as it is now in the database.
pub async fn notify_item_change<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, item_id: i64, change: Vec<(i64, Value)>) -> Result<(), CallError> {
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
pub(super) fn notify_item_removed<X: Send>(outbox: &mut Outbox<X>, item: Item) {
    let old_location = item.location_id;
    let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
    let change = Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old_location))]);
    outbox.notify_client("OnItemChange", vec![gone.row(), change, Value::None]);
}

pub(super) async fn split_stack(db: &(impl Executor + ?Sized), item: &Item, quantity: i32, owner: i64, location: i64, flag: i32, singleton: bool) -> Result<i64, CallError> {
    if !crate::dao::trades::reserved(db, &[item.item_id]).await.map_err(db_err)?.is_empty() {
        return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
    }
    inventory_dao::split(db, item, quantity, owner, location, flag, singleton).await.map_err(db_err)?
        .ok_or_else(|| notify_error(messages::STACK_CANNOT_BE_SPLIT))
}

/// Apply the same ownership and active-ship policy as TrashItems.
pub(crate) async fn trash_items(c: &mut Client, ids: &[i64]) -> Result<(), CallError> {
    let _lock = super::market::lock().await;
    let char_id = c.session.int("charid").unwrap_or(0);
    let ship = c.session.int("shipid");
    let db = c.db.clone();
    let sde = c.sde.clone();
    let ids = ids.to_vec();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let ids = ids.clone();
            let sde = sde.clone();
            Box::pin(async move {
                if !crate::dao::trades::reserved(tx, &ids).await.map_err(db_err)?.is_empty() {
                    return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                }
                if crate::dao::industry::has_active_blueprint_job(tx, &ids).await.map_err(db_err)? {
                    return Err(notify_error(messages::industry::BLUEPRINT_ACTIVE));
                }
                let mut items = load_items(tx, &sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
                items.sort_by_key(|item| item.item_id);
                for item in items {
                    if corporate_custody(tx, Some(&sde), item.item_id).await?.is_some() {
                        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                    }
                    if item.owner_id != char_id { continue; }
                    if Some(item.item_id) != ship {
                        inventory_dao::delete(tx, item.item_id).await.map_err(db_err)?;
                        notify_item_removed(outbox, item);
                    }
                }
                Ok(())
            })
        },
        |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}




/// Simulate transfers in request order, retaining the original snapshots for guarded writes.
fn plan_merges(items: &[Item], ops: &[(i64, i64, i64)]) -> Result<BTreeMap<i64, QuantityChange>, CallError> {
    let mut states: BTreeMap<i64, (&Item, Option<i32>)> = items.iter().map(|item| (item.item_id, (item, Some(item.quantity)))).collect();
    for &(source, dest, requested) in ops {
        if source == dest { continue; }
        let (Some(&(src, Some(src_qty))), Some(&(dst, Some(dst_qty)))) = (states.get(&source), states.get(&dest)) else { continue };
        if src.type_id != dst.type_id || src.singleton || dst.singleton || src_qty <= 0 { continue; }
        if (src.owner_id, src.location_id, src.flag_id) != (dst.owner_id, dst.location_id, dst.flag_id) { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
        let take = requested.clamp(1, i64::from(src_qty)) as i32;
        let total = dst_qty.checked_add(take).ok_or_else(|| notify_error(messages::INSUFFICIENT_ITEM_QUANTITY))?;
        states.get_mut(&source).unwrap().1 = if take == src_qty { None } else { Some(src_qty - take) };
        states.get_mut(&dest).unwrap().1 = Some(total);
    }
    Ok(states.into_iter().filter_map(|(id, (item, quantity))| {
        (quantity != Some(item.quantity)).then(|| (id, QuantityChange::new(item, quantity)))
    }).collect())
}

async fn mutation_corporate(ex: &(impl Executor + ?Sized), sde: &Sde, item: &Item, me: i64, station: Option<i64>) -> Result<bool, CallError> {
    if let Some(allowed) = custody_allowed(ex, sde, item.item_id, me, station, true).await? {
        if !allowed { return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE)); }
        return Ok(true);
    }
    if item.owner_id != me { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
    Ok(false)
}

async fn apply_merges<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, reach: &Reachability, outbox: &mut Outbox<X>, items: Vec<Item>, ops: &[(i64, i64, i64)], me: i64, station: Option<i64>) -> Result<(), CallError> {
    let mut corporate = Vec::new();
    for item in &items {
        if mutation_corporate(db, sde, item, me, station).await? { corporate.push(item.item_id); }
        reach.check(db, sde, Endpoint { location: item.location_id, flag: item.flag_id }).await?;
    }
    let changes = plan_merges(&items, ops)?;
    if !quantities::apply(db, &changes).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY)); }
    for item in items {
        if let Some(change) = changes.get(&item.item_id) {
            if corporate.contains(&item.item_id) {
                let updated = if let Some(quantity) = change.quantity { Item { quantity, ..item.clone() } } else { Item { location_id: LOCATION_JUNKYARD, ..item.clone() } };
                let key = if change.quantity.is_some() { IX_STACK_SIZE } else { IX_LOCATION_ID };
                let old = if change.quantity.is_some() { item.quantity as i64 } else { item.location_id };
                notify_corp_item_change(db, outbox, &updated, vec![(key, Value::Int(old))]).await?;
            } else if change.quantity.is_none() {
                notify_item_removed(outbox, item);
            } else {
                notify_item_change(db, sde, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
            }
        }
    }
    Ok(())
}

async fn merge_operations(c: &mut Client, container: &Container, source_container: Option<i64>, ops: Vec<(i64, i64, i64)>) -> Result<(), CallError> {
    let _lock = super::market::lock().await;
    let me = c.session.int("charid").unwrap_or(0);
    let station = c.session.int("stationid");
    let ids: Vec<i64> = ops.iter().flat_map(|&(source, dest, _)| [source, dest]).collect();
    let snapshots = load_items(&c.db, &c.sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
    let endpoints: Vec<_> = snapshots.iter().map(|item| Endpoint { location: item.location_id, flag: item.flag_id }).collect();
    let reach = Reachability::capture(&super::identity::Identity::of_client(c), &c.db, &c.world, c.session.int("solarsystemid"), &endpoints).await?;
    let container = container.clone();
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let ops = ops.clone();
            let sde = sde.clone();
            let container = container.clone();
            let reach = reach.clone();
            Box::pin(async move {
                let ids: Vec<i64> = ops.iter().flat_map(|&(source, dest, _)| [source, dest]).collect();
                if !crate::dao::trades::reserved(tx, &ids).await.map_err(db_err)?.is_empty() {
                    return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                }
                let items = load_items(tx, &sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
                for item in &items {
                    if ops.iter().any(|&(source, _, _)| source == item.item_id)
                        && source_container.is_some_and(|id| id != item.location_id) {
                        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                    }
                    if ops.iter().any(|&(_, dest, _)| dest == item.item_id) {
                        let matches = match &container {
                            Container::Hangar { station_id, owner_id } => item.location_id == *station_id && item.owner_id == *owner_id && item.flag_id == FLAG_HANGAR,
                            Container::Deliveries { station_id, owner_id } => item.location_id == *station_id && item.owner_id == *owner_id && item.flag_id == FLAG_CORP_DELIVERIES,
                            Container::Item(id) => item.location_id == *id,
                        };
                        if !matches { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
                    }
                }
                apply_merges(tx, &sde, &reach, outbox, items, &ops, me, station).await
            })
        },
        |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}

async fn stack_items(ex: &(impl Executor + ?Sized), sde: &Sde, container: &Container, flag: Option<i64>) -> Result<Vec<Item>, CallError> {
    let flags = flag.map(|f| vec![f as i32]);
    let items = match container {
        Container::Hangar { station_id, owner_id } => {
            let flags = flags.unwrap_or_else(|| vec![FLAG_HANGAR]);
            load_items(ex, sde, ItemQuery::LocationOwnerFlags(*station_id, *owner_id, &flags)).await
        }
        Container::Deliveries { station_id, owner_id } =>
            load_items(ex, sde, ItemQuery::LocationOwnerFlags(*station_id, *owner_id, &[FLAG_CORP_DELIVERIES])).await,
        Container::Item(id) => match flags {
            Some(flags) => load_items(ex, sde, ItemQuery::LocationFlags(*id, &flags)).await,
            None => load_items(ex, sde, ItemQuery::Location(*id)).await,
        },
    };
    items.map_err(db_err)
}

/// `StackAll([flag])`: merge every unassembled stack of the same type in the container.
async fn stack_all(c: &mut Client, container: &Container, flag: Option<i64>) -> Result<(), CallError> {
    let _lock = super::market::lock().await;
    let me = c.session.int("charid").unwrap_or(0);
    let station = c.session.int("stationid");
    if let Container::Item(id) = container {
        if super::offices::office_owner(&c.db, *id).await?.is_some_and(|corp| c.session.int("corpid") != Some(corp)) {
            return Ok(());
        }
    }
    let endpoint = match container {
        Container::Hangar { station_id, .. } => Endpoint { location: *station_id, flag: flag.map(|f| f as i32).unwrap_or(FLAG_HANGAR) },
        Container::Deliveries { station_id, .. } => Endpoint { location: *station_id, flag: FLAG_CORP_DELIVERIES },
        Container::Item(id) => Endpoint { location: *id, flag: flag.map(|f| f as i32).unwrap_or(FLAG_CARGO) },
    };
    let snapshots = stack_items(&c.db, &c.sde, container, flag).await?;
    let mut endpoints = vec![endpoint];
    endpoints.extend(snapshots.iter().map(|item| Endpoint { location: item.location_id, flag: item.flag_id }));
    let reach = Reachability::capture(&super::identity::Identity::of_client(c), &c.db, &c.world, c.session.int("solarsystemid"), &endpoints).await?;
    let container = container.clone();
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let container = container.clone();
            let sde = sde.clone();
            let reach = reach.clone();
            Box::pin(async move {
                if let Container::Deliveries { station_id, owner_id } = &container {
                    if station != Some(*station_id) || !may_take_corp_hangar(tx, me, *owner_id, FLAG_CORP_DELIVERIES).await? {
                        return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE));
                    }
                }
                let items = stack_items(tx, &sde, &container, flag).await?;
                let ids: Vec<i64> = items.iter().map(|item| item.item_id).collect();
                if !crate::dao::trades::reserved(tx, &ids).await.map_err(db_err)?.is_empty() {
                    return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                }
                reach.check(tx, &sde, endpoint).await?;
                let mut first: std::collections::HashMap<(i32, i32), i64> = Default::default();
                let mut ops = Vec::new();
                for item in items.iter().filter(|i| !i.singleton) {
                    match first.get(&(item.type_id, item.flag_id)) {
                        Some(&dest) => ops.push((item.item_id, dest, item.quantity as i64)),
                        None => { first.insert((item.type_id, item.flag_id), item.item_id); }
                    }
                }
                apply_merges(tx, &sde, &reach, outbox, items, &ops, me, station).await
            })
        },
        |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}

/// Calls on the inventory manager (`invbroker` bound to a location).
pub async fn broker(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let char_id = c.session.int("charid").unwrap_or(0);
    Some(Ok(match call.method {
        "GetInventoryFromId" => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            if super::trade::is_trade_container(id) {
                return Some(Err(notify_error(messages::trade::TRADE_CONTAINER_RESERVED)));
            }
            match crate::dao::trades::reserved(&c.db, &[id]).await {
                Ok(ids) if !ids.is_empty() => return Some(Err(notify_error(messages::trade::TRADE_CONTAINER_RESERVED))),
                Err(error) => return Some(Err(db_err(error))),
                _ => {}
            }
            if super::exploration::container_locked(id) {
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
        "GetItemDescriptor" => super::item_descriptor().to_value(),
        "IsMyItemAtDockableLocation" => Value::Bool(true),
        // SplitStack(locationID, itemID, qty, ownerID): a new stack beside the old one.
        "SplitStack" => {
            let _lock = super::market::lock().await;
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
            let _lock = super::market::lock().await;
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

/// Calls on a bound inventory.
pub async fn inventory(c: &mut Client, call: &Call<'_>) -> CallResult {
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
        if super::trade::is_trade_container(*id) {
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
            movement::move_item(&super::identity::Identity::of_client(c), &c.db, &c.world, &c.sde, c.session.int("solarsystemid"), c.notify_tx.clone(), &container, item_id, call.args.get(1).and_then(Value::as_int), flag, qty, |n| super::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await.map(|id| id.map(Value::Int).unwrap_or(Value::None))
        }
        // MultiAdd(itemIDs, sourceID, flag=)
        "MultiAdd" => {
            let flag = call.kwargs.get("flag").and_then(|v| v.as_int()).map(|f| f as i32);
            for item_id in value_id_list(call.args.first()) {
                movement::move_item(&super::identity::Identity::of_client(c), &c.db, &c.world, &c.sde, c.session.int("solarsystemid"), c.notify_tx.clone(), &container, item_id, call.args.get(1).and_then(Value::as_int), flag, None, |n| super::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await?;
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

#[cfg(test)] mod tests;
