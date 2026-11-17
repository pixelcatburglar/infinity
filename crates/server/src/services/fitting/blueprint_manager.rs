//! Blueprint inventory for blueprintManager and blueprintSvc.py. Uses game
//! blueprint/job rows and the shared physical asset ancestry classifier with SDE
//! station facts. Structures and unsupported asset-safety producers are not modelled.
use std::collections::BTreeMap;
use crate::services::dogma::info::wire_quantity;
use crate::services::{keyval, notify_error, user_error, Call, CallError, CallResult};
use crate::client::Client;
use infinity_core::{consts::CATEGORY_BLUEPRINT, messages};
use marshal::Value;

/// Server response limit advertised by blueprintManager.GetLimits.
const MAX_BLUEPRINT_RESULTS: i64 = 10_000;

async fn blueprints(c: &Client, owner: i64, item: Option<i64>, facility: Option<i64>) -> Result<(Vec<Value>, BTreeMap<Option<i64>, i64>), CallError> {
    let (mut assets, rows) = crate::services::universe::assets::Snapshot::blueprints(c, owner, item).await?;
    let mut values = Vec::new();
    let mut counts = BTreeMap::new();
    for row in rows {
        if !c.sde.types.get(row.type_id).is_some_and(|ty| ty.category_id == Some(CATEGORY_BLUEPRINT)) { continue; }
        if !assets.matches_blueprint(&row) { continue; }
        let Some(root) = assets.resolve(row.item_id) else { continue };
        if !assets.visible(root) || facility.is_some_and(|id| root.station != Some(id)) { continue; }
        let station = root.station.and_then(|id| c.sde.map.station(id));
        let location_type = assets.item_type(row.location_id).map(i64::from).or_else(|| c.sde.map.station(row.location_id).and_then(|station| station.type_id));
        let solar_system = station.and_then(|station| station.solar_system_id)
            .or_else(|| c.sde.map.system(root.location).map(|_| root.location));
        let facility = root.station;
        *counts.entry(facility).or_insert(0) += 1;
        values.push(keyval([
            ("itemID", Value::Int(row.item_id)),
            ("typeID", Value::Int(i64::from(row.type_id))),
            ("ownerID", Value::Int(row.owner_id)),
            ("locationID", Value::Int(row.location_id)),
            ("locationTypeID", location_type.map(Value::Int).unwrap_or(Value::None)),
            ("locationFlagID", Value::Int(i64::from(root.flag))),
            ("flagID", Value::Int(i64::from(row.flag_id))),
            ("facilityID", facility.map(Value::Int).unwrap_or(Value::None)),
            ("solarSystemID", solar_system.map(Value::Int).unwrap_or(Value::None)),
            ("quantity", Value::Int(wire_quantity(row.type_id, row.singleton, row.quantity))),
            ("runs", Value::Int(row.runs)),
            ("timeEfficiency", Value::Int(row.time_efficiency)),
            ("materialEfficiency", Value::Int(row.material_efficiency)),
            ("jobID", row.job_id.map(Value::Int).unwrap_or(Value::None)),
            ("isImpounded", Value::Bool(root.impounded)),
        ]));
    }
    Ok((values, counts))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let char_id = c.session.int("charid").unwrap_or(0);
    Some(match call.method {
        "GetLimits" => Ok(Value::dict([("maxBlueprintResults", Value::Int(MAX_BLUEPRINT_RESULTS))])),
        "GetBlueprintDataByOwner" => {
            let owner = call.args.first().and_then(Value::as_int).unwrap_or(char_id);
            let facility = match call.args.get(1) {
                None | Some(Value::None) => None,
                Some(value) => match value.as_int() {
                    Some(id) => Some(id),
                    None => return Some(Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION))),
                },
            };
            match blueprints(c, owner, None, facility).await {
                Ok((values, counts)) => Ok(Value::tuple([Value::List(values), Value::Dict(counts.into_iter()
                    .map(|(facility, count)| (facility.map(Value::Int).unwrap_or(Value::None), Value::Int(count))).collect())])),
                Err(error) => Err(error),
            }
        }
        "GetBlueprintData" => {
            let item = call.args.first().and_then(Value::as_int);
            match blueprints(c, char_id, item, None).await {
                Ok((values, _)) => values.into_iter().next().ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![])),
                Err(error) => Err(error),
            }
        }
        _ => return None,
    })
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
