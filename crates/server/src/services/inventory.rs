//! invbroker: inventory managers (bound per station / solar system) hand out
//! bound inventories, which list and describe the items in a container
//! (eve/client/script/environment/invCache.py, invCacheContainer).
//! Item quantities are stored in `game.items`; static item data comes from the loaded SDE.
//! Merge plans are guarded against stale snapshots. Staged trade items and their
//! descendants cannot be moved, split, merged, trashed, renamed or accessed here.
//! Moves require an owned source, an authorized corporation office, or a nearby
//! positioned wreck or cargo container, and an owned or authorized destination.
//! Loot rights and suspect flags are not implemented.
//! Item capacity and contents are read from one game-item statement snapshot.
//! Successful inventory moves queue item-move events after their writes.
//! Departing online modules are offlined transactionally; bank and cycle effects
//! are dispatched after commit and running cycles finish normally.

use crate::dao::items::loading::{Item, ItemQuery};
use super::items::load_items;
use crate::dao::items::inventory as inventory_dao;
use crate::dao::items::quantities::{self, QuantityChange};
use super::{db_err, keyval, notify_error, value_id_list, Call, CallError, CallResult};
use crate::client::{short, Client};
use crate::consts::{ATTR_CAPACITY, CATEGORY_SHIP, FLAG_CARGO, FLAG_CORP_SAG1, FLAG_FITTING_FIRST, FLAG_FITTING_LAST, FLAG_HANGAR, FLAG_PILOT, FLAG_RIG_FIRST, FLAG_RIG_LAST, FLAG_SUBSYSTEM_FIRST, FLAG_SUBSYSTEM_LAST, IX_FLAG, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE, LOCATION_JUNKYARD, ROLE_DIRECTOR, TYPE_CHARACTER};
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_client, Outbox};
use crate::messages;
use crate::sde::Sde;
use marshal::Value;
use std::collections::BTreeMap;
use tracing::warn;

pub(super) mod gm;

/// const.containerHangar: "my hangar in the current station".
const CONTAINER_HANGAR: i64 = 10004;
/// Existing server policy for the highest corporation-hangar division index.
const MAX_CORP_HANGAR_DIVISION_INDEX: i32 = 6;
/// Internal bound-object name for a single inventory.
pub const INVENTORY: &str = "invInventory";

/// Which container a bound inventory lists.
#[derive(Clone)]
pub(crate) enum Container {
    /// The character's hangar in a station.
    Hangar { station_id: i64, owner_id: i64 },
    /// Any item acting as a container (ship, character, can...).
    Item(i64),
}

fn container_of(params: Option<&Value>) -> Option<Container> {
    let p = params?.items()?;
    match p {
        [Value::Int(station), Value::Int(CONTAINER_HANGAR), Value::Int(owner)] => {
            Some(Container::Hangar { station_id: *station, owner_id: *owner })
        }
        [Value::Int(item)] => Some(Container::Item(*item)),
        _ => None,
    }
}

fn kw_or_arg<'a>(call: &'a Call<'_>, name: &str, idx: usize) -> Option<&'a Value> {
    call.kwargs.get(name).or_else(|| call.args.get(idx)).filter(|v| !v.is_none())
}

async fn list(c: &Client, container: &Container, flags: Option<Vec<i64>>) -> Result<Vec<Item>, CallError> {
    let flags: Option<Vec<i32>> = flags.map(|f| f.into_iter().map(|x| x as i32).collect());
    match container {
        Container::Hangar { station_id, owner_id } => {
            let flags = flags.unwrap_or_else(|| vec![FLAG_HANGAR]);
            load_items(&c.db, &c.sde, ItemQuery::LocationOwnerFlags(*station_id, *owner_id, &flags)).await.map_err(db_err)
        }
        // Another corporation's hangars stay closed.
        Container::Item(item_id) if super::offices::office_owner(&c.db, *item_id).await?.is_some_and(|corp| c.session.int("corpid") != Some(corp)) => Ok(vec![]),
        Container::Item(item_id) => match flags {
            Some(flags) => load_items(&c.db, &c.sde, ItemQuery::LocationFlags(*item_id, &flags)).await.map_err(db_err),
            None => load_items(&c.db, &c.sde, ItemQuery::Location(*item_id)).await.map_err(db_err),
        },
    }
}

