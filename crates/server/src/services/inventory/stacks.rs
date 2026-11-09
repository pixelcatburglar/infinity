//! Inventory stack merges, splits and trashing for
//! `eve/client/script/environment/invCache.py`. Uses `game.items`, trade
//! reservations, corporate custody and SDE data; staged trade items are not mutable.

use super::access::{custody_allowed, may_take_corp_hangar, Container};
use super::notifications::{notify_corp_item_change, notify_item_change, notify_item_removed};
use super::reachability::{Endpoint, Reachability};
use crate::client::Client;
use infinity_core::consts::{FLAG_CARGO, FLAG_CORP_DELIVERIES, FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::dao::items::{inventory as inventory_dao, loading::{Item, ItemQuery}, quantities::{self, QuantityChange}};
use crate::db::{executor::Executor, outbox::{dispatch_client, Outbox}};
use infinity_core::messages;
use crate::sde::Sde;
use crate::services::{db_err, items::load_items, notify_error, CallError};
use marshal::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::destination::ParentSnapshot;

pub(super) async fn capture_parents(ex: &(impl Executor + ?Sized), sde: &Sde, endpoints: &[Endpoint]) -> Result<Vec<ParentSnapshot>, CallError> {
    let mut ids: Vec<_> = endpoints.iter().map(|endpoint| endpoint.location).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut parents = Vec::new();
    for id in ids {
        let Some(item) = load_items(ex, sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next() else { continue };
        if item.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP) || super::super::offices::office_owner(ex, id).await?.is_some() { continue; }
        let ancestry = crate::dao::items::deliveries::ancestry(ex, id).await.map_err(db_err)?;
        parents.push(ParentSnapshot { item, ancestry });
    }
    Ok(parents)
}

pub(super) async fn lock_parents<'tx>(
    ex: &'tx (impl Executor + ?Sized), sde: &Sde, parents: &[ParentSnapshot], items: &[Item], write_ids: &[i64],
) -> Result<crate::dao::items::parent_locks::LockedItems<'tx>, CallError> {
    use crate::dao::items::parent_locks::{LockRequest, LockStrength};
    let mut lineages = Vec::new();
    let mut requests: Vec<_> = write_ids.iter().map(|id| LockRequest { item_id: *id, strength: LockStrength::Update }).collect();
    for location in items.iter().map(|item| item.location_id).chain(parents.iter().map(|parent| parent.item.item_id)) {
        let ancestry = crate::dao::items::deliveries::ancestry(ex, location).await.map_err(db_err)?;
        requests.extend(super::destination::parent_requests(sde,
            ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?);
        lineages.push((location, ancestry));
    }
    for parent in parents {
        if super::hold_storage::is_ordinary_container(sde, parent.item.type_id) {
            requests.push(LockRequest { item_id: parent.item.item_id, strength: LockStrength::Update });
        }
    }
    let locks = super::destination::lock_items(ex, sde, &requests, &[]).await?;
    for (location, ancestry) in lineages {
        if crate::dao::items::deliveries::ancestry(ex, location).await.map_err(db_err)? != ancestry {
            return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
    }
    super::destination::ParentGuard::revalidate(&locks, parents)?;
    Ok(locks)
}


fn same_snapshot(before: &[Item], current: &[Item]) -> bool {
    if before.len() != current.len() { return false; }
    let by_id: std::collections::HashMap<_, _> = before.iter().map(|item| (item.item_id, item)).collect();
    current.iter().all(|item| by_id.get(&item.item_id).is_some_and(|old|
        old.owner_id == item.owner_id && old.location_id == item.location_id && old.flag_id == item.flag_id
            && old.type_id == item.type_id && old.singleton == item.singleton && old.quantity == item.quantity))
}

/// Simulate transfers in request order, retaining the original snapshots for guarded writes.
pub(super) fn plan_merges(items: &[Item], ops: &[(i64, i64, i64)]) -> Result<BTreeMap<i64, QuantityChange>, CallError> {
    let mut states: BTreeMap<i64, (&Item, Option<i64>)> = items.iter().map(|item| (item.item_id, (item, Some(item.quantity)))).collect();
    for &(source, dest, requested) in ops {
        if source == dest { continue; }
        let (Some(&(src, Some(src_qty))), Some(&(dst, Some(dst_qty)))) = (states.get(&source), states.get(&dest)) else { continue };
        if src.type_id != dst.type_id || src.singleton || dst.singleton || src_qty <= 0 { continue; }
        if (src.owner_id, src.location_id, src.flag_id) != (dst.owner_id, dst.location_id, dst.flag_id) { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
        let take = i128::from(requested.clamp(1, src_qty)).min(i128::from(infinity_core::consts::MAX_STACK_QUANTITY) - i128::from(dst_qty)) as i64;
        let total = dst_qty + take;
        states.get_mut(&source).unwrap().1 = if take == src_qty { None } else { Some(src_qty - take) };
        states.get_mut(&dest).unwrap().1 = Some(total);
    }
    Ok(states.into_iter().filter_map(|(id, (item, quantity))| {
        (quantity != Some(item.quantity)).then(|| (id, QuantityChange::new(item, quantity)))
    }).collect())
}

pub(super) async fn mutation_corporate(ex: &(impl Executor + ?Sized), sde: &Sde, item: &Item, me: i64, station: Option<i64>, shared: bool) -> Result<bool, CallError> {
    if shared && station.is_none() { return Ok(false); }
    if let Some(allowed) = custody_allowed(ex, sde, item.item_id, me, station, true).await? {
        if !allowed { return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE)); }
        return Ok(true);
    }
    if item.owner_id != me && !shared {
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    }
    Ok(false)
}

