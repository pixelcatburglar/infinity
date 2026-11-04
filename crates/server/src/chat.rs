//! XMPP transport for `xmppchatclient/xmppchatsvc.py` and `xmppchannelsettings.py`.
//! Token-authenticated SASL, binding, presence, MUC discovery and messages use
//! plain TCP (the launcher disables TLS). Durable room metadata and ACLs are
//! supplied by chat services and their DAO; real and virtual occupants are runtime
//! state. Virtual occupants join system rooms only, not durable custom rooms.
//! Password rooms, corporation/alliance ACLs and proxy ownership are unsupported.

use crate::consts::TYPE_CHARACTER;
use anyhow::Result;
use base64::Engine;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

mod rooms;
mod stanzas;
#[cfg(test)]
mod tests;

use crate::{db::Db, space::World};
use stanzas::stanza;

/// RFC 8259: JSON control characters below U+0020 require escaping.
const JSON_CONTROL_LIMIT: u32 = 0x20;
/// XEP-0045 status code identifying the recipient's own presence.
const MUC_SELF_PRESENCE: u16 = 110;

/// A parsed XML element (one stanza or a child of one).
#[derive(Debug, Clone, Default)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }
    fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }
    /// Serialise, optionally overriding attributes.
    fn to_xml_with(&self, overrides: &[(&str, &str)]) -> String {
        let mut s = format!("<{}", self.name);
        for (k, v) in &self.attrs {
            if overrides.iter().any(|(o, _)| o == k) {
                continue;
            }
            s.push_str(&format!(" {k}='{}'", esc(v)));
        }
        for (k, v) in overrides {
            s.push_str(&format!(" {k}='{}'", esc(v)));
        }
        if self.children.is_empty() && self.text.is_empty() {
            s.push_str("/>");
            return s;
        }
        s.push('>');
        s.push_str(&esc(&self.text));
        for c in &self.children {
            s.push_str(&c.to_xml_with(&[]));
        }
        s.push_str(&format!("</{}>", self.name));
        s
    }
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\'', "&apos;").replace('"', "&quot;")
}

type Tx = mpsc::UnboundedSender<String>;

#[derive(Clone)]
struct Occupant {
    full_jid: String,
    tx: Option<Tx>,
    affiliation: String,
    role: String,
}

impl Occupant {
    fn send(&self, stanza: String) {
        if let Some(tx) = &self.tx { let _ = tx.send(stanza); }
    }

    fn matches_socket(&self, jid: &str, tx: &Tx) -> bool {
        self.full_jid == jid && self.tx.as_ref().is_some_and(|current| current.same_channel(tx))
    }

    fn same_identity(&self, other: &Self) -> bool {
        self.full_jid == other.full_jid && match (&self.tx, &other.tx) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same_channel(b),
            _ => false,
        }
    }
}

#[derive(Default)]
struct Rooms {
    epoch: u64,
    entries: HashMap<String, HashMap<String, Occupant>>,
}

struct Profile {
    info: String,
    corp: i64,
    alliance: Option<i64>,
    faction: Option<i64>,
    role: i64,
}

impl Profile {
    fn xml(&self) -> String {
        format!("<eve_user_data corpid='{}' allianceid='{}' warfactionid='{}' typeid='{TYPE_CHARACTER}' role='{}' info='{}'/>",
            self.corp, self.alliance.unwrap_or(0), self.faction.unwrap_or(0), self.role, esc(&self.info))
    }
}

/// Shared chat state: auth tokens, connected users, rooms.
pub struct Hub {
    pub host: String,
    db: Db,
    world: Arc<World>,
    tokens: Mutex<HashMap<String, String>>,
    users: Mutex<HashMap<String, (String, Tx)>>,
    rooms: Mutex<Rooms>,
    /// charID -> `<eve_user_data .../>` sent with presences and user data queries.
    profiles: Mutex<HashMap<String, Profile>>,
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < JSON_CONTROL_LIMIT => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Hub {
    pub fn new(host: &str, db: Db, world: Arc<World>) -> Arc<Hub> {
        Arc::new(Hub { host: host.to_string(), db, world, tokens: Mutex::new(HashMap::new()),
            users: Mutex::new(HashMap::new()), rooms: Mutex::new(Rooms::default()), profiles: Mutex::new(HashMap::new()) })
    }

    /// Issue (or reuse) the chat password for a character.
    pub fn token_for(&self, char_id: i64) -> String {
        let mut t = self.tokens.lock().unwrap();
        t.entry(char_id.to_string()).or_insert_with(|| format!("{:016x}", crate::handshake::rand_i64())).clone()
    }

    fn check(&self, user: &str, password: &str) -> bool {
        self.tokens.lock().unwrap().get(user).is_some_and(|t| t == password)
    }

    /// What chat shows about a character (xmppchatsvc._GetPresenceUserData): affiliations,
    /// session role, and an eveowners row `[ownerID, ownerName, typeID, gender, ownerNameID]`.
    pub fn set_profile(&self, char_id: i64, name: &str, gender: i64, corp: i64, alliance: Option<i64>, faction: Option<i64>, role: i64) {
        let info = format!("[{char_id}, {}, {TYPE_CHARACTER}, {gender}, null]", json_str(name));
        self.profiles.lock().unwrap().insert(char_id.to_string(), Profile { info, corp, alliance, faction, role });
    }

    fn profile(&self, user: &str) -> String {
        self.profiles.lock().unwrap().get(user).map(Profile::xml).unwrap_or_default()
    }

    fn conference(&self) -> String {
        format!("conference.{}", self.host)
    }

    /// Invite a character into a session channel (`local_<systemID>`...) after a
    /// session change; invites from `admin` are joined without asking (xmppchatsvc._HandleInvite).
    pub fn invite(&self, char_id: i64, channel: &str) {
        let users = self.users.lock().unwrap();
        let Some((jid, tx)) = users.get(&format!("{char_id}@{}", self.host)) else { return };
        let _ = tx.send(format!(
            "<message from='{}' to='{}'><x xmlns='http://jabber.org/protocol/muc#user'><invite from='{}'/></x></message>",
            esc(&format!("{channel}@{}", self.conference())),
            esc(jid),
            esc(&format!("admin@{}", self.host))
        ));
    }

}

pub async fn serve(bind: String, hub: Arc<Hub>) -> Result<()> {
    let listener = TcpListener::bind(&bind).await?;
    info!("chat (XMPP) listening on {bind}, domain {}", hub.host);
    loop {
        let (stream, peer) = listener.accept().await?;
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Err(e) = connection(stream, hub).await {
                debug!("chat connection {peer} ended: {e:#}");
            }
        });
    }
}

