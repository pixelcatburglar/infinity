//! Empty planetary compatibility for `planetMgr`.
//! Consumers are `eve/client/script/environment/planetSvc.py` and
//! `eve/client/script/environment/planet/clientPlanet.py`. Planet metadata comes
//! from the startup SDE map; colonies, resources and planetary gameplay are not modelled.

use super::{keyval, notify_error, Call, CallResult};
use infinity_core::{consts::GROUP_PLANET, messages};
use crate::{client::Client, sde::map::Celestial};
use marshal::Value;

fn planet_info(planet: &Celestial) -> CallResult {
    let (Some(system), Some(kind), Some(radius), Some(index)) =
        (planet.solar_system_id, planet.type_id, planet.radius, planet.celestial_index)
    else { return Err(notify_error(messages::planetary::INVALID_PLANET)); };
    Ok(keyval([
        ("solarSystemID", Value::Int(system)),
        ("planetTypeID", Value::Int(kind)),
        ("radius", Value::Float(radius)),
        ("celestialIndex", Value::Int(index)),
    ]))
}

pub(super) fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    handle(call, |id| c.sde.map.celestial(id))
}

fn handle<'a>(call: &Call<'_>, lookup: impl FnOnce(i64) -> Option<&'a Celestial>) -> Option<CallResult> {
    if call.service != "planetMgr" { return None; }
    match call.method {
        "UserAbandonPlanet" | "UserUpdateNetwork" | "GMGetLocalDistributionReport"
        | "UserLaunchCommodities" | "UserTransferCommodities" | "GetProgramResultInfo"
        | "GMForceInstallProgram" | "GMConvertCommandCenter" | "GMAddCommodity"
        | "GMGetSynchedServerState" | "GMRunDepletionSim" | "GMGetCompleteResource"
        | "GMCreateNuggetLayer" => return Some(Err(notify_error(messages::planetary::UNAVAILABLE))),
        "GetPlanetsForChar" | "GetMyLaunchesDetails" => return Some(Ok(Value::list([]))),
        "GetFullNetworkForOwner" | "GetCommandPinsForPlanet" | "GetExtractorsForPlanet"
        | "GetPlanetInfo" | "GetPlanetResourceInfo" | "GetResourceData" => {},
        _ => return None,
    }
    let result = (|| {
        let id = match (call.bound, call.bind_params) {
            (true, Some(Value::Int(id))) => *id,
            _ => return Err(notify_error(messages::planetary::INVALID_PLANET)),
        };
        let planet = lookup(id).filter(|p| p.group_id == Some(i64::from(GROUP_PLANET)))
            .ok_or_else(|| notify_error(messages::planetary::INVALID_PLANET))?;
        if matches!(call.method, "GetFullNetworkForOwner" | "GetCommandPinsForPlanet" | "GetExtractorsForPlanet")
            && !matches!(call.args.first(), Some(Value::Int(value)) if *value == id) {
            return Err(notify_error(messages::planetary::INVALID_PLANET));
        }
        match call.method {
            "GetFullNetworkForOwner" => Ok(Value::tuple([Value::list([]), Value::list([])])),
            "GetCommandPinsForPlanet" | "GetPlanetResourceInfo" => Ok(Value::Dict(vec![])),
            "GetExtractorsForPlanet" => Ok(Value::list([])),
            "GetPlanetInfo" => planet_info(planet),
            "GetResourceData" => Ok(keyval([("data", Value::None)])),
            _ => unreachable!(),
        }
    })();
    Some(result)
}

#[cfg(test)]
mod tests;
