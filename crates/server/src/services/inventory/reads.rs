//! Authorized `GetContainerContents` reads and `ValidateItemListCanBeOpened`
//! snapshot validation for `eve/client/script/environment/invCache.py`,
//! `fittingSvc.py` and `containerContentWindow.py`. Uses current `game.items`
//! ancestry, office custody, trade reservations, loaded charges, corporate roles
//! and SDE data. Structure inventories are unsupported.
//! Custom loaded-charge item rows apply display-only quantity projection;
//! stored amounts remain exact.

use std::collections::{HashMap, HashSet};
use marshal::{DbRowDescriptor, DbType, Value};
use infinity_core::{consts::{CARGO_CONTAINER_GROUPS, CATEGORY_SHIP, FLAG_CARGO, FLAG_CORP_DELIVERIES, FLAG_FLEET_HANGAR, FLAG_SHIP_HANGAR, GROUP_SOLAR_SYSTEM, GROUP_STATION, GROUP_WRECK}, messages};
use crate::{client::Client, dao::items::{charges, deliveries::LineageRow, loading::{Item, ItemQuery}}, services::{Call, CallError, CallResult, db_err, items::load_items, notify_error}};
use super::reachability::{Endpoint, Reachability};

fn refusal() -> CallError { notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION) }

fn broker_bind(c: &Client, call: &Call<'_>) -> Result<(), CallError> {
    c.session.int("charid").filter(|id| *id > 0).ok_or_else(refusal)?;
    let (location, group) = if let Some(station) = c.session.int("stationid").filter(|id| *id > 0) {
        (station, GROUP_STATION)
    } else {
        (c.session.int("solarsystemid").filter(|id| *id > 0).ok_or_else(refusal)?, GROUP_SOLAR_SYSTEM)
    };
    if !call.bound || !matches!(call.bind_params, Some(Value::Tuple(v)) if matches!(v.as_slice(), [Value::Int(id), Value::Int(g)] if *id == location && *g == group))
        || !matches!(call.kwargs, Value::None) && !matches!(call.kwargs, Value::Dict(v) if v.is_empty()) {
        return Err(refusal());
    }
    Ok(())
}

fn request(c: &Client, call: &Call<'_>) -> Result<(i64, i64, Option<i64>), CallError> {
    broker_bind(c, call)?;
    match call.args {
        [Value::Int(id), Value::Int(dockable)] if *id > 0 && *dockable > 0 => Ok((*id, *dockable, None)),
        [Value::Int(id), Value::Int(dockable), system] if *id > 0 && *dockable > 0 => {
            let system = match system { Value::None => None, Value::Int(n) if *n > 0 => Some(*n), _ => return Err(refusal()) };
            Ok((*id, *dockable, system))
        }
        _ => Err(refusal()),
    }
}

fn charge_row(hull: &Item, charge: &charges::Charge, group: i64, category: i64) -> Result<Value, CallError> {
    let mut descriptor = crate::services::item_descriptor();
    descriptor.columns[0].1 = DbType::Str;
    let flag = i16::try_from(charge.flag_id).map_err(|e| CallError::Internal(e.into()))?;
    Ok(descriptor.row(&[
        Value::tuple([Value::Int(hull.item_id), Value::Int(i64::from(charge.flag_id)), Value::Int(i64::from(charge.type_id))]),
        Value::Int(i64::from(charge.type_id)), Value::Int(hull.owner_id), Value::Int(hull.item_id),
        Value::Int(i64::from(flag)), Value::Int(crate::services::dogma::info::wire_quantity(charge.type_id, false, charge.quantity)),
        Value::Int(group), Value::Int(category), Value::None,
    ]))
}

/// Walk exact parent edges rather than treating the recursive SQL result as proof
/// of a complete, acyclic chain. Office custody is attached to its child edge.
fn resolve<'a>(c: &Client, root: &Item, rows: &'a [LineageRow]) -> Option<(i64, Option<i64>, Option<(i64, i64, i32)>, Vec<&'a LineageRow>)> {
    let by_id: HashMap<_, _> = rows.iter().map(|r| (r.ancestor.item_id, r)).collect();
    let mut id = root.item_id;
    let mut visited = HashSet::new();
    let mut chain = Vec::new();
    let mut custody = None;
    loop {
        if !visited.insert(id) { return None; }
        let row = *by_id.get(&id)?;
        let a = &row.ancestor;
        if chain.is_empty() && (a.type_id != root.type_id || a.owner_id != root.owner_id || a.location_id != root.location_id || a.flag_id != root.flag_id || a.quantity != root.quantity || a.singleton != root.singleton) { return None; }
        chain.push(row);
        if let Some(office) = &row.office {
            let child = chain.get(chain.len().checked_sub(2)?)?;
            if c.sde.map.station(office.station_id).is_none() { return None; }
            custody = Some((office.corporation_id, office.station_id, child.ancestor.flag_id));
            let station = c.sde.map.station(office.station_id)?;
            return Some((office.station_id, station.solar_system_id, custody, chain));
        }
        let ty = c.sde.types.get(a.type_id)?;
        let ship = ty.category_id == Some(CATEGORY_SHIP);
        if !a.singleton || (ship && a.quantity != 1)
            || !(ship || ty.group_id.is_some_and(|g| g == GROUP_WRECK || CARGO_CONTAINER_GROUPS.contains(&g))) { return None; }
        if let Some(station) = c.sde.map.station(a.location_id) {
            if a.flag_id == FLAG_CORP_DELIVERIES && crate::services::corps::is_player_corp(a.owner_id) {
                custody = Some((a.owner_id, station.station_id, a.flag_id));
            }
            return Some((station.station_id, station.solar_system_id, custody, chain));
        }
        if c.sde.map.system(a.location_id).is_some() { return Some((a.location_id, Some(a.location_id), custody, chain)); }
        id = a.location_id;
    }
}

