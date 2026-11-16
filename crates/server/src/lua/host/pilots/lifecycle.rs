//! Lifecycle transitions for Lua-owned pilots through the normal presence services.
//! Character and item DAOs supply authoritative location and ship state. Runtime
//! chat and notification state is process-local and does not survive server restart.
//! Simulated space entry initializes the normal asteroid environment.
//! Membership session notifications use the existing pilot generation and presence adapter.

use infinity_core::{messages, session::OnlineSession};
use crate::{dao::characters::selection::{self, SelectedCharacter}, db::outbox::{dispatch_world, Outbox}, lua::events::GameEvent, services::{self, CallError}, space::NotifyTx};
use super::{Pilots, Resources};
use crate::services::identity::Identity;

pub(super) struct OnlinePilot {
    pub(super) session: OnlineSession,
    pub(super) notify: NotifyTx,
    drain: tokio::task::JoinHandle<()>,
    binding: std::sync::Arc<crate::space::containment::PilotBinding>,
    admitted: bool,
}

async fn selected(r: &Resources, id: i64) -> Result<SelectedCharacter, CallError> {
    selection::selected(&r.db, id, r.pilots.account).await.map_err(services::db_err)?
        .ok_or_else(|| CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] })
}

pub(super) async fn snapshot(r: &Resources, id: i64, row: &SelectedCharacter) -> Result<Identity, CallError> {
    identity_from(&r.db, id, row).await
}
async fn identity_from(db: &crate::db::Db, id: i64, row: &SelectedCharacter) -> Result<Identity, CallError> {
    Ok(Identity { char_id: id, corp_id: row.corporation_id, ship_id: selection::ship_id(db, id).await.map_err(services::db_err)?, station_id: row.station_id, system_id: Some(row.solar_system_id) })
}
/// Mirrors `eve/client/script/parklife/sensorSuiteService.py` `OnEnterSpace`,
/// which calls `scanMgr.SignalTrackerRegister` after `solarsystemid` is available.
pub(super) async fn entered_space(r: &Resources, session: &OnlineSession) -> Result<(), CallError> {
    if let (None, Some(system)) = (session.station_id, session.system_id) {
        r.world.system(system).await.map_err(CallError::Internal)?;
    }
    Ok(())
}

pub(super) async fn locked(r: &Resources, id: i64) -> Result<(tokio::sync::OwnedMutexGuard<Option<OnlinePilot>>, SelectedCharacter), CallError> {
    selected(r, id).await?;
    let state = r.pilots.state(id).lock_owned().await;
    let row = selected(r, id).await?;
    Ok((state, row))
}

pub(super) async fn identity(r: &Resources, id: i64) -> Result<(Identity, bool), CallError> {
    let (_state, row) = locked(r, id).await?;
    Ok((snapshot(r, id, &row).await?, r.world.is_online(id)))
}

