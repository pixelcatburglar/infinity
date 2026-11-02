//! Pilot preparation and process-local pilot and chat presence.
//! `carbon/common/script/net/machoNet.py:3954-3957` binds the new charid to the
//! client and releases the old one in `ProcessSessionChange`. Preparation uses
//! character, item and skill data and the SDE to restore the selected pilot.
//! Transport lifecycle and cross-process presence are not managed here.

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

#[cfg(test)]
mod tests;