pub(super) async fn contents(c: &Client, call: &Call<'_>) -> CallResult {
    let (id, dockable, system) = request(c, call)?;
    let me = c.session.int("charid").filter(|id| *id > 0).ok_or_else(refusal)?;
    if crate::services::trade::is_trade_container(id) || crate::services::exploration::container_locked(id) { return Err(refusal()); }
    let root = load_items(&c.db, &c.sde, ItemQuery::Id(id)).await.map_err(db_err)?.pop().ok_or_else(refusal)?;
    let ship = root.category_id == i64::from(CATEGORY_SHIP);
    if !root.singleton || (ship && root.quantity != 1) || !(ship || root.group_id == i64::from(GROUP_WRECK) || CARGO_CONTAINER_GROUPS.iter().any(|g| i64::from(*g) == root.group_id)) { return Err(refusal()); }
    let local = c.session.int("stationid") == Some(dockable) || (c.session.int("stationid").is_none() && c.session.int("solarsystemid") == Some(dockable));
    let flags = [FLAG_CARGO, FLAG_FLEET_HANGAR, FLAG_SHIP_HANGAR];
    let endpoints: Vec<_> = flags.iter().map(|&flag| Endpoint { location: id, flag }).collect();
    let checked = if local {
        let identity = crate::services::identity::Identity::of_client(c);
        let reach = Reachability::capture(&identity, &c.db, &c.world, c.session.int("solarsystemid"), &endpoints).await?;
        Some(reach.inspect_many(&c.db, &c.sde, &endpoints).await?)
    } else { None };
    let remote_rows;
    let rows = if let Some(checked) = &checked { checked.full_lineage(id).ok_or_else(refusal)? } else {
        remote_rows = crate::dao::items::deliveries::ancestry_many(&c.db, &[id]).await.map_err(db_err)?;
        &remote_rows
    };
    let (actual, actual_system, custody, chain) = resolve(c, &root, rows).ok_or_else(refusal)?;
    if actual != dockable || system.is_some_and(|system| Some(system) != actual_system) || chain.iter().any(|row| crate::services::trade::is_trade_container(row.ancestor.item_id) || crate::services::exploration::container_locked(row.ancestor.item_id)) { return Err(refusal()); }
    let corp_allowed = if let Some((corp, station, flag)) = custody {
        let role = super::access::corp_hangar_role(flag, false).ok_or_else(refusal)?;
        crate::services::corps::permissions::load(&c.db, me).await?.is_some_and(|a| crate::services::corps::permissions::allows_at(&a, corp, station, role))
    } else { false };
    if custody.is_some() && !corp_allowed { return Err(refusal()); }
    let privately_owned = chain.iter().all(|r| r.ancestor.owner_id == me || custody.is_some_and(|(corp, _, _)| r.ancestor.owner_id == corp));
    let allowed = checked.as_ref().map(|v| v.admitted.as_slice());
    let nested_shared = root.owner_id == me && allowed.is_some_and(|a| a[0]);
    let full = (privately_owned && (root.owner_id == me || corp_allowed)) || nested_shared;
    if !full && !allowed.is_some_and(|a| a.iter().any(|v| *v)) { return Err(refusal()); }
    let items = load_items(&c.db, &c.sde, ItemQuery::Location(id)).await.map_err(db_err)?;
    let mut ids = Vec::with_capacity(items.len() + 1);
    ids.push(id);
    ids.extend(items.iter().map(|item| item.item_id));
    let reserved: HashSet<_> = crate::dao::trades::reserved(&c.db, &ids).await.map_err(db_err)?.into_iter().collect();
    if reserved.contains(&id) { return Err(refusal()); }
    let mut result: Vec<_> = items.into_iter().filter(|item| {
        !reserved.contains(&item.item_id) && !crate::services::trade::is_trade_container(item.item_id)
            && if full { item.owner_id == root.owner_id || item.owner_id == me }
            else { flags.iter().position(|flag| *flag == item.flag_id).is_some_and(|index| allowed.is_some_and(|a| a[index])) }
    }).map(|item| item.row()).collect();
    if full && ship {
        for charge in charges::positive(&c.db, id).await.map_err(db_err)? {
            let ty = c.sde.types.get(charge.type_id).ok_or_else(refusal)?;
            result.push(charge_row(&root, &charge, i64::from(ty.group_id.ok_or_else(refusal)?), i64::from(ty.category_id.ok_or_else(refusal)?))?);
        }
    }
    Ok(Value::List(result))
}

