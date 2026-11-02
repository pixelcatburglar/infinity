//! The ship client service; `ballparkCommon/docking.py` defines NPC undock channels
//! and initial velocity. Station graphics and celestial data come from the SDE;
//! ship locations persist in game tables. Upwell undocking is not modeled.
//! Successful undocking hands off a lifecycle event after the session changes.
use super::*;

async fn undock(c: &mut Client) -> CallResult {
    let (Some(station), Some(system), Some(ship)) = (c.session.int("stationid"), c.session.int("solarsystemid2"), c.session.int("shipid")) else {
        return Err(CallError::User { msg: messages::MSG_CANT_UNDOCK_NOT_IN_STATION.into(), dict: vec![] });
    };
    let st = required(c.sde.map.celestial(station), "station celestial")?;
    let spos: [f64; 3] = [required(st.x, "station x")?, required(st.y, "station y")?, required(st.z, "station z")?];
    let station_type = i32::try_from(required(st.type_id, "station type")?)
        .map_err(|_| CallError::Internal(anyhow::anyhow!("invalid SDE station type")))?;
    let mut ball = ship_ball(c, ship, spos).await?;
    let group = c.sde.types.get(ball.type_id).and_then(|ty| ty.group_id);
    undock_point::prepare(&mut ball, c.sde.graphic_locations.get(station_type), group, spos,
        c.sde.celestial_radius(st), crate::space::collision::enabled(), crate::services::rand01);
    let _lock = crate::services::market::lock().await;
    crate::services::trade::on_leave_locked(c).await?;
    item_dao::undock(&c.db, ship, system, ball.pos).await.map_err(db_err)?;
    character_dao::undock(&c.db, c.session.int("charid").unwrap_or(0), system).await.map_err(db_err)?;
    c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    info!(user = c.login.user_id, "ship {ship} undocked from {station} into {system}");
    c.change_session(vec![("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
    if let Some(char_id) = c.session.int("charid") {
        let mut outbox = Outbox::default();
        outbox.event(GameEvent::Undocked { char_id, ship_id: ship, station_id: station, system_id: system });
        dispatch_client((), outbox, c)?;
    }
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "Undock" => Some(undock(c).await),
        "LaunchDrones" => Some(crate::services::drones::inventory::launch::launch(c, call).await),
        "ScoopDrone" => Some(crate::services::drones::inventory::returning::scoop(c, call).await),
        _ => None,
    }
}
