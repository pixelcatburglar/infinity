//! XMPP MUC wire handling for `xmppchatsvc.py` and `xmppchannelsettings.py`.
//! Chat services authorize durable DAO-backed room data; occupants and their
//! cached admission roles are runtime state. Password rooms, corporate/alliance
//! ACLs and proxy ownership are unsupported.
use super::*;
use infinity_core::messages::chat as text;
use crate::{dao::chat_channels::Restriction, services::{chat::{authority, expiry}, CallError}};

const OWNER: &str = "http://jabber.org/protocol/muc#owner";
const ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const USER: &str = "http://jabber.org/protocol/muc#user";
const ITEMS: &str = "http://jabber.org/protocol/disco#items";
const INFO: &str = "http://jabber.org/protocol/disco#info";
/// `xmppchatsvc.py`: timed mute/ban mutation and deadline searches.
const EXPIRY: &str = "urn:xmpp:expiring_record";
const EXPIRY_SEARCH: &str = "urn:xmpp:expiring_record#search";
/// XEP-0009 legacy error codes consumed by `xmppchatsvc.py:GetErrorCode`.
const ERROR_BAD_REQUEST: u16 = 400;
const ERROR_NOT_AUTHORIZED: u16 = 401;
const ERROR_FORBIDDEN: u16 = 403;
const ERROR_CONFLICT: u16 = 409;
const ERROR_INTERNAL: u16 = 500;
const ERROR_UNSUPPORTED: u16 = 501;
/// XEP-0004 room configuration form type.
const ROOM_CONFIG_FORM: &str = "http://jabber.org/protocol/muc#roomconfig";
/// Percent-encoded bytes use hexadecimal digits (RFC 3986).
const HEX_RADIX: u32 = 16;

pub(super) async fn stanza(c: &mut Conn, el: Element) -> Result<()> {
    match el.local() {
        "auth" => auth(c, &el),
        "iq" => iq(c, &el).await,
        "presence" => presence(c, &el).await,
        "message" => message(c, &el).await,
        _ => Ok(()),
    }
}

fn channel<'a>(hub: &Hub, jid: &'a str) -> Option<&'a str> {
    let (bare, _) = jid.split_once('/').unwrap_or((jid, ""));
    let (name, domain) = bare.split_once('@')?;
    if domain != hub.conference() || name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') { return None; }
    Some(name)
}
fn session(c: &Conn) -> Option<infinity_core::session::OnlineSession> {
    let id = c.user.as_deref()?.parse::<i64>().ok()?;
    c.full_jid.as_ref()?;
    if !c.hub.check(c.user.as_deref()?, c.password.as_deref()?) { return None; }
    c.hub.world.online_session(id).filter(|s| s.character_id == id)
}
fn reply(c: &Conn, el: &Element, payload: &str) {
    let _ = c.tx.send(format!("<iq type='result' id='{}' from='{}' to='{}'>{payload}</iq>", esc(el.attr("id").unwrap_or("")), esc(el.attr("to").unwrap_or(&c.hub.host)), esc(c.full_jid.as_deref().unwrap_or(""))));
}
fn error(c: &Conn, el: &Element, condition: &str, description: &str) {
    let code = match condition { "bad-request" => ERROR_BAD_REQUEST, "not-authorized" => ERROR_NOT_AUTHORIZED, "forbidden" => ERROR_FORBIDDEN, "conflict" => ERROR_CONFLICT, "internal-server-error" => ERROR_INTERNAL, _ => ERROR_UNSUPPORTED };
    let _ = c.tx.send(format!("<{} type='error' id='{}' from='{}' to='{}'><error type='cancel' code='{code}'><{} xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/><text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>{}</text></error></{}>", el.local(), esc(el.attr("id").unwrap_or("")), esc(el.attr("to").unwrap_or(&c.hub.host)), esc(c.full_jid.as_deref().unwrap_or("")), condition, esc(description), el.local()));
}
fn domain_error(c: &Conn, el: &Element, failure: CallError) {
    match failure {
        CallError::User { dict, .. } => {
            let message = dict.iter().find(|(key, _)| key == "notify").and_then(|(_, value)| value.as_text());
            error(c, el, "forbidden", message.as_deref().unwrap_or(text::FORBIDDEN));
        }
        CallError::Internal(failure) => { warn!("chat domain failure: {failure:#}"); error(c, el, "internal-server-error", ""); }
    }
}
fn target(hub: &Hub, jid: &str) -> Option<i64> {
    let bare = jid.split('/').next()?;
    let (id, domain) = bare.split_once('@')?;
    if domain != hub.host { return None; }
    let value = id.parse::<i64>().ok()?;
    (value > 0 && id == value.to_string()).then_some(value)
}
fn field<'a>(form: &'a Element, name: &str) -> Option<&'a str> {
    form.children.iter().find(|f| f.local() == "field" && f.attr("var") == Some(name)).and_then(|f| f.child("value")).map(|v| v.text.as_str())
}
fn form_field(name: &str, kind: &str, value: &str) -> String { format!("<field var='{}' type='{}'><value>{}</value></field>", esc(name), esc(kind), esc(value)) }
fn bool_value(value: &str) -> Option<bool> { match value { "1" | "true" => Some(true), "0" | "false" => Some(false), _ => None } }

