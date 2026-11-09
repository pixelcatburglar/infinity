//! Ship RPC routing for undocking, boarding, stored ships, fitting, configuration,
//! deployment, drones and persistent `GetDirtTimestamp`/`ResetDirtTimestamp`.
//! `ballparkCommon/docking.py` defines NPC undock channels and initial velocity;
//! `eve/client/script/ui/shared/fitting/cleanShipButton.py` uses ship cleaning.
//! Data includes `game.ship_dirt`, game items and character locations, and the SDE
//! map and station graphics. Successful undocking records its lifecycle event
//! with the location change. Upwell undocking, structure `LeaveShip`, POS
//! `SetShipPassword` and `GetShipFittingInfo` are unsupported.
use super::*;

/// The caller must hold `economy` until session updates are handled.
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
    let economy = crate::services::market::lock().await;
    let mut ball = balls::selected(identity, db, world, sde, space_system, alliance_id, ship, spos).await?;
    let group = sde.types.get(ball.ball.type_id).and_then(|ty| ty.group_id);
    undock_point::prepare(&mut ball.ball, sde.graphic_locations.get(station_type), group, spos,
        sde.celestial_radius(st), crate::space::collision::enabled(), crate::services::rand01);
    crate::services::trade::on_leave_locked(db, world, sde, identity.char_id).await?;
    let character = identity.char_id;
    let position = ball.ball.pos;
    let market = &economy;
    let affiliation = db.transaction(
        |tx, outbox: &mut Outbox| Box::pin(async move {
            let affiliation = if character != 0 {
                crate::dao::characters::selection::lock_character(tx, character).await.map_err(db_err)?;
                let current = character_dao::current(tx, character).await.map_err(db_err)?
                    .ok_or_else(|| CallError::User { msg: messages::MSG_CANT_UNDOCK_NOT_IN_STATION.into(), dict: vec![] })?;
                if current.station != Some(station) || current.system != Some(system) || current.ship != Some(ship) {
                    return Err(CallError::User { msg: messages::MSG_CANT_UNDOCK_NOT_IN_STATION.into(), dict: vec![] });
                }
                let authority = crate::dao::characters::roles::authority(tx, character).await.map_err(db_err)?
                    .ok_or_else(|| CallError::User { msg: messages::MSG_CANT_UNDOCK_NOT_IN_STATION.into(), dict: vec![] })?;
                Some((current.corporation, current.alliance, authority))
            } else { None };
            item_dao::undock(tx, ship, system, position).await.map_err(db_err)?;
            character_dao::undock(tx, character, system).await.map_err(db_err)?;
            if character != 0 {
                outbox.event(GameEvent::Undocked { char_id: character, ship_id: ship, station_id: station, system_id: system });
            }
            Ok(affiliation)
        }),
        |affiliation, outbox| async move {
            if let Some((corp, alliance, _)) = &affiliation {
                ball.ball.corp_id = *corp;
                ball.ball.alliance_id = alliance.unwrap_or(-1);
            }
            publish_ship(world, system, ball, market).await?;
            crate::db::outbox::dispatch_world(affiliation, outbox, world)
        },
    ).await?;
    let mut updates = vec![("stationid", Value::None), ("solarsystemid", Value::Int(system))];
    if let Some((corp, alliance, authority)) = affiliation {
        let changed = corp != identity.corp_id || alliance != alliance_id
            || world.online_session(character).is_some_and(|session|
                session.corporation_id != Some(corp) || session.alliance_id != alliance || session.corporation_role != authority.roles[0]);
        if changed {
            let opt = |value: Option<i64>| value.map(Value::Int).unwrap_or(Value::None);
            let hq = authority.headquarters.or_else(|| sde.npc_corporations.get(corp).and_then(|row| row.station_id));
            updates.extend([("corpid", Value::Int(corp)), ("allianceid", opt(alliance)), ("corprole", Value::Int(authority.roles[0])),
                ("rolesAtAll", Value::Int(authority.roles[0])), ("rolesAtHQ", Value::Int(authority.roles[1])),
                ("rolesAtBase", Value::Int(authority.roles[2])), ("rolesAtOther", Value::Int(authority.roles[3])), ("hqID", opt(hq))]);
            if corp != identity.corp_id { updates.push(("corpAccountKey", Value::Int(infinity_core::consts::ACCOUNT_CASH as i64))); }
        }
    }
    Ok(Undocked {
        change: SessionChange {
            updates,
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
    drop(economy);
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "Undock" => Some(undock_rpc(c).await),
        "Board" => Some(super::boarding::board(c, call.args.first().and_then(|v| v.as_int()).unwrap_or(0), call.args.get(1).and_then(|v| v.as_int())).await),
        "StoreVessel" => Some(super::stored::store(c, call).await),
        "BoardStoredShip" => Some(super::stored::board(c, call).await),
        "LaunchFromContainer" => Some(super::stored::launch(c, call).await),
        "AssembleShip" => Some(crate::services::fitting::ship::assemble(c, call).await),
        "CheckEnoughSubSystemToUndock" => Some(Ok(Value::Bool(true))),
        "GetDirtTimestamp" => Some(super::cleaning::get(c, call).await),
        "ResetDirtTimestamp" => Some(super::cleaning::reset(c, call).await),
        "LeaveShip" => Some(Err(crate::services::notify_error(infinity_core::messages::ship::LEAVE_UNAVAILABLE))),
        "SetShipPassword" => Some(Err(crate::services::notify_error(infinity_core::messages::ship::PASSWORD_UNAVAILABLE))),
        "GetShipFittingInfo" => Some(Err(crate::services::notify_error(infinity_core::messages::crates::METADATA_UNAVAILABLE))),
        "GetShipConfiguration" => Some(super::configuration::get(c, call).await),
        "ConfigureShip" => Some(super::configuration::configure(c, call).await),
        "Drop" => Some(super::deployment::drop_items(c, call).await),
        "LaunchDrones" => Some(crate::services::drones::inventory::launch::launch(c, call).await),
        "ScoopDrone" => Some(crate::services::drones::inventory::returning::scoop(c, call).await),
        _ => None,
    }
}
