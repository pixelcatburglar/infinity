//! Character blueprint inventory, mirroring blueprintManager and blueprintSvc.py.
//! Uses game blueprints and items, industry jobs and SDE station data.
use crate::services::dogma::info::wire_quantity;
use crate::services::{db_err, keyval, user_error, Call, CallError, CallResult};
use crate::dao::items::fitting as fitting_dao;
use crate::client::Client;
use infinity_core::consts::FLAG_HANGAR;
use infinity_core::messages;
use marshal::Value;
/// SDE invCategories: Blueprint.
const CATEGORY_BLUEPRINT: i64 = 9;
/// Server response limit advertised by blueprintManager.GetLimits.
const MAX_BLUEPRINT_RESULTS: i64 = 10_000;

/// Blueprint items of `owner` as industryCommon.BlueprintInstance data. Originals are
/// singletons (quantity -1, runs -1) or stacks carrying their quantity; copies are
/// singletons of quantity -2. Research state comes from game.blueprints, and a blueprint
/// in (a container in) a station has that station as its facility.
async fn blueprints(c: &Client, owner: i64, item: Option<i64>) -> Result<Vec<Value>, CallError> {
    let rows = fitting_dao::blueprints(&c.db, owner, item).await.map_err(db_err)?;
    Ok(rows
        .iter()
        .filter(|r| c.sde.types.get(r.type_id).is_some_and(|t| t.category_id == Some(CATEGORY_BLUEPRINT as i32)))
        .map(|r| {
            let station = c.sde.map.station(r.location_parent_id.unwrap_or(r.location_id));
            let location_type = r.location_type_id.map(i64::from).or_else(|| station.and_then(|s| s.type_id));
            let solar_system = station.and_then(|s| s.solar_system_id).or(r.location_parent_id).or(r.character_system_id);
            let singleton = r.singleton;
            let stored = r.quantity as i64;
            let quantity = wire_quantity(r.type_id, singleton, stored);
            keyval([
                ("itemID", Value::Int(r.item_id)),
                ("typeID", Value::Int(r.type_id as i64)),
                ("ownerID", Value::Int(r.owner_id)),
                ("locationID", Value::Int(r.location_id)),
                ("locationTypeID", location_type.map(Value::Int).unwrap_or(Value::None)),
                ("locationFlagID", Value::Int(FLAG_HANGAR as i64)),
                ("flagID", Value::Int(r.flag_id as i64)),
                ("facilityID", station.map(|s| Value::Int(s.station_id)).unwrap_or(Value::None)),
                ("solarSystemID", solar_system.map(Value::Int).unwrap_or(Value::None)),
                ("quantity", Value::Int(quantity)),
                ("runs", Value::Int(r.runs)),
                ("timeEfficiency", Value::Int(r.time_efficiency)),
                ("materialEfficiency", Value::Int(r.material_efficiency)),
                ("jobID", r.job_id.map(Value::Int).unwrap_or(Value::None)),
                ("isImpounded", Value::Bool(false)),
            ])
        })
        .collect())
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let char_id = c.session.int("charid").unwrap_or(0);
    Some(match call.method {
        "GetLimits" => Ok(Value::dict([("maxBlueprintResults", Value::Int(MAX_BLUEPRINT_RESULTS))])),
        // GetBlueprintDataByOwner(ownerID, facilityID) -> (blueprints, {facilityID: count})
        "GetBlueprintDataByOwner" => {
            let owner = call.args.first().and_then(|v| v.as_int()).unwrap_or(char_id);
            match blueprints(c, owner, None).await {
                Ok(bps) => {
                    let count = bps.len() as i64;
                    Ok(Value::tuple([Value::List(bps), Value::Dict(vec![(Value::None, Value::Int(count))])]))
                }
                Err(e) => Err(e),
            }
        }
        "GetBlueprintData" => {
            let item = call.args.first().and_then(|v| v.as_int());
            match blueprints(c, char_id, item).await {
                Ok(bps) => bps.into_iter().next().ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![])),
                Err(e) => Err(e),
            }
        }
        _ => return None,
    })
}