pub(super) async fn online(r: Resources, id: i64) -> Result<bool, CallError> {
    let (mut state, _) = locked(&r, id).await?;
    if state.is_some() { return Ok(false); }
    let account = r.pilots.account;
    let row = r.db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            selection::lock_character(tx, id).await.map_err(services::db_err)?;
            selection::selected(tx, id, account).await.map_err(services::db_err)
        }),
        |value, ()| async move { Ok(value) },
    ).await?.ok_or_else(|| CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] })?;
    let (notify, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let pilots = std::sync::Arc::downgrade(&r.pilots);
    let world = std::sync::Arc::downgrade(&r.world);
    let sender = notify.downgrade();
    let drain = tokio::spawn(async move {
        while let Some((event, args)) = rx.recv().await {
            if event != "__session" { continue; }
            let (Some(pilots), Some(world), Some(sender)) = (pilots.upgrade(), world.upgrade(), sender.upgrade()) else { break };
            let state = pilots.state(id);
            let mut guard = state.lock().await;
            let Some(pilot) = guard.as_mut() else { continue };
            if !pilot.notify.same_channel(&sender) { continue; }
            let Some(live) = world.online_session(id) else { continue };
            if live.session_id != pilot.session.session_id { continue; }
            let pairs = infinity_core::session::decode_pairs(&args);
            let previous = pilot.session.clone();
            let mut current = previous.clone();
            if current.apply_updates(pairs.iter().map(|(key, value)| (key.as_str(), value))) {
                let identity = Identity {
                    char_id: id, corp_id: current.corporation_id.unwrap_or(0), ship_id: current.ship_id,
                    station_id: current.station_id, system_id: current.system_id,
                };
                services::presence::online(&identity, &world, &pilots.hub, sender, Some(&previous), current.clone());
                pilot.session = current;
            }
        }
    });
    // The slot is owned before `prepare`: session, notify receiver, drain task
    // and binding already exist, so a failed or panicking prepare has exactly
    // this generation to finish. `ship_id` stays absent until prepare returns.
    let mut session = OnlineSession { character_id: id, user_id: None, session_id: 0, ship_id: None,
        station_id: row.station_id.filter(|id| *id > 0), system_id: Some(row.solar_system_id), corporation_id: Some(row.corporation_id),
        alliance_id: row.alliance_id.filter(|id| *id > 0), faction_id: None, fleet_id: None, role: 0, corporation_role: row.roles };
    *state = Some(OnlinePilot {
        session: session.clone(), notify: notify.clone(), drain,
        binding: bind_headless(&r, id, notify.clone())?, admitted: false,
    });
    let prepared = match services::presence::prepare(&r.db, &r.world, &r.sde, id, &row,
        |n| services::effects::online::dispatch_notification(n, &r.world, &notify, Some(id))).await {
        Ok(prepared) => prepared,
        Err(error) => {
            if let Err(cleanup) = finish_slot(&r.pilots, &r.world, id, Some(&row), &mut state).await {
                tracing::warn!(?cleanup, char_id = id, "simulated pilot cleanup failed");
            }
            return Err(error);
        }
    };
    let identity = Identity { char_id: id, corp_id: row.corporation_id, ship_id: Some(prepared.ship_id), station_id: row.station_id, system_id: Some(row.solar_system_id) };
    session.ship_id = Some(prepared.ship_id);
    if let Some(pilot) = state.as_mut() { pilot.session = session.clone(); }
    r.pilots.hub.set_profile(id, &row.name, i64::from(row.gender_id), row.corporation_id, row.alliance_id, None, 0);
    services::presence::online(&identity, &r.world, &r.pilots.hub, notify.clone(), None, session.clone());
    let admission = async {
        r.pilots.hub.join_virtual(id).await?;
        entered_space(&r, &session).await
    }.await;
    if let Err(error) = admission {
        finish_slot(&r.pilots, &r.world, id, Some(&row), &mut state).await?;
        return Err(error);
    }
    let reconciled: Result<(), CallError> = async {
        let fresh = selection::selected(&r.db, id, account).await.map_err(services::db_err)?
            .ok_or_else(|| CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] })?;
        let previous = session.clone();
        session.corporation_id = Some(fresh.corporation_id);
        session.alliance_id = fresh.alliance_id.filter(|id| *id > 0);
        session.corporation_role = fresh.roles;
        let current_identity = Identity { corp_id: fresh.corporation_id, ..identity };
        r.pilots.hub.set_profile(id, &fresh.name, i64::from(fresh.gender_id), fresh.corporation_id, fresh.alliance_id, None, 0);
        services::presence::online(&current_identity, &r.world, &r.pilots.hub, notify.clone(), Some(&previous), session.clone());
        selection::logon(&r.db, id).await.map_err(services::db_err)?;
        Ok(())
    }.await;
    if let Err(error) = reconciled {
        finish_slot(&r.pilots, &r.world, id, Some(&row), &mut state).await?;
        return Err(error);
    }
    state.as_mut().expect("headless slot").admitted = true;
    state.as_mut().expect("headless slot").session = session;
    services::online_status::presence_changed(&r.db, &r.world, id, true).await;
    let world = r.world.clone();
    let user_id = r.pilots.account;
    if let Err(error) = r.db.transaction(
        |_, outbox: &mut Outbox| Box::pin(async move {
            outbox.event(GameEvent::CharacterSelected { user_id, char_id: id });
            Ok::<_, CallError>(())
        }),
        |value, outbox| async move { dispatch_world(value, outbox, &world) },
    ).await {
        tracing::warn!(?error, char_id = id, "simulated pilot selection event dispatch failed");
    }
    Ok(true)
}