/// The container's own item row (`GetSelfInvItem`).
async fn self_item(c: &Client, container: &Container) -> Result<Value, CallError> {
    let id = match container {
        Container::Hangar { station_id, .. } => *station_id,
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

fn item_capacity(items: &[Item], id: i64, data: impl Fn(i32) -> (Option<f64>, Option<f64>, Option<f64>)) -> (f64, f64) {
    let Some(parent) = items.iter().find(|item| item.item_id == id) else { return (0.0, 0.0) };
    let (attribute, physical, _) = data(parent.type_id);
    // The former SQL COALESCE(sum(...), 0) returned positive zero for an empty sum.
    let used = items.iter().filter(|item| item.location_id == parent.item_id).filter_map(|item| {
        data(item.type_id).2.map(|volume| volume * f64::from(item.quantity.max(1)))
    }).fold(0.0_f64, |total, value| total + value);
    (attribute.or(physical).unwrap_or(0.0), used)
}

async fn capacity(c: &Client, container: &Container) -> Result<Value, CallError> {
    let (cap, used) = match container {
        Container::Hangar { .. } => (1e12, 0.0),
        Container::Item(id) => {
            let items = load_items(&c.db, &c.sde, ItemQuery::IdOrLocation(*id)).await.map_err(db_err)?;
            item_capacity(&items, *id, |type_id| {
                let data = c.sde.types.get(type_id).unwrap();
                (data.attribute(ATTR_CAPACITY), data.capacity, data.volume)
            })
        }
    };
    Ok(keyval([("capacity", Value::Float(cap)), ("used", Value::Float(used))]))
}

// ---------------------------------------------------------------- moving items

const FLAG_AUTOFIT: i32 = 0;
/// appConst.corpRoleHangarCanTake1; divisions 2-7 are the next bits.
const ROLE_HANGAR_CAN_TAKE_1: i64 = 8192;
/// appConst.py:1475, maxCargoContainerTransferDistance, in metres.
const MAX_CARGO_CONTAINER_TRANSFER_DISTANCE: f64 = 2500.0;

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
                let mut items = load_items(tx, &sde, ItemQuery::IdsOwner(&ids, char_id)).await.map_err(db_err)?;
                items.sort_by_key(|item| item.item_id);
                for item in items {
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


enum MoveEffect {
    Bank(super::combat::banks::BankStop),
    Online(super::effects::online::OnlineChanged),
}

impl From<super::effects::online::OnlineChanged> for MoveEffect {
    fn from(change: super::effects::online::OnlineChanged) -> Self { Self::Online(change) }
}

fn dispatch_move<T>(value: T, outbox: Outbox<MoveEffect>, c: &Client, target: i64) -> Result<T, CallError> {
    use crate::db::outbox::Effect;
    for entry in outbox.into_entries() {
        match entry {
            Effect::Operation(MoveEffect::Bank(stop)) => super::combat::banks::apply_stop(stop),
            Effect::Operation(MoveEffect::Online(change)) => super::effects::online::apply_online_changed(change),
            Effect::Notification(notification) => super::effects::online::dispatch_notification(gm::route_target(notification, target, c.session.int("charid")), c)?,
            Effect::Event(event) => crate::lua::on_event(event),
        }
    }
    Ok(value)
}


async fn authorized_source(c: &Client, item: &Item, source: Option<i64>, char_id: i64) -> Result<bool, CallError> {
    if source.is_some_and(|id| id != item.location_id) { return Ok(false); }
    if let Some(corp) = super::offices::office_owner(&c.db, item.location_id).await? {
        let (my_corp, roles) = super::corps::membership(&c.db, char_id).await?;
        let take = ROLE_HANGAR_CAN_TAKE_1 << (item.flag_id - FLAG_CORP_SAG1).clamp(0, MAX_CORP_HANGAR_DIVISION_INDEX);
        if my_corp != corp || roles & (take | ROLE_DIRECTOR) == 0 {
            return Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), Value::unicode(messages::NO_CORPORATION_HANGAR_TAKE_ROLE))] });
        }
        return Ok(true);
    }
    if item.owner_id == char_id { return Ok(true); }
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(false) };
    if c.session.int("stationid").is_some() { return Ok(false); }
    let Some(parent) = load_items(&c.db, &c.sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next() else { return Ok(false) };
    if parent.group_id != i64::from(crate::consts::GROUP_WRECK) && !i32::try_from(parent.group_id).ok().is_some_and(|id| crate::consts::CARGO_CONTAINER_GROUPS.contains(&id)) { return Ok(false); }
    let Some(position) = crate::dao::items::social::position(&c.db, parent.item_id).await.map_err(db_err)? else { return Ok(false) };
    if position.location_id != system { return Ok(false); }
    Ok(c.world.with(system, move |s| {
        let (ship, parent) = (s.ball(ship)?, s.ball(parent.item_id)?);
        Some(crate::space::dist(ship.position_at(s.now_ms()), parent.position_at(s.now_ms()))
            - ship.radius as f64 - parent.radius as f64 <= MAX_CARGO_CONTAINER_TRANSFER_DISTANCE)
    }).await.flatten().unwrap_or(false))
}

