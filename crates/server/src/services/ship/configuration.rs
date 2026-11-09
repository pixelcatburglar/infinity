//! Owner-bound ship bay settings for `eve/client/script/ui/services/shipConfigSvc.py`.
//! Configuration is stored in game items metadata; visibility is sampled from World.
//! Metadata does not itself authorize access to a bay or provide notifications.
use super::{Call, CallResult, Client, Value, db_err};
use infinity_core::consts::{CATEGORY_SHIP, FLAG_HANGAR, GROUP_SOLAR_SYSTEM, GROUP_STATION};
use crate::dao::items::{loading::{load_items, GameItem, ItemQuery}, ship_configuration::{self, Patch}};
use crate::services::{notify_error, inventory::bay_access};
use infinity_core::messages::ship as text;

const KEYS: [&str; 4] = ["FleetHangar_AllowFleetAccess", "FleetHangar_AllowCorpAccess", "SMB_AllowFleetAccess", "SMB_AllowCorpAccess"];

/// The shared physical-hull and local visibility check does not grant bay access.
pub(super) async fn visible_hull(c: &Client, ship: i64, character: i64) -> Result<Option<GameItem>, super::CallError> {
    let Some(item) = load_items(&c.db, ItemQuery::Id(ship)).await.map_err(db_err)?.pop() else { return Ok(None) };
    if !item.singleton || item.quantity != 1 || c.sde.types.get(item.type_id).and_then(|ty| ty.category_id) != Some(CATEGORY_SHIP) { return Ok(None); }
    let in_hangar = item.owner_id == character && c.session.int("stationid") == Some(item.location_id) && item.flag_id == FLAG_HANGAR;
    let in_space = if let Some(system) = c.session.int("solarsystemid").filter(|_| c.session.int("stationid").is_none()) {
        item.location_id == system && c.world.with(system, move |s| s.visible_to(character, ship)).await == Some(true)
    } else { false };
    Ok((in_hangar || in_space).then_some(item))
}

pub(super) async fn get(c: &Client, call: &Call<'_>) -> CallResult {
    let invalid = || notify_error(text::CONFIGURATION_INACCESSIBLE);
    let ship = match call.args.first() { Some(Value::Int(id)) if *id > 0 => *id, _ => return Err(invalid()) };
    let character = c.session.int("charid").filter(|id| *id > 0).ok_or_else(invalid)?;
    let item = visible_hull(c, ship, character).await?.ok_or_else(invalid)?;
    if item.owner_id != character {
        let affiliation = bay_access::affiliation(&c.db, character, item.owner_id).await?;
        if !affiliation.fleet && !affiliation.corporation { return Err(invalid()); }
    }
    let settings = ship_configuration::get(&c.db, ship).await.map_err(db_err)?.filter(|s| s.owner_id == item.owner_id).unwrap_or_default();
    Ok(Value::dict([(KEYS[0], Value::Bool(settings.fleet_hangar_fleet)), (KEYS[1], Value::Bool(settings.fleet_hangar_corp)),
        (KEYS[2], Value::Bool(settings.maintenance_fleet)), (KEYS[3], Value::Bool(settings.maintenance_corp))]))
}

fn parse(value: &Value) -> Option<Patch> {
    let Value::Dict(entries) = value else { return None };
    let mut patch = Patch::default();
    for (key, value) in entries {
        let Value::Bool(value) = value else { return None };
        let slot = match key.as_text().as_deref() {
            Some(k) if k == KEYS[0] => &mut patch.fleet_hangar_fleet,
            Some(k) if k == KEYS[1] => &mut patch.fleet_hangar_corp,
            Some(k) if k == KEYS[2] => &mut patch.maintenance_fleet,
            Some(k) if k == KEYS[3] => &mut patch.maintenance_corp,
            _ => return None,
        };
        if slot.replace(*value).is_some() { return None; }
    }
    Some(patch)
}

/// Validate the location moniker against the caller's current session.
pub(super) fn current_binding(c: &Client, call: &Call<'_>) -> Option<(Option<i64>, i64)> {
    let station = c.session.int("stationid");
    let system = if station.is_some() { c.session.int("solarsystemid2") } else { c.session.int("solarsystemid") }.filter(|id| *id > 0)?;
    let location = station.unwrap_or(system);
    let group = if station.is_some() { GROUP_STATION } else { GROUP_SOLAR_SYSTEM };
    if !call.bound || !matches!(call.bind_params, Some(Value::Tuple(parts)) if parts.len() == 2 && parts[0].as_int() == Some(location) && parts[1].as_int() == Some(group)) { return None; }
    Some((station, system))
}

pub(super) async fn configure(c: &Client, call: &Call<'_>) -> CallResult {
    let invalid = || notify_error(text::CONFIGURATION_INACCESSIBLE);
    let [value] = call.args else { return Err(invalid()) };
    let patch = parse(value).ok_or_else(invalid)?;
    let character = c.session.int("charid").filter(|id| *id > 0).ok_or_else(invalid)?;
    let ship = c.session.int("shipid").filter(|id| *id > 0).ok_or_else(invalid)?;
    let (station, system) = current_binding(c, call).ok_or_else(invalid)?;
    if station.is_none() && c.world.with(system, move |s| s.pilot_ball(character).is_some_and(|b| b.id == ship)).await != Some(true) { return Err(invalid()); }
    let flag = if station.is_some() { FLAG_HANGAR } else { 0 };
    let user = c.login.user_id;
    let sde = c.sde.clone();
    c.db.transaction(|tx, _: &mut crate::db::outbox::Outbox| {
        let sde = sde.clone();
        Box::pin(async move {
            let kind = ship_configuration::lock_selected(tx, character, user, ship, station, system, flag).await.map_err(db_err)?.ok_or_else(invalid)?;
            if sde.types.get(kind).and_then(|ty| ty.category_id) != Some(CATEGORY_SHIP) { return Err(invalid()); }
            if !ship_configuration::patch(tx, ship, character, &patch).await.map_err(db_err)? { return Err(invalid()); }
            Ok::<_, super::CallError>(Value::None)
        })
    }, |value, _| async move { Ok(value) }).await
}
