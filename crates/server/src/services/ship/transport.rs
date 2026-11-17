//! Relocation of the connected pilot's active ship for GM transport commands.
//! The `station` submodule owns the docked-arrival transaction shared with
//! ordinary docking (`services/ship/mod.rs` and `space/boarding.rs`); this
//! file adapts a connected `Client` to it and keeps space-to-space relocation
//! on the existing teleport primitive.
//! Data comes from `game.characters`, `game.items` and the SDE map. On a
//! committed move the callback applies the session change, publishes the
//! root-hull `OnItemChange` (consumed by
//! `eve/client/script/environment/invCache.py`), and invites the pilot to the
//! destination's new system channels. Station-to-space relocation is not yet
//! supported; it arrives with a real departure path instead of this adapter.

use super::{CallError, Client, SessionChange};
use crate::services::{identity::Identity, notify_error};
use crate::db::outbox::{dispatch_client, Outbox};
use crate::sde::Sde;
use crate::space::V3;
use infinity_core::messages;
use infinity_core::session::OnlineSession;

pub(crate) mod station;

/// A resolved transport destination: an NPC station, or a point in space.
pub(crate) enum Destination {
    Station(i64),
    Space { system: i64, position: V3 },
}

/// Move the caller to `destination`: stations through the shared
/// station-arrival core, space points through the existing teleport. A
/// station source is a cold transaction with no space state to sever; a space
/// source commits through the condition-checkpoint worker, then this adapter
/// severs the old destiny flow before the station session change. Relocating
/// out of a station into space is refused until its departure path lands.
pub(in crate::services) async fn relocate(c: &mut Client, destination: Destination) -> Result<(), CallError> {
    match destination {
        Destination::Space { system, position } => {
            if c.session.int("stationid").is_some() {
                return Err(notify_error(messages::GM_UNDOCK_FIRST));
            }
            super::teleport(c, system, position).await
        }
        Destination::Station(station) => {
            let sde: &Sde = &c.sde;
            let destination = station::resolve(sde, station)?;
            let identity = Identity::of_client(c);
            let expected = OnlineSession::from_session(&c.session)
                .filter(|expected| expected.user_id == Some(c.login.user_id))
                .ok_or_else(|| notify_error(messages::gm::GM_TRANSPORT_CONFLICT))?;
            let old_channels = crate::services::chat::authority::system_channels(sde, &expected);
            let in_space = identity.station_id.is_none();
            let db = c.db.clone();
            let world = c.world.clone();
            let old_char = identity.char_id;
            station::enter(&identity, &expected, &db, &world, destination,
                move |change: SessionChange, outbox: Outbox| {
                    if in_space { c.leave_space(); }
                    c.change_session(change.updates);
                    dispatch_client((), outbox, c)?;
                    // The committed session now names the destination; invite
                    // only the system channels that did not exist before.
                    let session = OnlineSession::from_session(&c.session)
                        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("online session lost during station arrival")))?;
                    for channel in crate::services::chat::authority::system_channels(&c.sde, &session) {
                        if !old_channels.contains(&channel) {
                            c.chat.invite(old_char, &channel);
                        }
                    }
                    Ok(())
                }).await
        }
    }
}

#[cfg(all(test, feature = "tests-ships"))]
mod tests;