async fn authorized_destination(c: &Client, container: &Container, flag: Option<i32>, char_id: i64) -> Result<Option<(i64, i64, i32)>, CallError> {
    match container {
        Container::Hangar { station_id, owner_id } => {
            if *owner_id != char_id || c.session.int("stationid") != Some(*station_id) { return Ok(None); }
            Ok(Some((*station_id, *owner_id, flag.filter(|f| *f != FLAG_AUTOFIT).unwrap_or(FLAG_HANGAR))))
        }
        Container::Item(id) => {
            if let Some(corp) = super::offices::office_owner(&c.db, *id).await? {
                if c.session.int("corpid") != Some(corp) {
                    return Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), Value::unicode(messages::NOT_YOUR_CORPORATION_HANGAR))] });
                }
                let sag = FLAG_CORP_SAG1;
                return Ok(Some((*id, corp, flag.filter(|f| (sag..sag + 7).contains(f)).unwrap_or(sag))));
            }
            let Some(item) = load_items(&c.db, &c.sde, ItemQuery::Id(*id)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
            if item.owner_id != char_id { return Ok(None); }
            let default = if item.category_id == CATEGORY_SHIP as i64 { FLAG_CARGO } else { FLAG_HANGAR };
            Ok(Some((*id, char_id, flag.unwrap_or(default))))
        }
    }
}

