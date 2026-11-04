//! Runtime MUC occupant lifecycle for `xmppchatsvc.py` and room settings clients.
//! Authority and durable ACLs live in chat services/DAO; cached occupant roles are
//! refreshed after commits. Virtual occupants have no transport and join only
//! authority-derived system rooms; durable custom virtual rooms are unsupported.
//! Password rooms, corporate ACLs and proxy ownership are unsupported.
use super::*;
use crate::services::{chat::authority, CallError};

/// Server-designated XMPP resource for protocol metadata, not an authenticated socket.
const VIRTUAL_RESOURCE: &str = "virtual";

pub(super) enum JoinError { Forbidden, Conflict, Domain(CallError) }

impl Hub {
    pub(super) async fn join_room(&self, session: &crate::session::OnlineSession, name: &str, full: &str, real: Option<(&Tx, &str)>) -> Result<(), JoinError> {
        let epoch = self.rooms.lock().unwrap().epoch;
        let admission = authority::admit(&self.db, self.world.sde(), session, name).await.map_err(JoinError::Domain)?.ok_or(JoinError::Forbidden)?;
        self.commit_admissions(session, epoch, &[(name.to_owned(), admission)], full, real)
    }

    pub(super) fn commit_admissions(&self, session: &crate::session::OnlineSession, epoch: u64, admissions: &[(String, authority::Admission)], full: &str, real: Option<(&Tx, &str)>) -> Result<(), JoinError> {
        let nick = session.character_id.to_string();
        let mut rooms = self.rooms.lock().unwrap();
        if rooms.epoch != epoch || self.world.online_session(session.character_id).as_ref() != Some(session) { return Err(JoinError::Forbidden); }
        for (name, _) in admissions {
            let room = format!("{name}@{}", self.conference());
            if let Some(old) = rooms.entries.get(&room).and_then(|occupants| occupants.get(&nick)) {
                match real {
                    Some((tx, _)) if old.matches_socket(full, tx) => (),
                    None if old.tx.is_none() && old.full_jid == full => (),
                    _ => return Err(JoinError::Conflict),
                }
            }
        }
        for (name, admission) in admissions {
            let room = format!("{name}@{}", self.conference());
            let occupants = rooms.entries.entry(room.clone()).or_default();
            if real.is_none() && occupants.get(&nick).is_some_and(|old| old.tx.is_none() && old.full_jid == full) { continue; }
            if let Some((tx, _)) = real {
                for (n, o) in occupants.iter() {
                    let _ = tx.send(format!("<presence from='{}' to='{}'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='{}' role='{}' jid='{}'/></x>{}</presence>", esc(&format!("{room}/{n}")), esc(full), esc(&o.affiliation), esc(&o.role), esc(&o.full_jid), self.profile(n)));
                }
            }
            let p = format!("<presence from='{}'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='{}' role='{}' jid='{}'/></x>{}</presence>", esc(&format!("{room}/{nick}")), esc(&admission.affiliation), esc(&admission.role), esc(full), self.profile(&nick));
            for o in occupants.values() { o.send(p.clone()); }
            occupants.insert(nick.clone(), Occupant { full_jid: full.to_owned(), tx: real.map(|(tx, _)| tx.clone()), affiliation: admission.affiliation.clone(), role: admission.role.clone() });
            if let Some((tx, id)) = real {
                let _ = tx.send(format!("<presence id='{}' from='{}' to='{}'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='{}' role='{}' jid='{}'/><status code='{MUC_SELF_PRESENCE}'/></x>{}</presence>", esc(id), esc(&format!("{room}/{nick}")), esc(full), esc(&admission.affiliation), esc(&admission.role), esc(full), self.profile(&nick)));
                let _ = tx.send(format!("<message from='{}' to='{}' type='groupchat'><subject>{}</subject></message>", esc(&room), esc(full), esc(&admission.motd)));
            }
        }
        Ok(())
    }

    pub(crate) async fn join_virtual(&self, character_id: i64) -> Result<(), CallError> {
        let denied = || crate::services::notify_error(crate::messages::chat::FORBIDDEN);
        let session = self.world.online_session(character_id).filter(|s| s.character_id == character_id).ok_or_else(denied)?;
        let epoch = self.rooms.lock().unwrap().epoch;
        let full = format!("{character_id}@{}/{VIRTUAL_RESOURCE}", self.host);
        let mut admissions = Vec::new();
        for name in authority::system_channels(self.world.sde(), &session) {
            let admission = authority::admit(&self.db, self.world.sde(), &session, &name).await?.ok_or_else(denied)?;
            admissions.push((name, admission));
        }
        self.commit_admissions(&session, epoch, &admissions, &full, None)
            .map_err(|error| match error { JoinError::Domain(error) => error, JoinError::Forbidden | JoinError::Conflict => denied() })?;
        Ok(())
    }
}


fn unavailable(room: &str, nick: &str, destroy: bool) -> String {
    format!("<presence from='{}' type='unavailable'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='none' role='none'/>{}</x></presence>", esc(&format!("{room}/{nick}")), if destroy { "<destroy/>" } else { "" })
}

