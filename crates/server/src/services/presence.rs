//! Pilot preparation, ordered departure, and process-local pilot and chat presence.
//! `carbon/common/script/net/machoNet.py:3954-3957` binds the new charid to the
//! client and releases the old one in `ProcessSessionChange`. Preparation and
//! departure use character, item and skill data and the SDE to restore or save
//! pilots. The process-local pilot registry also supplies station guest records:
//! online, station changes and offline send arrival or departure notices to
//! other registered guests in the same station. Transport lifecycle and
//! cross-process presence are not managed here.

use std::sync::Arc;

use crate::chat::Hub;
use crate::dao::{characters, items, skills};
use crate::dao::characters::selection::SelectedCharacter;
use crate::db::{Db, outbox::Notification};
use crate::sde::Sde;
use crate::session::OnlineSession;
use crate::space::{NotifyTx, World};
use super::identity::Identity;
use super::{db_err, CallError};

pub(crate) fn guest_record(session: &OnlineSession) -> marshal::Value {
    marshal::Value::tuple([
        marshal::Value::Int(session.character_id),
        session.corporation_id.map(marshal::Value::Int).unwrap_or(marshal::Value::None),
        session.alliance_id.map(marshal::Value::Int).unwrap_or(marshal::Value::None),
        session.faction_id.map(marshal::Value::Int).unwrap_or(marshal::Value::None),
    ])
}

fn station_notice(world: &World, station: i64, session: &OnlineSession, event: &str) {
    for guest in world.station_guests(station) {
        if guest.character_id != session.character_id {
            world.notify_pilot(guest.character_id, event, vec![guest_record(session)]);
        }
    }
}

pub(crate) struct Prepared {
    pub ship_id: i64,
    pub cloak_until: Option<i64>,
}