async fn apply_merges<X: Send>(db: &(impl Executor + ?Sized), sde: &Arc<Sde>, checked: &super::reachability::CheckedEndpoints, audience: &super::notifications::ResolvedAudience, outbox: &mut Outbox<X>, items: Vec<Item>, ops: &[(i64, i64, i64)], me: i64, station: Option<i64>) -> Result<(), CallError> {
    let mut corporate = Vec::new();
    for item in &items {
        let endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
        let shared = checked.lineage(endpoint.location).is_some_and(|rows| super::movement::shared_lineage(sde, endpoint, rows.len(), |id| rows.get(&id)));
        if mutation_corporate(db, sde, item, me, station, shared).await? { corporate.push(item.item_id); }
    }
    let changes = plan_merges(&items, ops)?;
    let mut holds = BTreeMap::<(i64, i32), (Vec<super::hold_storage::Addition>, Vec<i64>)>::new();
    for item in items.iter().filter(|item| changes.contains_key(&item.item_id)) {
        let entry = holds.entry((item.location_id, item.flag_id)).or_default();
        entry.1.push(item.item_id);
        if let Some(quantity) = changes[&item.item_id].quantity {
            entry.0.push(super::hold_storage::Addition { type_id: item.type_id, singleton: item.singleton, quantity: i64::from(quantity) });
        }
    }
    for ((location, flag), (additions, excluded)) in holds {
        let capacity_owner = load_items(db, sde, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter()
            .next().map_or(me, |row| row.owner_id);
        super::hold_storage::check_destination(db, sde, capacity_owner, location, flag, &additions,
            super::hold_storage::Replaced { items: &excluded, reloads: &[] }, None).await?;
    }
    if !quantities::apply(db, &changes).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY)); }
    for item in items {
        if let Some(change) = changes.get(&item.item_id) {
            let updated = if let Some(quantity) = change.quantity { Item { quantity, ..item.clone() } } else { Item { location_id: LOCATION_JUNKYARD, ..item.clone() } };
            let key = if change.quantity.is_some() { IX_STACK_SIZE } else { IX_LOCATION_ID };
            let old = if change.quantity.is_some() { item.quantity as i64 } else { item.location_id };
            audience.item_change(outbox, &item, &updated, vec![(key, Value::Int(old))], me);
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
    let audience = super::notifications::Audience::capture(&super::super::identity::Identity::of_client(c), &c.db, &c.world, c.session.int("solarsystemid"), &endpoints).await?;
    let parents = capture_parents(&c.db, &c.sde, &endpoints).await?;
    let container = container.clone();
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let ops = ops.clone();
            let sde = sde.clone();
            let container = container.clone();
            let reach = reach.clone();
            let audience = audience.clone();
            let snapshots = snapshots.clone();
            let parents = parents.clone();
            Box::pin(async move {
                let write_ids: Vec<_> = plan_merges(&snapshots, &ops)?.into_keys().collect();
                let _locks = lock_parents(tx, &sde, &parents, &snapshots, &write_ids).await?;
                let ids: Vec<i64> = ops.iter().flat_map(|&(source, dest, _)| [source, dest]).collect();
                if !crate::dao::trades::reserved(tx, &ids).await.map_err(db_err)?.is_empty() {
                    return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                }
                let items = load_items(tx, &sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
                if !same_snapshot(&snapshots, &items) { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
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
                let endpoints: Vec<_> = items.iter().map(|item| Endpoint { location: item.location_id, flag: item.flag_id }).collect();
                let checked = reach.inspect_many(tx, &sde, &endpoints).await?;
                if checked.admitted.iter().any(|allowed| !allowed) {
                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                let audience = audience.resolve(tx, &sde).await?;
                apply_merges(tx, &sde, &checked, &audience, outbox, items, &ops, me, station).await
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
    let audience = super::notifications::Audience::capture(&super::super::identity::Identity::of_client(c), &c.db, &c.world, c.session.int("solarsystemid"), &endpoints).await?;
    let parents = capture_parents(&c.db, &c.sde, &endpoints).await?;
    let container = container.clone();
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let container = container.clone();
            let sde = sde.clone();
            let reach = reach.clone();
            let audience = audience.clone();
            let snapshots = snapshots.clone();
            let parents = parents.clone();
            Box::pin(async move {
                let mut groups = BTreeMap::<(i64, i32, i32), usize>::new();
                for item in snapshots.iter().filter(|item| !item.singleton) {
                    *groups.entry((item.owner_id, item.type_id, item.flag_id)).or_default() += 1;
                }
                let write_ids: Vec<_> = snapshots.iter().filter(|item| !item.singleton
                    && groups[&(item.owner_id, item.type_id, item.flag_id)] > 1).map(|item| item.item_id).collect();
                let _locks = lock_parents(tx, &sde, &parents, &snapshots, &write_ids).await?;
                if let Container::Deliveries { station_id, owner_id } = &container {
                    if station != Some(*station_id) || !may_take_corp_hangar(tx, me, *owner_id, *station_id, FLAG_CORP_DELIVERIES).await? {
                        return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE));
                    }
                }
                let mut items = stack_items(tx, &sde, &container, flag).await?;
                if !same_snapshot(&snapshots, &items) { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
                let mut selected: Vec<_> = items.iter().map(|item| Endpoint { location: item.location_id, flag: item.flag_id }).collect();
                if flag.is_some() || items.is_empty() { selected.push(endpoint); }
                let mut checked = reach.inspect_many(tx, &sde, &selected).await?;
                if flag.is_some() || items.is_empty() {
                    if !checked.admitted.pop().unwrap_or(false) { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
                }
                if flag.is_some() && checked.admitted.iter().any(|allowed| !allowed) { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
                if !items.is_empty() && checked.admitted.iter().all(|allowed| !allowed) {
                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                let mut index = 0;
                items.retain(|_| { let keep = checked.admitted[index]; index += 1; keep });
                let ids: Vec<i64> = items.iter().map(|item| item.item_id).collect();
                if !crate::dao::trades::reserved(tx, &ids).await.map_err(db_err)?.is_empty() {
                    return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                }
                let mut first: std::collections::HashMap<(i64, i32, i32), (i64, i64)> = Default::default();
                let mut ops = Vec::new();
                for item in items.iter().filter(|i| !i.singleton) {
                    let key = (item.owner_id, item.type_id, item.flag_id);
                    if let Some((dest, quantity)) = first.get_mut(&key) {
                        if item.quantity <= 0 { continue; }
                        let take = i128::from(item.quantity).min(i128::from(infinity_core::consts::MAX_STACK_QUANTITY) - i128::from(*quantity)) as i64;
                        if take > 0 { ops.push((item.item_id, *dest, take)); }
                        *quantity += take;
                        if take < item.quantity { *dest = item.item_id; *quantity = item.quantity - take; }
                    } else {
                        first.insert(key, (item.item_id, item.quantity));
                    }
                }
                let audience = audience.resolve(tx, &sde).await?;
                apply_merges(tx, &sde, &checked, &audience, outbox, items, &ops, me, station).await
            })
        },
        |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}

pub(in crate::services) async fn split_stack(db: &(impl Executor + ?Sized), item: &Item, quantity: i64, owner: i64, location: i64, flag: i32, singleton: bool) -> Result<i64, CallError> {
    if !crate::dao::trades::reserved(db, &[item.item_id]).await.map_err(db_err)?.is_empty() {
        return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
    }
    inventory_dao::split(db, item, quantity, owner, location, flag, singleton).await.map_err(db_err)?
        .ok_or_else(|| notify_error(messages::STACK_CANNOT_BE_SPLIT))
}
