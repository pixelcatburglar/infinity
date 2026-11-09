//! Native ship.Drop adapter for sovereignty Hub deployment.
//! `eveMisc.LaunchFromShip` consumes a per-source sequence of IDs or user errors.
//! The sovereignty kernel owns inventory, authorization and publication; other item
//! types are refused by that kernel rather than launched as containers here.
use marshal::Value;
use crate::client::Client;
use infinity_core::consts::GROUP_SOLAR_SYSTEM;
use crate::services::{Call, CallResult, identity::Identity, item_launch, notify_error, sovereignty::{Resources, deploy::{self, DeployRequest}}};

pub(super) async fn drop_items(c: &mut Client, call: &Call<'_>) -> CallResult {
    let invalid = || notify_error(infinity_core::messages::STRUCTURE_OPERATION_INVALID);
    if !matches!(call.kwargs, Value::None) && !matches!(call.kwargs, Value::Dict(v) if v.is_empty()) { return Err(invalid()); }
    let (requests, behalf) = item_launch::parse(call.args).map_err(|()| invalid())?;
    let system = c.session.int("solarsystemid").ok_or_else(invalid)?;
    if !call.bound || c.session.int("stationid").is_some() || !matches!(call.bind_params, Some(Value::Tuple(v)) if matches!(v.as_slice(), [Value::Int(id), Value::Int(group)] if *id == system && *group == GROUP_SOLAR_SYSTEM)) { return Err(invalid()); }
    let identity = Identity::of_client(c);
    let resources = Resources { db: c.db.clone(), sde: c.sde.clone(), world: c.world.clone() };
    let mut result = Vec::with_capacity(requests.len());
    for (id, quantity) in requests {
        let value = match deploy::deploy(resources.clone(), identity.clone(), DeployRequest { source_item_id: id, quantity, whose_behalf: behalf }).await {
            Ok(deployed) => Value::list([Value::Int(deployed.item_id)]),
            Err(error) => item_launch::error_value(error)?,
        };
        result.push((Value::Int(id), value));
    }
    Ok(Value::Dict(result))
}