impl Hub {
    fn remove_matching(&self, predicate: impl Fn(&str, &str, &Occupant) -> bool, destroy: bool) {
        let mut rooms = self.rooms.lock().unwrap();
        rooms.epoch = rooms.epoch.wrapping_add(1);
        for (room, occupants) in &mut rooms.entries {
            let gone: Vec<_> = occupants.iter().filter(|(nick, o)| predicate(room, nick, o)).map(|(nick, _)| nick.clone()).collect();
            for nick in gone {
                if let Some(removed) = occupants.remove(&nick) {
                    let stanza = unavailable(room, &nick, destroy);
                    removed.send(stanza.clone());
                    for occupant in occupants.values() { occupant.send(stanza.clone()); }
                }
            }
        }
        rooms.entries.retain(|_, occupants| !occupants.is_empty());
    }

    pub(super) fn leave_all(&self, full_jid: &str, tx: &Tx) {
        self.remove_matching(|_, _, o| o.matches_socket(full_jid, tx), false);
    }

    pub(crate) fn disconnect_character(&self, id: i64) {
        self.tokens.lock().unwrap().remove(&id.to_string());
        self.remove_matching(|_, nick, _| nick == id.to_string(), false);
    }

    pub(crate) fn session_changed(&self, id: i64) {
        let allowed = self.world.online_session(id).map(|s| authority::system_channels(self.world.sde(), &s)).unwrap_or_default();
        self.remove_matching(|room, nick, _| {
            let channel = room.split('@').next().unwrap_or("");
            nick == id.to_string() && ["local_", "wormhole_", "nolocal_", "nullsec_", "triglavian_", "corp_", "alliance_", "faction_", "fleet_"].iter().any(|prefix| channel.starts_with(prefix)) && !allowed.iter().any(|name| name == channel)
        }, false);
    }

    pub(crate) fn refresh_profile(&self, session: &crate::session::OnlineSession) {
        let nick = session.character_id.to_string();
        let xml = {
            let mut profiles = self.profiles.lock().unwrap();
            let Some(profile) = profiles.get_mut(&nick) else { return };
            profile.corp = session.corporation_id.unwrap_or(0);
            profile.alliance = session.alliance_id;
            profile.faction = session.faction_id;
            profile.role = session.role;
            profile.xml()
        };
        let rooms = self.rooms.lock().unwrap();
        for (room, occupants) in &rooms.entries {
            let Some(occupant) = occupants.get(&nick) else { continue };
            let stanza = format!("<presence from='{}'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='{}' role='{}' jid='{}'/></x>{xml}</presence>",
                esc(&format!("{room}/{nick}")), esc(&occupant.affiliation), esc(&occupant.role), esc(&occupant.full_jid));
            for target in occupants.values() { target.send(stanza.clone()); }
        }
    }

    pub(crate) fn evict_room(&self, channel: &str) {
        let room = format!("{channel}@{}", self.conference());
        self.remove_matching(|r, _, _| r == room, true);
    }

    pub(crate) fn evict_member(&self, channel: &str, id: i64) {
        let room = format!("{channel}@{}", self.conference());
        self.remove_matching(|r, nick, _| r == room && nick == id.to_string(), false);
    }

    pub(crate) fn broadcast_subject(&self, channel: &str, subject: &str) {
        let room = format!("{channel}@{}", self.conference());
        let rooms = self.rooms.lock().unwrap();
        if let Some(occupants) = rooms.entries.get(&room) {
            for o in occupants.values() { o.send(format!("<message from='{}' to='{}' type='groupchat'><subject>{}</subject></message>", esc(&room), esc(&o.full_jid), esc(subject))); }
        }
    }

    pub(crate) async fn refresh_permissions(&self, channel: &str) -> Result<(), CallError> {
        let room = format!("{channel}@{}", self.conference());
        let occupants = {
            let mut rooms = self.rooms.lock().unwrap();
            rooms.epoch = rooms.epoch.wrapping_add(1);
            rooms.entries.get(&room).cloned().unwrap_or_default()
        };
        for (nick, old) in occupants {
            let id = match nick.parse::<i64>() { Ok(id) => id, Err(_) => continue };
          loop {
            let epoch = self.rooms.lock().unwrap().epoch;
            let session = self.world.online_session(id);
            let admission = match &session {
                Some(s) => match authority::admit(&self.db, self.world.sde(), s, channel).await {
                    Ok(admission) => admission,
                    Err(error) => { self.remove_matching(|r, _, _| r == room, false); return Err(error); }
                },
                None => None,
            };
            let mut rooms = self.rooms.lock().unwrap();
            if rooms.epoch != epoch || self.world.online_session(id) != session { continue; }
            let Some(occupants) = rooms.entries.get_mut(&room) else { break };
            if !occupants.get(&nick).is_some_and(|o| o.same_identity(&old)) { break; }
            if let Some(admission) = admission {
                let o = occupants.get_mut(&nick).unwrap();
                if o.role != admission.role || o.affiliation != admission.affiliation {
                    o.role = admission.role; o.affiliation = admission.affiliation;
                    let p = format!("<presence from='{}'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='{}' role='{}' jid='{}'/></x></presence>", esc(&format!("{room}/{nick}")), esc(&o.affiliation), esc(&o.role), esc(&o.full_jid));
                    for target in occupants.values() { target.send(p.clone()); }
                }
            } else {
                let removed = occupants.remove(&nick).unwrap();
                let p = unavailable(&room, &nick, false);
                removed.send(p.clone());
                for target in occupants.values() { target.send(p.clone()); }
            }
            break;
          }
        }
        Ok(())
    }
}