struct Conn {
    hub: Arc<Hub>,
    tx: Tx,
    user: Option<String>,
    full_jid: Option<String>,
    password: Option<String>,
}

async fn connection(stream: TcpStream, hub: Arc<Hub>) -> Result<()> {
    let (rd, mut wr) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(s) = rx.recv().await {
            if wr.write_all(s.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let mut c = Conn { hub: hub.clone(), tx, user: None, full_jid: None, password: None };
    let result = read_loop(rd, &mut c).await;
    if let Some(jid) = &c.full_jid {
        hub.leave_all(jid, &c.tx);
        if let Some(user) = &c.user {
            let mut users = hub.users.lock().unwrap();
            let key = format!("{user}@{}", hub.host);
            if users.get(&key).is_some_and(|(current, tx)| current == jid && tx.same_channel(&c.tx)) { users.remove(&key); }
        }
        info!("chat: {jid} disconnected");
    }
    writer.abort();
    result
}

async fn read_loop(rd: tokio::net::tcp::OwnedReadHalf, c: &mut Conn) -> Result<()> {
    let mut reader = quick_xml::Reader::from_reader(BufReader::new(rd));
    reader.config_mut().check_end_names = false;
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut stack: Vec<Element> = Vec::new();
    loop {
        buf.clear();
        let ev = reader.read_event_into_async(&mut buf).await?;
        match ev {
            Event::Eof => return Ok(()),
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == b"stream:stream" => {
                stack.clear();
                stream_opened(c);
            }
            Event::Start(e) => stack.push(element_from(&e)?),
            Event::Empty(e) => {
                let el = element_from(&e)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => stanza(c, el).await?,
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.unescape()?);
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::End(e) => {
                if e.name().as_ref() == b"stream:stream" {
                    let _ = c.tx.send("</stream:stream>".into());
                    return Ok(());
                }
                if let Some(el) = stack.pop() {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(el),
                        None => stanza(c, el).await?,
                    }
                }
            }
            _ => {}
        }
    }
}

fn element_from(e: &quick_xml::events::BytesStart<'_>) -> Result<Element> {
    let mut el = Element { name: String::from_utf8_lossy(e.name().as_ref()).into_owned(), ..Default::default() };
    for a in e.attributes().flatten() {
        el.attrs.push((String::from_utf8_lossy(a.key.as_ref()).into_owned(), a.unescape_value()?.into_owned()));
    }
    Ok(el)
}

fn stream_opened(c: &mut Conn) {
    let host = &c.hub.host;
    let id = format!("{:x}", crate::handshake::rand_i64());
    let mut s = format!(
        "<?xml version='1.0'?><stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' id='{id}' from='{host}' version='1.0' xml:lang='en'>"
    );
    if c.user.is_none() {
        s.push_str("<stream:features><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>PLAIN</mechanism></mechanisms></stream:features>");
    } else {
        s.push_str("<stream:features><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/><session xmlns='urn:ietf:params:xml:ns:xmpp-session'/></stream:features>");
    }
    let _ = c.tx.send(s);
}


fn auth(c: &mut Conn, el: &Element) -> Result<()> {
    let decoded = base64::engine::general_purpose::STANDARD.decode(el.text.trim()).unwrap_or_default();
    let parts: Vec<&[u8]> = decoded.split(|b| *b == 0).collect();
    let (user, pass) = match parts.as_slice() {
        [_, u, p] => (String::from_utf8_lossy(u).into_owned(), String::from_utf8_lossy(p).into_owned()),
        _ => (String::new(), String::new()),
    };
    if c.hub.check(&user, &pass) {
        c.user = Some(user);
        c.password = Some(pass);
        let _ = c.tx.send("<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>".into());
    } else {
        warn!("chat: auth failed for {user:?}");
        let _ = c.tx.send("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>".into());
    }
    Ok(())
}