pub(crate) async fn prepare(
    db: &Db, world: &World, sde: &Arc<Sde>, char_id: i64,
    selected: &SelectedCharacter,
    mut notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> Result<Prepared, CallError> {
    super::skills::expert_systems::advance_expired(db, sde, char_id, crate::client::filetime_now(), &mut notify).await?;
    let station = selected.station_id;
    let race = selected.race_id;
    let space_ship = characters::selection::space_ship(db, char_id).await.map_err(db_err)?;
    let ship = match (&space_ship, station) {
        (Some(s), None) => s.item_id,
        (_, Some(st)) => super::character::provisioning::ensure_ship(db, sde, char_id, race, st).await?,
        (None, None) => return Err(CallError::Internal(anyhow::anyhow!("character {char_id} is in space without a ship"))),
    };
    let has_skills = skills::selection::has_skills(db, char_id).await.map_err(db_err)?;
    if !has_skills {
        super::skills::starting::grant_starting_skills(db, sde, char_id, race).await?;
    }
    characters::selection::logoff(db, char_id).await.map_err(db_err)?;
    super::effects::online::reconcile_ship(db, sde, char_id, ship, notify).await?;
    if station.is_some() {
        super::combat::load_docked_condition(db, world, char_id, ship).await?;
    }
    let cloak_until = if let Some(s) = space_ship.as_ref().filter(|_| station.is_none()) {
        let type_id = items::inventory::type_id(db, ship).await.map_err(db_err)?
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing SDE ship item")))?;
        let vessel = items::space_ships::FreeShip {
            item_id: ship, type_id, owner_id: char_id, corporation_id: selected.corporation_id,
            alliance_id: selected.alliance_id.unwrap_or(-1), pos: [s.x.unwrap(), s.y.unwrap(), s.z.unwrap()],
        };
        let security = super::crimewatch::security_status(db, char_id).await?;
        let ball = super::ship::balls::piloted(db, sde.clone(), &vessel, char_id, security).await?.install();
        Some(super::ship::arrive_cloaked(world, selected.solar_system_id, ball).await?)
    } else {
        None
    };
    Ok(Prepared { ship_id: ship, cloak_until })
}

pub(crate) fn online(identity: &Identity, world: &World, chat: &Hub, notify_tx: NotifyTx, previous: Option<&OnlineSession>, current: OnlineSession) {
    let previous = previous.filter(|s| s.character_id == current.character_id);
    let old_station = previous.and_then(|s| s.station_id);
    let registered = world.online_session(current.character_id);
    if let (Some(old), Some(live)) = (previous, registered.as_ref()) {
        if old.session_id != live.session_id { return; }
    }
    let same_generation = previous.is_some_and(|s| s.session_id == current.session_id);
    let only_ship_refresh = same_generation && previous.is_some_and(|s| {
        let mut old = s.clone();
        old.ship_id = current.ship_id;
        old == current
    });
    if previous.map(|s| s.character_id) != Some(identity.char_id) {
        world.register_pilot(identity.char_id, notify_tx);
    }
    world.set_online_session(identity.char_id, Some(current.clone()));
    if only_ship_refresh { return; }
    chat.session_changed(identity.char_id);
    if let Some(old) = previous {
        if old.corporation_id != current.corporation_id || old.alliance_id != current.alliance_id
            || old.faction_id != current.faction_id || old.role != current.role {
            chat.refresh_profile(&current);
            let old_channels = super::chat::authority::system_channels(world.sde(), old);
            for channel in super::chat::authority::system_channels(world.sde(), &current) {
                if (channel.starts_with("corp_") || channel.starts_with("alliance_")) && !old_channels.contains(&channel) {
                    chat.invite(current.character_id, &channel);
                }
            }
        }
        if (old.corporation_id != current.corporation_id || old.alliance_id != current.alliance_id)
            && current.station_id.is_none() {
            if let (Some(system), Some(ship)) = (current.system_id, current.ship_id) {
                let character = current.character_id;
                let corp = current.corporation_id;
                let alliance = current.alliance_id;
                world.post(system, move |s| s.refresh_pilot_affiliation(character, ship, corp, alliance));
            }
        }
    }
    if old_station != current.station_id {
        if let (Some(station), Some(old)) = (old_station, previous) {
            station_notice(world, station, old, "OnCharNoLongerInStation");
        }
        if let Some(station) = current.station_id {
            station_notice(world, station, &current, "OnCharNowInStation");
        }
    }
}


pub(crate) fn offline_generation(identity: &Identity, world: &World, chat: &Hub, session_id: Option<i64>) {
    let previous = world.online_session(identity.char_id);
    if let Some(sid) = session_id {
        if previous.as_ref().is_none_or(|s| s.session_id != sid) { return; }
    }
    if let Some(sid) = session_id { world.unregister_pilot_generation(identity.char_id, sid); }
    else { world.unregister_pilot(identity.char_id); }
    chat.disconnect_character(identity.char_id);
    if let Some(old) = previous {
        if let Some(station) = old.station_id {
            station_notice(world, station, &old, "OnCharNoLongerInStation");
        }
    }
}

/// Logout removes only the matching pilot ball, holding the economy lock through
/// condition and position saves. Unlike dock removal, missing or replaced balls
/// are skipped, save failures do not restore a ball, and cleanup continues.
pub(crate) async fn leave(
    identity: &Identity, db: &Db, world: &World, chat: &Hub, sde: &Arc<Sde>,
    space_system: Option<i64>, session_id: Option<i64>, unobserve: impl FnOnce() + Send,
) -> anyhow::Result<()> {
    if let Some(sid) = session_id {
        if world.online_session(identity.char_id).is_some_and(|live| live.session_id != sid) {
            unobserve();
            return Ok(());
        }
    }
    let char_id = identity.char_id;
    if char_id != 0 {
        super::combat::reset_pilot(world, char_id, space_system.zip(identity.ship_id));
    }
    let mut failure = None;
    if let Some(ship) = identity.ship_id {
        let _lock = super::market::lock().await;
        let ball = if let Some(system) = space_system.filter(|_| char_id != 0) {
            world.with(system, move |s| {
                if s.pilot_ball(char_id).is_some_and(|ball| ball.id == ship) {
                    s.remove_ball(ship)
                } else {
                    None
                }
            }).await.flatten()
        } else {
            None
        };
        if space_system.is_none() || ball.is_some() {
            if let Err(error) = super::combat::save_condition_in(db, ship).await {
                let error = match error {
                    CallError::Internal(error) => error,
                    CallError::User { msg, .. } => anyhow::anyhow!(msg),
                };
                tracing::warn!(ship, %error, "saving condition on leave failed");
                failure = Some(error);
            }
        }
        if let Some(b) = ball {
            if let Err(error) = crate::dao::items::positions::save(db, ship, &b.pos).await {
                tracing::warn!(ship, %error, "saving position on leave failed");
                if failure.is_none() { failure = Some(error.into()); }
            }
        }
    }
    unobserve();
    if char_id != 0 {
        super::drones::settings::logged_off(char_id);
        offline_generation(identity, world, chat, session_id);
        super::online_status::presence_changed(db, world, char_id, false).await;
        if let Err(error) = super::trade::on_leave(identity, db, world, sde).await {
            let error = match error {
                CallError::Internal(error) => error,
                CallError::User { msg, .. } => anyhow::anyhow!(msg),
            };
            tracing::warn!(%error, "trade cleanup on leave failed");
            if failure.is_none() { failure = Some(error); }
        }
    }
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests;