/// Move `item_id` (all of it, or `qty` split off a stack) into `container` under `flag`.
/// Returns the ID of the item now at the destination.
pub(crate) async fn move_item(c: &mut Client, container: &Container, item_id: i64, source: Option<i64>, flag: Option<i32>, qty: Option<i64>) -> Result<Option<i64>, CallError> {
    let _lock = super::market::lock().await;
    let char_id = c.session.int("charid").unwrap_or(0);
    let Some(item) = load_items(&c.db, &c.sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
    let allowed_source = authorized_source(c, &item, source, char_id).await?;
    let destination = if allowed_source { authorized_destination(c, container, flag, char_id).await? } else { None };
    let Some((location, owner, flag)) = destination else {
        let destination = match container { Container::Hangar { station_id, .. } => *station_id, Container::Item(id) => *id };
        warn!(char_id, item_id, destination, "inventory move authorization refused");
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    };
    if Some(item_id) == c.session.int("shipid") && matches!(container, Container::Item(_)) {
        return Err(CallError::User { msg: messages::MSG_CAN_NOT_PLACE_ACTIVE_SHIP_IN_ANOTHER_HANGAR.into(), dict: vec![] });
    }
    let db = c.db.clone();
    let sde = c.sde.clone();
    let active_ship = c.session.int("shipid");
    let station = c.session.int("stationid");
    let policy = super::effects::online::OnlinePolicy::from_client(c)?;
    db.transaction(
        move |tx, outbox: &mut Outbox<MoveEffect>| {
            let item = item.clone();
            let sde = sde.clone();
            let policy = policy.clone();
            Box::pin(async move {
                let destination_fitting = is_fitting_flag(flag) || (flag == FLAG_AUTOFIT
                    && (item.category_id == i64::from(crate::consts::CATEGORY_MODULE) || item.category_id == i64::from(crate::consts::CATEGORY_SUBSYSTEM)));
                let source_fitting = is_fitting_flag(item.flag_id);
                let mut locks = Vec::new();
                if source_fitting { locks.push(item.location_id); }
                if destination_fitting { locks.push(location); }
                locks.sort_unstable();
                locks.dedup();
                for ship in locks {
                    let exists = crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?;
                    if ship == location && destination_fitting && !exists {
                        return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
                    }
                }
                if destination_fitting {
                    let hull = load_items(tx, &sde, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                    if hull.owner_id != owner || hull.category_id != i64::from(CATEGORY_SHIP) || !hull.singleton
                        || (active_ship != Some(location) && !(station == Some(hull.location_id) && hull.flag_id == FLAG_HANGAR)) {
                        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                    }
                }
                let flag = if flag == FLAG_AUTOFIT && destination_fitting {
                    super::effects::fitting::free_slot(tx, sde.clone(), owner, location, item.item_id, item.type_id).await?
                        .ok_or_else(|| notify_error(messages::fitting::SLOTS))?
                } else if flag == FLAG_AUTOFIT { FLAG_CARGO } else { flag };
                let take = if is_fitting_flag(flag) { Some(1) } else { qty }
                    .filter(|q| *q > 0 && !item.singleton && (*q as i32) < item.quantity);
                let departure = source_fitting && take.is_none() && (item.location_id != location || item.flag_id != flag || item.owner_id != owner);
                let leaving_fit = departure && (item.location_id != location || !is_fitting_flag(flag) || item.owner_id != owner);
                let online = leaving_fit && item.category_id == i64::from(crate::consts::CATEGORY_MODULE)
                    && crate::dao::items::dogma::online(tx, item.location_id).await.map_err(db_err)?.iter().any(|entry| entry.item_id == item.item_id);
                if online {
                    super::effects::online::set_online_core(tx, &sde, &policy, outbox, item.location_id, item.item_id, false).await?;
                }
                let before = if departure && !online { Some(super::combat::banks::snapshot(tx, item.location_id, &[item.item_id]).await?) } else { None };
                let master = if departure && !online { super::combat::targeting_charges::master_for_member(tx, item.location_id, item.item_id).await? } else { item.item_id };
                let moved_id = move_core(tx, &sde, outbox, &item, owner, location, flag, take).await?;
                if is_fitting_flag(flag) {
                    super::effects::fitting::validate_fit(tx, sde.clone(), owner, location, moved_id).await?;
                }
                if let Some(before) = before {
                    if super::combat::banks::remove_member(tx, item.location_id, item.item_id, master, false).await? {
                        for stop in super::combat::banks::changes(tx, item.location_id, before).await? { outbox.operation(MoveEffect::Bank(stop)); }
                        outbox.notify_client("OnWeaponBanksChanged", vec![Value::Int(item.location_id), super::combat::banks(tx, item.location_id).await?]);
                    }
                }
                if departure { super::effects::online::reconcile_core(tx, &sde, outbox, item.owner_id, item.location_id).await?; }
                if is_fitting_flag(flag) { super::effects::online::reconcile_core(tx, &sde, outbox, owner, location).await?; }
                if take.is_some() || item.location_id != location || item.flag_id != flag || item.owner_id != owner || (is_fitting_flag(flag) && !item.singleton) {
                    outbox.event(crate::lua::events::GameEvent::ItemsMoved {
                        char_id, item_id: moved_id, from_location_id: item.location_id,
                        to_location_id: location, quantity: take.map_or(item.quantity, |q| q as i32),
                    });
                }
                Ok(Some(moved_id))
            })
        },
        |value, outbox| async move { dispatch_move(value, outbox, c, char_id) },
    ).await
}

async fn move_core<X: Send>(
    tx: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>,
    item: &Item, owner: i64, location: i64, flag: i32, take: Option<i64>,
) -> Result<i64, CallError> {
    if !crate::dao::trades::reserved(tx, &[item.item_id, location]).await.map_err(db_err)?.is_empty() {
        return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
    }
    let item_id = item.item_id;
    let fitting = is_fitting_flag(flag);
    if let Some(q) = take {
        let id = split_stack(tx, item, q as i32, owner, location, flag, fitting).await?;
        notify_item_change(tx, sde, outbox, item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
        notify_item_change(tx, sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))]).await?;
        Ok(id)
    } else {
        let singleton = item.singleton || fitting;
        if !inventory_dao::move_item(tx, item, location, flag, owner, singleton).await.map_err(db_err)? {
            return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
        }
        let mut change = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))];
        if singleton != item.singleton {
            change.push((IX_SINGLETON, Value::Bool(item.singleton)));
        }
        notify_item_change(tx, sde, outbox, item_id, change).await?;
        Ok(item_id)
    }
}

