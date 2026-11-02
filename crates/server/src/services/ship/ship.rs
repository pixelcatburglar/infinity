//! The ship client service; `ballparkCommon/docking.py` defines NPC undock channels
//! and initial velocity. Station graphics and celestial data come from the SDE;
//! ship locations persist in game tables. Upwell undocking is not modeled.
//! Successful undocking hands off a lifecycle event after the session changes.
use super::*;

/// The caller must hold `economy` until session updates and the lifecycle event are handled.
pub(crate) struct Undocked {
    pub change: SessionChange,
    pub economy: tokio::sync::MutexGuard<'static, ()>,
}

pub(crate) async fn undock(identity: &Identity, db: &crate::db::Db, world: &space::World,
    sde: &std::sync::Arc<crate::sde::Sde>, space_system: Option<i64>, alliance_id: Option<i64>) -> Result<Undocked, CallError> {
    let (Some(station), Some(system), Some(ship)) = (identity.station_id, identity.system_id, identity.ship_id) else {
        return Err(CallError::User { msg: messages::MSG_CANT_UNDOCK_NOT_IN_STATION.into(), dict: vec![] });
    };
    let st = required(sde.map.celestial(station), "station celestial")?;
    let spos: [f64; 3] = [required(st.x, "station x")?, required(st.y, "station y")?, required(st.z, "station z")?];
    let station_type = i32::try_from(required(st.type_id, "station type")?)
        .map_err(|_| CallError::Internal(anyhow::anyhow!("invalid SDE station type")))?;
    let mut ball = balls::selected(identity, db, world, sde, space_system, alliance_id, ship, spos).await?;
    let group = sde.types.get(ball.type_id).and_then(|ty| ty.group_id);
    undock_point::prepare(&mut ball, sde.graphic_locations.get(station_type), group, spos,
        sde.celestial_radius(st), crate::space::collision::enabled(), crate::services::rand01);
    let economy = crate::services::market::lock().await;
    crate::services::trade::on_leave_locked(db, world, sde, identity.char_id).await?;
    item_dao::undock(db, ship, system, ball.pos).await.map_err(db_err)?;
    character_dao::undock(db, identity.char_id, system).await.map_err(db_err)?;
    world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    Ok(Undocked {
        change: SessionChange {
            updates: vec![("stationid", Value::None), ("solarsystemid", Value::Int(system))],
            event: (identity.char_id != 0).then_some(GameEvent::Undocked {
                char_id: identity.char_id, ship_id: ship, station_id: station, system_id: system,
            }),
        },
        economy,
    })
}

async fn undock_rpc(c: &mut Client) -> CallResult {
    let identity = Identity::of_client(c);
    let Undocked { change, economy } = undock(&identity, &c.db, &c.world, &c.sde,
        c.session.int("solarsystemid"), c.session.int("allianceid")).await?;
    info!(user = c.login.user_id, "ship {} undocked from {} into {}", identity.ship_id.unwrap(), identity.station_id.unwrap(), identity.system_id.unwrap());
    c.change_session(change.updates);
    if let Some(event) = change.event {
        let mut outbox = Outbox::default();
        outbox.event(event);
        dispatch_client((), outbox, c)?;
    }
    drop(economy);
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "Undock" => Some(undock_rpc(c).await),
        "LaunchDrones" => Some(crate::services::drones::inventory::launch::launch(c, call).await),
        "ScoopDrone" => Some(crate::services::drones::inventory::returning::scoop(c, call).await),
        _ => None,
    }
}