pub(super) async fn offline(r: Resources, id: i64) -> Result<(), CallError> {
    let (mut state, row) = locked(&r, id).await?;
    if state.is_none() { return Ok(()); }
    finish_slot(&r.pilots, &r.world, id, Some(&row), &mut state).await
}

fn bind_headless(r: &Resources, id: i64, notify: NotifyTx) -> Result<std::sync::Arc<crate::space::containment::PilotBinding>, CallError> {
    r.world.bind_pilot(id, notify, Some(crate::space::containment::DisconnectTarget::Headless(std::sync::Arc::downgrade(&r.pilots))), None)
}

/// One finishing path for ordinary offline, a failed admission and an exact
/// actor retirement: attempt the real presence leave while the caller's slot
/// guard serializes it, then always take this exact slot, abort and await its
/// notification receiver, and release the binding root. The leave's real
/// result is returned; a never-admitted pilot leaves without a user id.
async fn finish_slot(pilots: &Pilots, world: &std::sync::Arc<crate::space::World>, id: i64, row: Option<&SelectedCharacter>,
    state: &mut Option<OnlinePilot>) -> Result<(), CallError> {
    if state.is_none() { return Ok(()); }
    let leave = match row {
        Some(row) => match identity_from(world.db(), id, row).await {
            Ok(identity) => {
                let user = state.as_ref().expect("headless slot").admitted.then_some(pilots.account);
                services::presence::leave(&identity, user, world.db(), world, &pilots.hub, world.sde(),
                    row.station_id.is_none().then_some(row.solar_system_id), None, || {}).await.map_err(CallError::Internal)
            }
            Err(error) => Err(error),
        },
        None => Err(CallError::Internal(anyhow::anyhow!("headless character identity unavailable"))),
    };
    let Some(pilot) = state.take() else { return leave };
    pilot.drain.abort();
    let _ = pilot.drain.await;
    drop(pilot.binding);
    leave
}

/// Retire the exact old generation matching `expected`: an absent or closed
/// slot is already gone and a live replacement is left alone. The same slot
/// guard that serializes normal offline verifies the channel and runs the
/// real cleanup; an ordinary cleanup error is reported, not a lost proof.
pub(super) async fn retire_generation(pilots: &Pilots, world: &std::sync::Arc<crate::space::World>, character: i64,
    expected: &NotifyTx) -> Result<(), crate::space::containment::ContainmentFailure>
{
    let state = pilots.state(character);
    let mut guard = state.lock().await;
    let matched = match guard.as_ref() {
        Some(pilot) if pilot.notify.same_channel(expected) => true,
        _ if expected.is_closed() => false,
        _ => return Err(crate::space::containment::ContainmentFailure::HeadlessOwnerUnproven),
    };
    if !matched { return Ok(()); }
    let selected = selection::selected(world.db(), character, pilots.account).await;
    let row = match &selected { Ok(row) => row.as_ref(), Err(_) => None };
    if let Err(error) = finish_slot(pilots, world, character, row, &mut guard).await {
        tracing::warn!(?error, character, "retired headless pilot cleanup failed");
    }
    match selected {
        Ok(Some(_)) => {}
        other => {
            let error = match other {
                Err(error) => services::db_err(error),
                Ok(None) => CallError::Internal(anyhow::anyhow!("headless character identity unavailable")),
                Ok(Some(_)) => unreachable!("matched identity was already returned"),
            };
            tracing::warn!(?error, character, "retired headless identity failed after cleanup");
        }
    }
    Ok(())
}