pub(super) async fn validate(c: &Client, call: &Call<'_>) -> CallResult {
    broker_bind(c, call)?;
    let [Value::List(rows) | Value::Tuple(rows)] = call.args else { return Err(refusal()) };
    let canonical = crate::services::item_descriptor();
    let mut snapshots: HashMap<i64, (i64, i64, i64, i64)> = HashMap::new();
    let mut order = Vec::new();
    let mut inconsistent = HashSet::new();
    for row in rows {
        let Value::DbRow { descriptor, .. } = row else { return Err(refusal()) };
        let descriptor = DbRowDescriptor::from_value(descriptor).filter(|d| *d == canonical).ok_or_else(refusal)?;
        let integer = |name| match descriptor.get(row, name) {
            Some(Value::Int(n)) => Ok(n),
            _ => Err(refusal()),
        };
        let id = integer("itemID")?;
        if id <= 0 { return Err(refusal()); }
        let snapshot = (integer("typeID")?, integer("ownerID")?, integer("locationID")?, integer("flagID")?);
        if let Some(previous) = snapshots.get(&id) {
            if *previous != snapshot { inconsistent.insert(id); }
        } else {
            snapshots.insert(id, snapshot);
            order.push(id);
        }
    }
    if order.is_empty() { return Ok(Value::List(Vec::new())); }
    let items = load_items(&c.db, &c.sde, ItemQuery::Ids(&order)).await.map_err(db_err)?;
    let items: HashMap<_, _> = items.into_iter().map(|item| (item.item_id, item)).collect();
    let endpoints: Vec<_> = order.iter().map(|&location| Endpoint { location, flag: FLAG_CARGO }).collect();
    let identity = crate::services::identity::Identity::of_client(c);
    let reach = Reachability::capture(&identity, &c.db, &c.world, c.session.int("solarsystemid"), &endpoints).await?;
    let checked = reach.inspect_many(&c.db, &c.sde, &endpoints).await?;
    let reserved: HashSet<_> = crate::dao::trades::reserved(&c.db, &order).await.map_err(db_err)?.into_iter().collect();
    let me = c.session.int("charid").ok_or_else(refusal)?;
    let mut resolved = HashMap::new();
    let mut needs_authority = false;
    for &id in &order {
        if let Some((_, _, custody, chain)) = items.get(&id).and_then(|item|
            checked.full_lineage(id).and_then(|rows| resolve(c, item, rows))) {
            needs_authority |= custody.is_some();
            resolved.insert(id, (custody, chain));
        }
    }
    let authority = if needs_authority {
        crate::services::corps::permissions::load(&c.db, me).await?
    } else { None };
    let mut invalid = Vec::new();
    for (index, &id) in order.iter().enumerate() {
        let valid = items.get(&id).is_some_and(|item| {
            if inconsistent.contains(&id) || reserved.contains(&id) || !checked.admitted[index]
                || !item.singleton || !CARGO_CONTAINER_GROUPS.iter().any(|g| i64::from(*g) == item.group_id)
                || snapshots[&id] != (i64::from(item.type_id), item.owner_id, item.location_id, i64::from(item.flag_id)) {
                return false;
            }
            let Some((custody, chain)) = resolved.get(&id) else { return false };
            if chain.iter().any(|r| crate::services::trade::is_trade_container(r.ancestor.item_id)
                || crate::services::exploration::container_locked(r.ancestor.item_id)) { return false; }
            if c.session.int("stationid").is_some() && custody.is_none()
                && chain.iter().any(|r| r.ancestor.owner_id != me) { return false; }
            if let Some((corp, station, flag)) = custody {
                let Some(role) = super::access::corp_hangar_role(*flag, false) else { return false };
                if !authority.as_ref().is_some_and(|a| crate::services::corps::permissions::allows_at(a, *corp, *station, role)) { return false; }
            }
            true
        });
        if !valid { invalid.push(Value::Int(id)); }
    }
    Ok(Value::List(invalid))
}
