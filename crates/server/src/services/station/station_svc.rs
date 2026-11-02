//! `stationSvc` NPC station and solar-system information from the startup SDE map.
//! Consumers include `eve/client/script/ui/shared/maps/mapSvc.py`,
//! `eve/client/script/ui/station/loyaltyPointStore/lpStoreSvc.py`,
//! `eve/client/script/ui/shared/info/infosvc.py`, `infoContentBase.py`, and
//! `eve/devtools/script/insider.py`. Names remain client-localized; player
//! stations, custom descriptions, and alliance sovereignty are not modelled.

use super::*;

use crate::{messages, sde::map::{SolarSystem, Station}, services::{keyval, notify_error}};

fn request_id(call: &Call<'_>) -> Result<i64, CallError> {
    if !match call.kwargs { Value::None => true, Value::Dict(items) => items.is_empty(), _ => false } {
        return Err(notify_error(messages::map::INVALID_MAP_REQUEST));
    }
    match call.args {
        [Value::Int(id)] if *id > 0 => Ok(*id),
        _ => Err(notify_error(messages::map::INVALID_MAP_REQUEST)),
    }
}

fn required(station: &Station, field: &str, value: Option<i64>) -> Result<Value, CallError> {
    value.map(Value::Int).ok_or_else(|| CallError::Internal(anyhow::anyhow!("station {} missing {field}", station.station_id)))
}

fn owner_row(station: &Station) -> CallResult {
    Ok(keyval([
        ("stationID", Value::Int(station.station_id)),
        ("solarSystemID", required(station, "solarSystemID", station.solar_system_id)?),
    ]))
}

fn station_row(station: &Station) -> CallResult {
    Ok(keyval([
        ("stationTypeID", required(station, "stationTypeID", station.type_id)?),
        ("ownerID", required(station, "ownerID", station.owner_id)?),
        ("operationID", required(station, "operationID", station.operation_id)?),
        ("solarSystemID", required(station, "solarSystemID", station.solar_system_id)?),
        ("constellationID", required(station, "constellationID", station.constellation_id)?),
        ("regionID", required(station, "regionID", station.region_id)?),
        ("description", Value::None),
    ]))
}

fn system_row(system: &SolarSystem) -> Value {
    keyval([("factionID", system.faction_id.map(Value::Int).unwrap_or(Value::None))])
}

fn stations_for_owner(c: &Client, call: &Call<'_>) -> CallResult {
    let id = request_id(call)?;
    let rows = c.sde.map.stations_by_owner(id).map(owner_row).collect::<Result<Vec<_>, _>>()?;
    Ok(Value::list(rows))
}

fn station(c: &Client, call: &Call<'_>) -> CallResult {
    let id = request_id(call)?;
    c.sde.map.station(id).map(station_row).unwrap_or(Ok(Value::None))
}

fn solar_system(c: &Client, call: &Call<'_>) -> CallResult {
    let id = request_id(call)?;
    c.sde.map.system(id).map(system_row).ok_or_else(|| notify_error(messages::map::INVALID_MAP_REQUEST))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let int = Value::Int;
    Some(Ok(match (call.service, call.method) {
        ("stationSvc", "GetStationItemBits") => {
            let station = c.session.int("stationid").unwrap_or(0);
            let Some(r) = c.sde.map.station(station as i32 as i64) else {
                return Some(Err(CallError::Internal(anyhow::anyhow!("station {station} not found"))));
            };
            Value::list([r.owner_id, Some(r.station_id), r.operation_id, r.type_id].into_iter().map(|v| v.map(int).unwrap_or(Value::None)))
        }
        ("stationSvc", "GetStationsForOwner") => return Some(stations_for_owner(c, call)),
        ("stationSvc", "GetStation") => return Some(station(c, call)),
        ("stationSvc", "GetSolarSystem") => return Some(solar_system(c, call)),
        _ => return None,
    }))
}

#[cfg(test)]
mod tests;
