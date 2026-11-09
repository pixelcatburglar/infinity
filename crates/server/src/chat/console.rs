//! Reserved private system-chat transport for the admin speak path in
//! `xmppchatclient/xmppchatsvc.py`.
//! Rooms, occupants and delivery use runtime maps, not database tables; the
//! reserved admin transport nick is distinct from display owner 1. The client
//! sees a read-only system speaker. There is no backlog or offline delivery.

use super::{esc, rooms::VIRTUAL_RESOURCE, xml_entity, Hub, Occupant};
use infinity_core::consts::OWNER_UNOWNED;

/// `xmppchatclient/xmppchatsvc.py` reserves this nick for the system speaker.
pub(super) const SYSTEM_NICK: &str = "admin";

/// `chatutil/__init__.py`: `MAX_TEXT_LEN`, applied to rendered UTF-16 text.
const MAX_TEXT_UTF16: usize = 1024;
/// Client chat rendering represents a newline with this markup.
const LINE_BREAK: &str = "<br>";
fn rendered_replacement(ch: char) -> Option<&'static str> {
    if ch == '\n' { Some(LINE_BREAK) } else { xml_entity(ch) }
}

fn rendered_units(ch: char) -> usize {
    rendered_replacement(ch).map_or_else(|| ch.len_utf16(), str::len)
}

/// Returns the recipient from a canonical system-owned private room name.
pub(crate) fn recipient(room: &str) -> Option<i64> {
    let (source, value) = room.strip_prefix("private_")?.split_once('_')?;
    if source.parse::<i64>().ok()? != OWNER_UNOWNED || source.starts_with('0')
        || value.starts_with('0') || !source.bytes().all(|c| c.is_ascii_digit())
        || !value.bytes().all(|c| c.is_ascii_digit()) { return None; }
    let id = value.parse::<i64>().ok()?;
    (id > 0).then_some(id)
}

/// Returns the canonical reserved private room for a positive character ID.
pub(crate) fn room(char_id: i64) -> String {
    format!("private_{OWNER_UNOWNED}_{char_id}")
}

/// Split raw text at UTF-16 boundaries, preferring a newline within each chunk.
fn visit_chunks<'a>(text: &'a str, mut emit: impl FnMut(&'a str)) {
    let mut start = 0;
    let mut units = 0;
    let mut newline = None;
    for (offset, ch) in text.char_indices() {
        let cost = rendered_units(ch);
        while units + cost > MAX_TEXT_UTF16 {
            let (cut, consumed) = newline.take().unwrap_or((offset, units));
            emit(&text[start..cut]);
            start = cut;
            units -= consumed;
        }
        units += cost;
        if ch == '\n' { newline = Some((offset + ch.len_utf8(), units)); }
    }
    if start < text.len() { emit(&text[start..]); }
}

fn rendered(text: &str) -> String {
    esc(text).replace('\n', LINE_BREAK)
}

impl Hub {
    /// Sends a best-effort system reply to one authenticated character.
    /// Offline characters and empty replies are silently ignored; nothing is retained.
    pub(crate) fn send_system_private(&self, char_id: i64, text: &str) {
        if char_id <= 0 || text.is_empty() { return; }
        let char_id_string = char_id.to_string();
        if !self.tokens.lock().unwrap().contains_key(&char_id_string)
            || !self.world.online_session(char_id).is_some_and(|s| s.character_id == char_id) { return; }
        let channel = room(char_id);
        let key = format!("{char_id_string}@{}", self.host);
        let Some((jid, tx)) = self.users.lock().unwrap().get(&key).cloned() else { return };
        let room_jid = format!("{channel}@{}", self.conference());
        let (invite, announce_marker) = {
            let mut rooms = self.rooms.lock().unwrap();
            let occupants = rooms.entries.entry(room_jid.clone()).or_default();
            let occupied = occupants.get(&char_id_string).is_some_and(|o| o.tx.as_ref().is_some_and(|current| current.same_channel(&tx)));
            let marker = occupants.contains_key(SYSTEM_NICK);
            if !marker {
                occupants.insert(SYSTEM_NICK.to_owned(), Occupant {
                    full_jid: format!("{SYSTEM_NICK}@{}/{VIRTUAL_RESOURCE}", self.host),
                    tx: None,
                    affiliation: "owner".into(),
                    role: "moderator".into(),
                });
            }
            (!occupied && !marker, occupied && !marker)
        };
        if announce_marker {
            let _ = tx.send(format!(
                "<presence from='{}' to='{}'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='owner' role='moderator' jid='{}@{}/{}'/></x></presence>",
                esc(&format!("{room_jid}/{SYSTEM_NICK}")),
                esc(&jid),
                SYSTEM_NICK,
                esc(&self.host),
                VIRTUAL_RESOURCE,
            ));
        }
        if invite { self.invite(char_id, &channel); }
        visit_chunks(text, |chunk| {
            let payload = serde_json::json!({"cmd":"speak", "charid": OWNER_UNOWNED, "messageText": rendered(chunk)}).to_string();
            let stanza = format!("<message from='{}' to='{}' type='groupchat'><body>{}</body></message>", esc(&room_jid), esc(&jid), esc(&payload));
            let _ = tx.send(stanza);
        });
    }
}

#[cfg(test)]
mod tests;