/// Simulate transfers in request order, retaining the original snapshots for guarded writes.
fn plan_merges(items: &[Item], ops: &[(i64, i64, i64)]) -> Result<BTreeMap<i64, QuantityChange>, CallError> {
    let mut states: BTreeMap<i64, (&Item, Option<i32>)> = items.iter().map(|item| (item.item_id, (item, Some(item.quantity)))).collect();
    for &(source, dest, requested) in ops {
        if source == dest { continue; }
        let (Some(&(src, Some(src_qty))), Some(&(dst, Some(dst_qty)))) = (states.get(&source), states.get(&dest)) else { continue };
        if src.type_id != dst.type_id || src.singleton || dst.singleton || src_qty <= 0 { continue; }
        let take = requested.clamp(1, i64::from(src_qty)) as i32;
        let total = dst_qty.checked_add(take).ok_or_else(|| notify_error(messages::INSUFFICIENT_ITEM_QUANTITY))?;
        states.get_mut(&source).unwrap().1 = if take == src_qty { None } else { Some(src_qty - take) };
        states.get_mut(&dest).unwrap().1 = Some(total);
    }
    Ok(states.into_iter().filter_map(|(id, (item, quantity))| {
        (quantity != Some(item.quantity)).then(|| (id, QuantityChange::new(item, quantity)))
    }).collect())
}

async fn apply_merges<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, items: Vec<Item>, ops: &[(i64, i64, i64)]) -> Result<(), CallError> {
    let changes = plan_merges(&items, ops)?;
    if !quantities::apply(db, &changes).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY)); }
    for item in items {
        if let Some(change) = changes.get(&item.item_id) {
            if change.quantity.is_none() {
                notify_item_removed(outbox, item);
            } else {
                notify_item_change(db, sde, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
            }
        }
    }
    Ok(())
}

