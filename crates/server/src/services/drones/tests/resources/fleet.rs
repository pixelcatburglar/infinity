use anyhow::{anyhow, ensure, Result};
use marshal::Value;
use crate::services::{Call, CallError};
use super::super::fixture::Fixture;

fn call_error(error: CallError) -> anyhow::Error {
    match error {
        CallError::Internal(error) => error,
        CallError::User { msg, dict } => anyhow!("fleet RPC: {msg}: {dict:?}"),
    }
}

pub(in crate::services) async fn form_fleet(f: &mut Fixture, other: i64, ship: i64) -> Result<i64> {
    let primary_type = f.world.ball(f.system, f.ship).await.ok_or_else(|| anyhow!("primary ship absent"))?.type_id;
    let other_ball = f.world.ball(f.system, ship).await.ok_or_else(|| anyhow!("other ship absent"))?;
    let other_type = other_ball.type_id;
    let corporation = crate::dao::characters::crimewatch::corporation(&f.db, other).await?.ok_or_else(|| anyhow!("other pilot absent"))?;
    let system = crate::dao::characters::crimewatch::solar_system(&f.db, other).await?;
    let kwargs = Value::None;
    let create = Call { service: "fleetObjectHandler", method: "CreateFleet", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::fleets::dispatch(&mut f.client, &create).await.ok_or_else(|| anyhow!("fleet service unavailable"))?.map_err(call_error)?;
    let ids: Vec<i64> = f.client.bound.values().filter_map(|(service, param)|
        (service == "fleetObjectHandler").then(|| param.as_int()).flatten()).collect();
    ensure!(ids.len() == 1, "expected one fleet binding");
    let id = ids[0];
    let params = Value::Int(id);
    let args = [Value::Int(i64::from(primary_type)), Value::None, Value::None];
    let call = Call { service: "fleetObjectHandler", method: "Init", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&params) };
    crate::services::fleets::dispatch(&mut f.client, &call).await.ok_or_else(|| anyhow!("fleet service unavailable"))?.map_err(call_error)?;

    let online = f.world.is_online(other);
    let original_target = f.world.chat_session(other);
    let target_session = original_target.clone().unwrap_or(crate::session::ChatSession {
        character_id: other, station_id: None, system_id: system, corporation_id: Some(corporation),
        alliance_id: (other_ball.alliance_id > 0).then_some(other_ball.alliance_id),
        faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    });
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    if !online { f.world.register_pilot(other, tx); }
    if original_target.is_none() { f.world.set_chat_session(other, Some(target_session.clone())); }

    let keys = ["charid", "shipid", "fleetid", "wingid", "squadid", "fleetrole",
        "corpid", "allianceid", "warfactionid", "solarsystemid2", "role", "corprole"];
    let saved: Vec<_> = keys.iter().map(|&key| (key, f.client.session.get(key).clone())).collect();
    let result: Result<()> = async {
        let args = [Value::Int(other), Value::None, Value::None, Value::None];
        let call = Call { service: "fleetObjectHandler", method: "Invite", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&params) };
        crate::services::fleets::dispatch(&mut f.client, &call).await.ok_or_else(|| anyhow!("fleet service unavailable"))?.map_err(call_error)?;
        let value = |id: Option<i64>| id.map(Value::Int).unwrap_or(Value::None);
        f.client.session.apply(vec![
            ("charid", Value::Int(other)), ("shipid", Value::Int(ship)), ("fleetid", Value::None),
            ("corpid", value(target_session.corporation_id)), ("allianceid", value(target_session.alliance_id)),
            ("warfactionid", value(target_session.faction_id)), ("solarsystemid2", value(target_session.system_id)),
            ("role", Value::Int(target_session.role)), ("corprole", Value::Int(target_session.corporation_role)),
        ]);
        let args = [Value::Int(i64::from(other_type))];
        let call = Call { service: "fleetObjectHandler", method: "AcceptInvite", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&params) };
        crate::services::fleets::dispatch(&mut f.client, &call).await.ok_or_else(|| anyhow!("fleet service unavailable"))?.map_err(call_error)?;
        Ok(())
    }.await;
    f.client.session.apply(saved);
    if online {
        let restored = original_target.map(|mut session| {
            if result.is_ok() { session.fleet_id = Some(id); }
            session
        });
        f.world.set_chat_session(other, restored);
    } else {
        f.world.unregister_pilot(other);
    }
    result?;
    ensure!(crate::services::fleets::same_fleet(f.owner, other), "fleet membership not established");
    Ok(id)
}