async fn iq(c: &mut Conn, el: &Element) -> Result<()> {
    let Some(user) = c.user.clone() else { error(c, el, "not-authorized", ""); return Ok(()) };
    if let Some(bind) = el.child("bind").filter(|e| e.attr("xmlns") == Some("urn:ietf:params:xml:ns:xmpp-bind")) {
        if !c.password.as_deref().is_some_and(|password| c.hub.check(&user, password)) || user.parse::<i64>().ok().and_then(|id| c.hub.world.online_session(id).filter(|s| s.character_id == id)).is_none() { error(c, el, "not-authorized", ""); return Ok(()); }
        let resource = bind.child("resource").map(|r| r.text.trim()).filter(|r| !r.is_empty()).unwrap_or("eve");
        let jid = format!("{user}@{}/{resource}", c.hub.host);
        c.hub.users.lock().unwrap().insert(format!("{user}@{}", c.hub.host), (jid.clone(), c.tx.clone()));
        c.full_jid = Some(jid.clone());
        reply(c, el, &format!("<bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><jid>{}</jid></bind>", esc(&jid)));
        return Ok(());
    }
    let Some(s) = session(c) else { error(c, el, "not-authorized", ""); return Ok(()) };
    let to = el.attr("to").unwrap_or(&c.hub.host);
    let Some(q) = el.child("query") else {
        if el.child("session").is_some_and(|x| x.attr("xmlns") == Some("urn:ietf:params:xml:ns:xmpp-session")) || el.child("ping").is_some_and(|x| x.attr("xmlns") == Some("urn:xmpp:ping")) { reply(c, el, ""); } else { error(c, el, "feature-not-implemented", ""); }
        return Ok(());
    };
    let ns = q.attr("xmlns").unwrap_or("");
    let kind = el.attr("type").unwrap_or("");
    if ns == "urn:xmpp:eve_user_data" && kind == "get" {
        let who = q.attr("jid").unwrap_or("").split('@').next().unwrap_or("");
        reply(c, el, &c.hub.profile(who)); return Ok(());
    }
    if (ns == EXPIRY && kind == "set") || (ns == EXPIRY_SEARCH && kind == "get") {
        if to != c.hub.host { error(c, el, "bad-request", ""); return Ok(()); }
        let Some(name) = q.attr("room").and_then(|room| channel(&c.hub, room)).filter(|_| q.attr("room").is_some_and(|room| !room.contains('/'))) else { error(c, el, "bad-request", ""); return Ok(()) };
        let restriction = match q.attr("category") { Some("mute") => Restriction::Mute, Some("ban") => Restriction::Ban, _ => { error(c, el, "bad-request", ""); return Ok(()) } };
        if ns == EXPIRY_SEARCH {
            match expiry::list(&c.hub.db, &s, name, restriction).await {
                Ok(ids) => { let mut out = format!("<query xmlns='{EXPIRY_SEARCH}'>"); for id in ids { out.push_str(&format!("<item jid='{}@{}'/>", id, esc(&c.hub.host))); } out.push_str("</query>"); reply(c, el, &out); }
                Err(e) => domain_error(c, el, e),
            }
        } else {
            let Some(id) = q.attr("jid").and_then(|jid| target(&c.hub, jid)).filter(|_| q.attr("jid").is_some_and(|jid| !jid.contains('/'))) else { error(c, el, "bad-request", ""); return Ok(()) };
            let result = match q.attr("action") {
                Some("set") => { let Some(duration) = q.attr("duration").and_then(|v| v.parse::<i64>().ok()).filter(|v| *v > 0) else { error(c, el, "bad-request", ""); return Ok(()) }; expiry::set(&c.hub.db, &c.hub, &s, name, id, restriction, duration).await }
                Some("delete") => expiry::remove(&c.hub.db, &c.hub, &s, name, id, restriction).await,
                _ => { error(c, el, "bad-request", ""); return Ok(()) }
            };
            match result { Ok(()) => reply(c, el, ""), Err(e) => domain_error(c, el, e) }
        }
        return Ok(());
    }
    if ns == ITEMS && kind == "get" && to == c.hub.conference() {
        let node = q.attr("node").unwrap_or("");
        if node != "forme" && !node.starts_with("byname/") { error(c, el, "feature-not-implemented", ""); return Ok(()); }
        match authority::disco_list(&c.hub.db, &s).await {
            Ok(list) => {
                let wanted = node.strip_prefix("byname/").and_then(percent_decode);
                if node.starts_with("byname/") && wanted.is_none() { error(c, el, "bad-request", ""); return Ok(()); }
                let mut out = format!("<query xmlns='{ITEMS}' node='{}'>", esc(node));
                for r in list.into_iter().filter(|r| wanted.as_ref().is_none_or(|w| &r.display_name == w)) { out.push_str(&format!("<item jid='{}' name='{}' node='{}'/>", esc(&format!("{}@{}", r.name, c.hub.conference())), esc(&r.display_name), esc(&r.affiliation))); }
                out.push_str("</query>"); reply(c, el, &out);
            }
            Err(e) => domain_error(c, el, e),
        }
        return Ok(());
    }
    let name = if ns == INFO && kind == "get" && to == c.hub.conference() {
        q.attr("node").filter(|node| channel(&c.hub, &format!("{node}@{}", c.hub.conference())) == Some(*node))
    } else { channel(&c.hub, to) };
    let Some(name) = name else { error(c, el, "feature-not-implemented", ""); return Ok(()) };
    if ns == INFO && kind == "get" {
        match authority::disco_info(&c.hub.db, c.hub.world.sde(), &s, name).await {
            Ok(Some(info)) => {
                let room = format!("{name}@{}", c.hub.conference());
                let count = c.hub.rooms.lock().unwrap().entries.get(&room).map_or(0, |o| o.len());
                let node = q.attr("node").map(|v| format!(" node='{}'", esc(v))).unwrap_or_default();
                reply(c, el, &format!("<query xmlns='{INFO}'{node}><identity category='conference' type='text' name='{}'/><x xmlns='jabber:x:data' type='result'>{}</x></query>", esc(&info.display_name), form_field("muc#roominfo_occupants", "text-single", &count.to_string())));
            }
            Ok(None) => error(c, el, "forbidden", text::FORBIDDEN),
            Err(e) => domain_error(c, el, e),
        }
        return Ok(());
    }
    if ns == ITEMS && kind == "get" && q.attr("node") == Some("norole") {
        match authority::admit(&c.hub.db, c.hub.world.sde(), &s, name).await {
            Ok(Some(_)) => { let mut out = format!("<query xmlns='{ITEMS}' node='norole'>"); let room = format!("{name}@{}", c.hub.conference()); if let Some(occupants) = c.hub.rooms.lock().unwrap().entries.get(&room) { for (nick, o) in occupants { if o.affiliation == "none" { out.push_str(&format!("<item name='{}'/>", esc(nick))); } } } out.push_str("</query>"); reply(c, el, &out); }
            Ok(None) => error(c, el, "forbidden", text::FORBIDDEN),
            Err(e) => domain_error(c, el, e),
        }
        return Ok(());
    }
    if ns == OWNER {
        if kind == "set" && q.child("destroy").is_some() {
            match authority::destroy(&c.hub.db, &c.hub, &s, name).await { Ok(()) => reply(c, el, ""), Err(e) => domain_error(c, el, e) }
            return Ok(());
        }
        if kind == "get" {
            match authority::room_config(&c.hub.db, &s, name).await {
                Ok(config) => {
                    let mut out = format!("<query xmlns='{OWNER}'><x xmlns='jabber:x:data' type='form'>");
                    out.push_str(&form_field("FORM_TYPE", "hidden", ROOM_CONFIG_FORM));
                    out.push_str(&form_field("muc#roomconfig_roomname", "text-single", &config.display_name));
                    for (key, value) in [("muc#roomconfig_membersonly", !config.public), ("muc#roomconfig_moderatedroom", config.moderated), ("members_by_default", config.members_by_default), ("muc#roomconfig_allowinvites", config.public), ("muc#roomconfig_passwordprotectedroom", false)] { out.push_str(&form_field(key, "boolean", if value { "1" } else { "0" })); }
                    out.push_str("</x></query>"); reply(c, el, &out);
                }
                Err(e) => domain_error(c, el, e),
            }
            return Ok(());
        }
        if kind == "set" {
            let Some(form) = q.child("x").filter(|x| x.attr("xmlns") == Some("jabber:x:data") && x.attr("type") == Some("submit")) else { error(c, el, "feature-not-implemented", ""); return Ok(()) };
            if field(form, "FORM_TYPE") != Some(ROOM_CONFIG_FORM) || form.children.iter().filter(|f| f.local() == "field").any(|f| f.attr("var").is_none() || f.child("value").is_none() || form.children.iter().filter(|other| other.local() == "field" && other.attr("var") == f.attr("var")).count() != 1) { error(c, el, "bad-request", ""); return Ok(()) }
            if field(form, "muc#roomconfig_roomsecret").is_some_and(|v| !v.is_empty()) { error(c, el, "feature-not-implemented", text::UNSUPPORTED_PASSWORD); return Ok(()); }
            if let Some(v) = field(form, "muc#roomconfig_passwordprotectedroom") { match bool_value(v) { Some(false) => (), Some(true) => { error(c, el, "feature-not-implemented", text::UNSUPPORTED_PASSWORD); return Ok(()) }, None => { error(c, el, "bad-request", ""); return Ok(()) } } }
            if form.children.iter().filter(|f| f.local() == "field").any(|f| !matches!(f.attr("var"), Some("FORM_TYPE" | "muc#roomconfig_roomname" | "muc#roomconfig_membersonly" | "muc#roomconfig_moderatedroom" | "members_by_default" | "muc#roomconfig_allowinvites" | "muc#roomconfig_passwordprotectedroom" | "muc#roomconfig_roomsecret"))) { error(c, el, "feature-not-implemented", ""); return Ok(()); }
            let mut config = match authority::room_config(&c.hub.db, &s, name).await { Ok(config) => config, Err(e) => { domain_error(c, el, e); return Ok(()) } };
            if let Some(v) = field(form, "muc#roomconfig_roomname") { config.display_name = v.to_owned(); }
            for (key, dest, invert) in [("muc#roomconfig_membersonly", &mut config.public, true), ("muc#roomconfig_moderatedroom", &mut config.moderated, false), ("members_by_default", &mut config.members_by_default, false)] {
                if let Some(v) = field(form, key) { let Some(value) = bool_value(v) else { error(c, el, "bad-request", ""); return Ok(()) }; *dest = value ^ invert; }
            }
            if let Some(v) = field(form, "muc#roomconfig_allowinvites") { let Some(value) = bool_value(v) else { error(c, el, "bad-request", ""); return Ok(()) }; if value != config.public { error(c, el, "feature-not-implemented", ""); return Ok(()) } }
            match authority::configure(&c.hub.db, &c.hub, &s, name, config).await { Ok(()) => reply(c, el, ""), Err(e) => domain_error(c, el, e) }
            return Ok(());
        }
    }
    if ns == ADMIN {
        let Some(item) = q.child("item") else { error(c, el, "bad-request", ""); return Ok(()) };
        if kind == "get" {
            if let Some(role) = item.attr("role") {
                if !["moderator", "participant", "visitor", "none"].contains(&role) || item.attr("affiliation").is_some() { error(c, el, "feature-not-implemented", ""); return Ok(()) }
                match authority::admit(&c.hub.db, c.hub.world.sde(), &s, name).await {
                    Ok(Some(_)) => { let mut out = format!("<query xmlns='{ADMIN}'>"); let room = format!("{name}@{}", c.hub.conference()); if let Some(occupants) = c.hub.rooms.lock().unwrap().entries.get(&room) { for o in occupants.values().filter(|o| o.role == role) { out.push_str(&format!("<item jid='{}' affiliation='{}' role='{}'/>", esc(&o.full_jid), esc(&o.affiliation), esc(&o.role))); } } out.push_str("</query>"); reply(c, el, &out); }
                    Ok(None) => error(c, el, "forbidden", text::FORBIDDEN),
                    Err(e) => domain_error(c, el, e),
                }
            } else if let Some(affiliation) = item.attr("affiliation").filter(|v| ["owner", "admin", "member", "outcast", "none"].contains(v)) {
                match authority::affiliations(&c.hub.db, &s, name).await {
                    Ok(list) => { let mut out = format!("<query xmlns='{ADMIN}'>"); for a in list.into_iter().filter(|a| a.affiliation == affiliation) { out.push_str(&format!("<item jid='{}@{}' affiliation='{}'/>", a.character_id, esc(&c.hub.host), esc(&a.affiliation))); } out.push_str("</query>"); reply(c, el, &out); }
                    Err(e) => domain_error(c, el, e),
                }
            } else { error(c, el, "feature-not-implemented", ""); }
            return Ok(());
        }
        if kind == "set" {
            if item.child("reason").is_some_and(|r| r.text == "corporation" || r.text == "alliance") { error(c, el, "feature-not-implemented", text::UNSUPPORTED_AFFILIATION); return Ok(()); }
            let target_id = item.attr("jid").and_then(|jid| target(&c.hub, jid)).or_else(|| item.attr("nick").and_then(|v| v.parse::<i64>().ok()).filter(|id| *id > 0));
            let Some(id) = target_id else { error(c, el, "bad-request", ""); return Ok(()) };
            let result = if let Some(aff) = item.attr("affiliation") { authority::set_affiliation(&c.hub.db, &c.hub, &s, name, id, aff).await } else if let Some(role) = item.attr("role") { authority::set_role(&c.hub.db, &c.hub, &s, name, id, role).await } else { error(c, el, "feature-not-implemented", ""); return Ok(()) };
            match result { Ok(()) => reply(c, el, ""), Err(e) => domain_error(c, el, e) }
            return Ok(());
        }
    }
    error(c, el, "feature-not-implemented", ""); Ok(())
}

fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len()); let bytes = s.as_bytes(); let mut i = 0;
    while i < bytes.len() { if bytes[i] == b'%' { let hex = std::str::from_utf8(bytes.get(i+1..i+3)?).ok()?; out.push(u8::from_str_radix(hex, HEX_RADIX).ok()?); i += 3; } else { out.push(bytes[i]); i += 1; } }
    String::from_utf8(out).ok()
}

async fn presence(c: &mut Conn, el: &Element) -> Result<()> {
    let Some(full) = c.full_jid.clone() else { if el.attr("to").is_some() { error(c, el, "not-authorized", ""); } return Ok(()) };
    let Some(to) = el.attr("to") else { if el.attr("type").is_none() { let _ = c.tx.send(format!("<presence from='{0}' to='{0}'/>", esc(&full))); } return Ok(()) };
    let Some(name) = channel(&c.hub, to) else { error(c, el, "forbidden", text::FORBIDDEN); return Ok(()) };
    let Some(s) = session(c) else { error(c, el, "not-authorized", ""); return Ok(()) };
    let (room, nick) = to.split_once('/').unwrap_or((to, ""));
    if nick != s.character_id.to_string() { error(c, el, "forbidden", text::FORBIDDEN); return Ok(()) }
    if el.attr("type") == Some("unavailable") {
        c.hub.leave_room(room, &full, &c.tx);
        return Ok(());
    }
    if el.attr("type").is_some() { error(c, el, "feature-not-implemented", ""); return Ok(()) }
    if el.child("x").and_then(|x| x.child("password")).is_some_and(|p| !p.text.is_empty()) { error(c, el, "feature-not-implemented", text::UNSUPPORTED_PASSWORD); return Ok(()) }
    match c.hub.join_room(&s, name, &full, Some((&c.tx, el.attr("id").unwrap_or("")))).await {
        Ok(_) => (),
        Err(rooms::JoinError::Forbidden) => error(c, el, "forbidden", text::FORBIDDEN),
        Err(rooms::JoinError::Conflict) => error(c, el, "conflict", ""),
        Err(rooms::JoinError::Domain(e)) => domain_error(c, el, e),
    }
    Ok(())
}