async fn merge_operations(c: &mut Client, ops: Vec<(i64, i64, i64)>) -> Result<(), CallError> {
    let _lock = super::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let ops = ops.clone();
            let sde = sde.clone();
            Box::pin(async move {
                let ids: Vec<i64> = ops.iter().flat_map(|&(source, dest, _)| [source, dest]).collect();
                if !crate::dao::trades::reserved(tx, &ids).await.map_err(db_err)?.is_empty() {
                    return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                }
                let items = load_items(tx, &sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
                apply_merges(tx, &sde, outbox, items, &ops).await
            })
        },
        |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}

/// `StackAll([flag])`: merge every unassembled stack of the same type in the container.
async fn stack_all(c: &mut Client, container: &Container, flag: Option<i64>) -> Result<(), CallError> {
    let _lock = super::market::lock().await;
    if let Container::Item(id) = container {
        if super::offices::office_owner(&c.db, *id).await?.is_some_and(|corp| c.session.int("corpid") != Some(corp)) {
            return Ok(());
        }
    }
    let container = container.clone();
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let container = container.clone();
            let sde = sde.clone();
            Box::pin(async move {
                let flags: Option<Vec<i32>> = flag.map(|f| vec![f as i32]);
                let items = match container {
                    Container::Hangar { station_id, owner_id } => {
                        let flags = flags.unwrap_or_else(|| vec![FLAG_HANGAR]);
                        load_items(tx, &sde, ItemQuery::LocationOwnerFlags(station_id, owner_id, &flags)).await.map_err(db_err)?
                    }
                    Container::Item(id) => match flags {
                        Some(flags) => load_items(tx, &sde, ItemQuery::LocationFlags(id, &flags)).await.map_err(db_err)?,
                        None => load_items(tx, &sde, ItemQuery::Location(id)).await.map_err(db_err)?,
                    },
                };
                let ids: Vec<i64> = items.iter().map(|item| item.item_id).collect();
                if !crate::dao::trades::reserved(tx, &ids).await.map_err(db_err)?.is_empty() {
                    return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                }
                let mut first: std::collections::HashMap<(i32, i32), i64> = Default::default();
                let mut ops = Vec::new();
                for item in items.iter().filter(|i| !i.singleton) {
                    match first.get(&(item.type_id, item.flag_id)) {
                        Some(&dest) => ops.push((item.item_id, dest, item.quantity as i64)),
                        None => { first.insert((item.type_id, item.flag_id), item.item_id); }
                    }
                }
                apply_merges(tx, &sde, outbox, items, &ops).await
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
            let r = db.transaction(
                move |tx, outbox: &mut Outbox| {
                    let item = item.clone();
                    let sde = sde.clone();
                    Box::pin(async move {
                        let (owner, location, flag) = (item.owner_id, item.location_id, item.flag_id);
                        let id = split_stack(tx, &item, qty, owner, location, flag, false).await?;
                        notify_item_change(tx, &sde, outbox, item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
                        notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
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
    if let Container::Item(id) = &container {
        if super::trade::is_trade_container(*id) {
            return Err(notify_error(messages::trade::TRADE_CONTAINER_RESERVED));
        }
        if !crate::dao::trades::reserved(&c.db, &[*id]).await.map_err(db_err)?.is_empty() {
            return Err(notify_error(messages::trade::TRADE_CONTAINER_RESERVED));
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
            move_item(c, &container, item_id, call.args.get(1).and_then(Value::as_int), flag, qty).await.map(|id| id.map(Value::Int).unwrap_or(Value::None))
        }
        // MultiAdd(itemIDs, sourceID, flag=)
        "MultiAdd" => {
            let flag = call.kwargs.get("flag").and_then(|v| v.as_int()).map(|f| f as i32);
            for item_id in value_id_list(call.args.first()) {
                move_item(c, &container, item_id, call.args.get(1).and_then(Value::as_int), flag, None).await?;
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
            merge_operations(c, ops).await?;
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
