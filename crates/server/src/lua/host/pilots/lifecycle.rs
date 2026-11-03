//! Lifecycle transitions for Lua-owned pilots through the normal presence services.
//! Character and item DAOs supply authoritative location and ship state. Runtime
//! chat and notification state is process-local and does not survive server restart.

use crate::{dao::characters::selection::{self, SelectedCharacter}, lua::events::GameEvent, messages, services::{self, CallError}, session::ChatSession, space::NotifyTx};
use super::Resources;
use crate::services::identity::Identity;

pub(super) struct OnlinePilot {
    _session: ChatSession,
    _notify: NotifyTx,
    drain: tokio::task::JoinHandle<()>,
}

async fn selected(r: &Resources, id: i64) -> Result<SelectedCharacter, CallError> {
    selection::selected(&r.db, id, r.pilots.account).await.map_err(services::db_err)?
        .ok_or_else(|| CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] })
}

async fn snapshot(r: &Resources, id: i64, row: &SelectedCharacter) -> Result<Identity, CallError> {
    Ok(Identity { char_id: id, corp_id: row.corporation_id, ship_id: selection::ship_id(&r.db, id).await.map_err(services::db_err)?, station_id: row.station_id, system_id: Some(row.solar_system_id) })
}

pub(super) async fn identity(r: &Resources, id: i64) -> Result<(Identity, bool), CallError> {
    selected(r, id).await?;
    let state = r.pilots.state(id);
    let _state = state.lock().await;
    let row = selected(r, id).await?;
    Ok((snapshot(r, id, &row).await?, r.world.is_online(id)))
}

pub(super) async fn online(r: Resources, id: i64) -> Result<bool, CallError> {
    selected(&r, id).await?;
    let state = r.pilots.state(id);
    let mut state = state.lock().await;
    let row = selected(&r, id).await?;
    if state.is_some() { return Ok(false); }
    let (notify, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let prepared = match services::presence::prepare(&r.db, &r.world, &r.sde, id, &row,
        |n| services::effects::online::dispatch_notification(n, &r.world, &notify, Some(id))).await {
        Ok(prepared) => prepared,
        Err(error) => { drain.abort(); return Err(error); }
    };
    let identity = Identity { char_id: id, corp_id: row.corporation_id, ship_id: Some(prepared.ship_id), station_id: row.station_id, system_id: Some(row.solar_system_id) };
    let session = ChatSession { character_id: id, station_id: row.station_id.filter(|id| *id > 0), system_id: Some(row.solar_system_id), corporation_id: Some(row.corporation_id), alliance_id: row.alliance_id.filter(|id| *id > 0), faction_id: None, fleet_id: None, role: 0, corporation_role: row.roles };
    r.pilots.hub.set_profile(id, &row.name, i64::from(row.gender_id), row.corporation_id, row.alliance_id, None, 0);
    services::presence::online(&identity, &r.world, &r.pilots.hub, notify.clone(), None, session.clone());
    if let Err(error) = r.pilots.hub.join_virtual(id).await {
        if let Err(cleanup) = services::presence::leave(&identity, &r.db, &r.world, &r.pilots.hub, &r.sde, row.station_id.is_none().then_some(row.solar_system_id), || {}).await {
            tracing::warn!(%cleanup, "simulated pilot admission cleanup failed");
        }
        drain.abort();
        return Err(error);
    }
    *state = Some(OnlinePilot { _session: session, _notify: notify, drain });
    services::online_status::presence_changed(&r.db, &r.world, id, true).await;
    crate::lua::on_event(GameEvent::CharacterSelected { user_id: r.pilots.account, char_id: id });
    Ok(true)
}

pub(super) async fn offline(r: Resources, id: i64) -> Result<(), CallError> {
    selected(&r, id).await?;
    let state = r.pilots.state(id);
    let mut state = state.lock().await;
    let row = selected(&r, id).await?;
    if state.is_none() { return Ok(()); }
    let identity = snapshot(&r, id, &row).await?;
    let result = services::presence::leave(&identity, &r.db, &r.world, &r.pilots.hub, &r.sde, row.station_id.is_none().then_some(row.solar_system_id), || {}).await;
    if let Some(pilot) = state.take() { pilot.drain.abort(); }
    result.map_err(CallError::Internal)?;
    selection::logoff(&r.db, id).await.map_err(services::db_err)?;
    crate::lua::on_event(GameEvent::CharacterLoggedOff { user_id: r.pilots.account, char_id: id });
    Ok(())
}
