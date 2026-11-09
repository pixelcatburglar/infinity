//! Entity moniker commands used by the client drone menu. State is delivered by
//! Michelle bootstrap and notifications, not a remote state-query method. Mining
//! orders require a visible asteroid owned by the current system.

use std::collections::HashSet;
use marshal::Value;
use crate::client::Client;
use infinity_core::messages::{self, drones as text};
use crate::services::{Call, CallError, CallResult, notify_error};
use crate::space::{drone::DroneOrder, MODE_WARP};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Command { Order(DroneOrder), Abandon }

fn positive_id(value: &Value) -> Option<i64> {
    match value { Value::Int(id) if *id > 0 => Some(*id), _ => None }
}

fn parse(method: &str, args: &[Value]) -> Result<(Vec<i64>, Command), CallError> {
    let invalid = || notify_error(text::INVALID_COMMAND);
    let (list, command) = match (method, args) {
        ("CmdEngage", [ids, target]) => (ids, Command::Order(DroneOrder::Engage(positive_id(target).ok_or_else(invalid)?))),
        ("CmdMineRepeatedly", [ids, target]) => (ids, Command::Order(DroneOrder::Mine(positive_id(target).ok_or_else(invalid)?))),
        ("CmdAssist", [character, ids]) => (ids, Command::Order(DroneOrder::Assist(positive_id(character).ok_or_else(invalid)?))),
        ("CmdGuard", [character, ids]) => (ids, Command::Order(DroneOrder::Guard(positive_id(character).ok_or_else(invalid)?))),
        ("CmdReturnHome", [ids]) => (ids, Command::Order(DroneOrder::ReturnHome)),
        ("CmdReturnBay", [ids]) => (ids, Command::Order(DroneOrder::ReturnBay)),
        ("CmdAbandonDrone", [ids]) => (ids, Command::Abandon),
        _ => return Err(invalid()),
    };
    let Value::List(items) = list else { return Err(invalid()) };
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for item in items {
        let id = positive_id(item).ok_or_else(invalid)?;
        if seen.insert(id) { ids.push(id); }
    }
    Ok((ids, command))
}

fn record_error(errors: &mut Vec<(Value, Value)>, id: i64, error: CallError) -> Result<(), CallError> {
    match error {
        CallError::User { msg, dict } => {
            errors.push((Value::Int(id), Value::tuple([Value::str(msg), Value::Dict(dict.into_iter().map(|(key, value)| (Value::str(key), value)).collect())])));
            Ok(())
        }
        error => Err(error),
    }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    if !matches!(call.method, "CmdEngage" | "CmdMineRepeatedly" | "CmdAssist" | "CmdGuard" | "CmdReturnHome" | "CmdReturnBay" | "CmdAbandonDrone") {
        return Err(crate::services::effects::unsupported::refusal(None, call.method, 0));
    }
    let (ids, command) = parse(call.method, call.args)?;
    let (Some(owner), Some(ship), Some(system)) = (c.session.int("charid"), c.session.int("shipid"), c.session.int("solarsystemid")) else {
        return Err(notify_error(text::NOT_IN_SPACE));
    };
    if !call.bound || call.bind_params.and_then(positive_id) != Some(system) || c.session.int("stationid").is_some() {
        return Err(notify_error(text::NOT_IN_SPACE));
    }
    let live = c.world.with(system, move |s| s.ball(ship).is_some_and(|b| b.char_id == Some(owner) && b.mode != MODE_WARP && b.cloak == 0)).await
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
    if !live { return Err(notify_error(text::NOT_IN_SPACE)); }
    let mut errors = Vec::new();
    for id in ids {
        let result = execute(c, system, owner, ship, id, command).await;
        if let Err(error) = result { record_error(&mut errors, id, error)?; }
    }
    Ok(Value::Dict(errors))
}

async fn execute(c: &mut Client, system: i64, owner: i64, ship: i64, id: i64, command: Command) -> Result<(), CallError> {
    let (drone, mineable) = c.world.with(system, move |s| {
        let mineable = match command { Command::Order(DroneOrder::Mine(target)) => s.rock_type(target).is_some(), _ => true };
        (s.drone(id).filter(|_| s.ball(id).is_some()), mineable)
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
    let drone = drone.ok_or_else(|| notify_error(text::NOT_AVAILABLE))?;
    if drone.owner != owner { return Err(notify_error(text::NOT_AVAILABLE)); }
    if command == Command::Abandon { return super::inventory::abandon(c, id).await; }
    let Command::Order(order) = command else { unreachable!() };
    if drone.controller != Some(ship) && !(order == DroneOrder::ReturnBay && drone.controller.is_none()) {
        return Err(notify_error(text::NOT_CONTROLLED));
    }
    if matches!(order, DroneOrder::Engage(_) | DroneOrder::Mine(_) | DroneOrder::Assist(_) | DroneOrder::Guard(_)) && drone.spec.effect_id.is_none() {
        return Err(crate::services::effects::unsupported::refusal(None, "drone effect", drone.spec.type_id));
    }
    if let DroneOrder::Engage(target) | DroneOrder::Mine(target) = order {
        if !crate::services::combat::targeting_charges::targets_of(owner).contains(&target) {
            return Err(crate::services::user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
        }
        if !mineable {
            return Err(notify_error(text::NOT_AVAILABLE));
        }
    }
    if let DroneOrder::Assist(delegate) | DroneOrder::Guard(delegate) = order {
        if !crate::services::fleets::same_fleet(owner, delegate) { return Err(notify_error(text::DELEGATE_NOT_IN_FLEET)); }
        let sde = c.sde.clone();
        let capsule_group = sde.types.get(infinity_core::consts::TYPE_CAPSULE).and_then(|t| t.group_id);
        let valid = c.world.with(system, move |s| s.pilot_ball(delegate).is_some_and(|b| {
            sde.types.get(b.type_id).is_some_and(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
                && t.group_id.is_some() && capsule_group.is_some() && t.group_id != capsule_group)
                && b.mode != MODE_WARP && b.cloak == 0
        })).await
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
        if !valid { return Err(notify_error(text::NOT_AVAILABLE)); }
    }
    let accepted = c.world.with(system, move |s| s.drone_order(id, owner, ship, order)).await
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
    if !accepted { return Err(notify_error(text::NOT_AVAILABLE)); }
    Ok(())
}

#[cfg(test)] mod tests;
