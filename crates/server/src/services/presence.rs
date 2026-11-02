//! Pilot preparation, ordered departure, and process-local pilot and chat presence.
//! `carbon/common/script/net/machoNet.py:3954-3957` binds the new charid to the
//! client and releases the old one in `ProcessSessionChange`. Preparation and
//! departure use character, item and skill data and the SDE to restore or save
//! pilots. Transport lifecycle and cross-process presence are not managed here.

use std::sync::Arc;

use crate::chat::Hub;
use crate::dao::{characters, items, skills};
use crate::dao::characters::selection::SelectedCharacter;
use crate::db::{Db, outbox::Notification};
use crate::sde::Sde;
use crate::session::ChatSession;
use crate::space::{NotifyTx, World};
use super::identity::Identity;
use super::{db_err, CallError};

pub(crate) struct Prepared {
    pub ship_id: i64,
    pub cloak_until: Option<i64>,
}

pub(crate) async fn prepare(
    db: &Db, world: &World, sde: &Arc<Sde>, char_id: i64,
    selected: &SelectedCharacter,
    notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> Result<Prepared, CallError> {
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

pub(crate) fn online(identity: &Identity, world: &World, chat: &Hub, notify_tx: NotifyTx, previous: Option<&ChatSession>, current: ChatSession) {
    if previous.map(|s| s.character_id) != Some(identity.char_id) {
        world.register_pilot(identity.char_id, notify_tx);
    }
    world.set_chat_session(identity.char_id, Some(current));
    chat.session_changed(identity.char_id);
}

pub(crate) fn offline(identity: &Identity, world: &World, chat: &Hub) {
    world.unregister_pilot(identity.char_id);
    chat.disconnect_character(identity.char_id);
}

/// Logout removes only the matching pilot ball, holding the economy lock through
/// condition and position saves. Unlike dock removal, missing or replaced balls
/// are skipped, save failures do not restore a ball, and cleanup continues.
pub(crate) async fn leave(
    identity: &Identity, db: &Db, world: &World, chat: &Hub, sde: &Arc<Sde>,
    space_system: Option<i64>, unobserve: impl FnOnce() + Send,
) -> anyhow::Result<()> {
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
        offline(identity, world, chat);
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
