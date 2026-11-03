//! Inventory stack merges, splits and trashing for
//! `eve/client/script/environment/invCache.py`. Uses `game.items`, trade
//! reservations, corporate custody and SDE data; staged trade items are not mutable.

use super::access::{corporate_custody, custody_allowed, may_take_corp_hangar, Container};
use super::notifications::{notify_corp_item_change, notify_item_change, notify_item_removed};
use super::reachability::{Endpoint, Reachability};
use crate::client::Client;
use crate::consts::{FLAG_CARGO, FLAG_CORP_DELIVERIES, FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::dao::items::{inventory as inventory_dao, loading::{Item, ItemQuery}, quantities::{self, QuantityChange}};
use crate::db::{executor::Executor, outbox::{dispatch_client, Outbox}};
use crate::messages;
use crate::sde::Sde;
use crate::services::{db_err, items::load_items, notify_error, CallError};
use marshal::Value;
use std::collections::BTreeMap;
/// Simulate transfers in request order, retaining the original snapshots for guarded writes.
pub(super) fn plan_merges(items: &[Item], ops: &[(i64, i64, i64)]) -> Result<BTreeMap<i64, QuantityChange>, CallError> {
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

pub(super) async fn mutation_corporate(ex: &(impl Executor + ?Sized), sde: &Sde, item: &Item, me: i64, station: Option<i64>) -> Result<bool, CallError> {
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

pub(super) async fn merge_operations(c: &mut Client, container: &Container, source_container: Option<i64>, ops: Vec<(i64, i64, i64)>) -> Result<(), CallError> {
    let _lock = super::super::market::lock().await;
    let me = c.session.int("charid").unwrap_or(0);
    let station = c.session.int("stationid");
    let ids: Vec<i64> = ops.iter().flat_map(|&(source, dest, _)| [source, dest]).collect();
    let snapshots = load_items(&c.db, &c.sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
    let endpoints: Vec<_> = snapshots.iter().map(|item| Endpoint { location: item.location_id, flag: item.flag_id }).collect();
    let reach = Reachability::capture(&super::super::identity::Identity::of_client(c), &c.db, &c.world, c.session.int("solarsystemid"), &endpoints).await?;
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
pub(super) async fn stack_all(c: &mut Client, container: &Container, flag: Option<i64>) -> Result<(), CallError> {
    let _lock = super::super::market::lock().await;
    let me = c.session.int("charid").unwrap_or(0);
    let station = c.session.int("stationid");
    if let Container::Item(id) = container {
        if super::super::offices::office_owner(&c.db, *id).await?.is_some_and(|corp| c.session.int("corpid") != Some(corp)) {
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
    let reach = Reachability::capture(&super::super::identity::Identity::of_client(c), &c.db, &c.world, c.session.int("solarsystemid"), &endpoints).await?;
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

pub(in crate::services) async fn split_stack(db: &(impl Executor + ?Sized), item: &Item, quantity: i32, owner: i64, location: i64, flag: i32, singleton: bool) -> Result<i64, CallError> {
    if !crate::dao::trades::reserved(db, &[item.item_id]).await.map_err(db_err)?.is_empty() {
        return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
    }
    inventory_dao::split(db, item, quantity, owner, location, flag, singleton).await.map_err(db_err)?
        .ok_or_else(|| notify_error(messages::STACK_CANNOT_BE_SPLIT))
}


/// Apply the same ownership and active-ship policy as TrashItems.
pub(crate) async fn trash_items(c: &mut Client, ids: &[i64]) -> Result<(), CallError> {
    let _lock = super::super::market::lock().await;
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