async fn message(c: &mut Conn, el: &Element) -> Result<()> {
    let Some(full) = c.full_jid.clone() else { if el.attr("to").is_some() { error(c, el, "not-authorized", ""); } return Ok(()) };
    let Some(s) = session(c) else { error(c, el, "not-authorized", ""); return Ok(()) };
    let Some(to) = el.attr("to") else { return Ok(()) };
    if let Some(name) = channel(&c.hub, to) {
        let room = to.split('/').next().unwrap_or(to);
        let epoch = c.hub.rooms.lock().unwrap().epoch;
        let admitted = authority::admit(&c.hub.db, c.hub.world.sde(), &s, name).await;
        let admitted = match admitted { Ok(Some(admitted)) => admitted, Ok(None) => { error(c, el, "forbidden", text::FORBIDDEN); return Ok(()) }, Err(e) => { domain_error(c, el, e); return Ok(()) } };
        if let Some(decline) = el.child("x").filter(|x| x.attr("xmlns") == Some(USER)).and_then(|x| x.child("decline")) {
            let rooms = c.hub.rooms.lock().unwrap();
            if rooms.epoch != epoch || c.hub.world.online_session(s.character_id).as_ref() != Some(&s) { error(c, el, "forbidden", text::FORBIDDEN); return Ok(()) }
            let Some(id) = decline.attr("to").and_then(|v| target(&c.hub, v)) else { error(c, el, "bad-request", ""); return Ok(()) };
            let key = format!("{id}@{}", c.hub.host);
            if let Some((jid, tx)) = c.hub.users.lock().unwrap().get(&key) {
                let reason = decline.child("reason").map_or("", |r| r.text.as_str());
                let _ = tx.send(format!("<message from='{}' to='{}'><x xmlns='{USER}'><decline from='{}'><reason>{}</reason></decline></x></message>", esc(room), esc(jid), esc(&full), esc(reason)));
            }
            return Ok(());
        }
        let occupied = { let rooms = c.hub.rooms.lock().unwrap(); rooms.epoch == epoch && c.hub.world.online_session(s.character_id).as_ref() == Some(&s) && rooms.entries.get(room).and_then(|o| o.get(&s.character_id.to_string())).is_some_and(|o| o.matches_socket(&full, &c.tx)) };
        if !occupied { error(c, el, "forbidden", text::FORBIDDEN); return Ok(()) }
        if let Some(x) = el.child("x").filter(|x| x.attr("xmlns") == Some(USER)) {
            if let Some(invite) = x.child("invite") {
                let Some(id) = invite.attr("to").and_then(|v| target(&c.hub, v)) else { error(c, el, "bad-request", ""); return Ok(()) };
                if let Err(e) = authority::invite(&c.hub.db, &c.hub, &s, name, id).await { domain_error(c, el, e); }
                return Ok(());
            }
            error(c, el, "feature-not-implemented", ""); return Ok(());
        }
        if el.attr("type") == Some("groupchat") {
            if let Some(subject) = el.child("subject") { if let Err(e) = authority::set_subject(&c.hub.db, &c.hub, &s, name, &subject.text).await { domain_error(c, el, e); } return Ok(()) }
            if admitted.role == "visitor" || admitted.role == "none" { error(c, el, "forbidden", text::FORBIDDEN); return Ok(()) }
            let rooms = c.hub.rooms.lock().unwrap();
            if rooms.epoch != epoch || c.hub.world.online_session(s.character_id).as_ref() != Some(&s) { error(c, el, "forbidden", text::FORBIDDEN); return Ok(()) }
            if let Some(occ) = rooms.entries.get(room) { if occ.get(&s.character_id.to_string()).is_some_and(|o| o.matches_socket(&full, &c.tx)) { let from = format!("{room}/{}", s.character_id); for o in occ.values() { o.send(el.to_xml_with(&[("from", &from), ("to", &o.full_jid)])); } } }
        } else { error(c, el, "feature-not-implemented", ""); }
        return Ok(());
    }
    if el.children.iter().any(|x| x.local() == "x" && x.attr("xmlns") == Some(USER)) { error(c, el, "forbidden", text::FORBIDDEN); return Ok(()); }
    let bare = to.split('/').next().unwrap_or(to);
    if target(&c.hub, bare).is_some() { if let Some((jid, tx)) = c.hub.users.lock().unwrap().get(bare) { let _ = tx.send(el.to_xml_with(&[("from", &full), ("to", jid)])); } }
    Ok(())
}
