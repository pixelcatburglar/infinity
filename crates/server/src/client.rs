//! One logged-in client's machoNet packet loop and ordered cleanup.
//! `carbon/common/script/net/machoNet.py` defines transport usurp behavior.
//! Session state uses the game database and in-memory world; account ownership
//! is process-local and does not coordinate separate server processes.
//! A successful disconnect cleanup hands off the selected character's logoff event.
//! Actor-timed transitions flush queued notifications and forward destiny batches while waiting.

use crate::conn::{Conn, Writer};
use crate::consts::{FILETIME_DAY, FILETIME_HOUR, FILETIME_SECOND, FILETIME_UNIX_EPOCH};
use crate::db::Db;
use crate::db::outbox::{dispatch_client, Outbox};
use crate::lua::events::GameEvent;
use crate::handshake::Login;
use crate::macho::{cmd, Address, Packet, ERR_WRAPPEDEXCEPTION};
use crate::services::{self, identity::Identity, presence, Call, CallError};
use crate::session::Session;
use crate::config::Config;
use anyhow::{anyhow, Result};
use marshal::Value;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Windows FILETIME uses 100 ns ticks, hence 10,000 ticks per millisecond.
const FILETIME_TICKS_PER_MS: i64 = 10_000;
/// Windows FILETIME tick duration in nanoseconds.
const FILETIME_TICK_NS: u32 = 100;
/// Internal pilot-channel request to recompile a brain.
pub(crate) const BRAIN_REFRESH: &str = "__brain";

pub struct Client {
    pub conn: Writer,
    inbox: mpsc::UnboundedReceiver<Result<Option<Value>>>,
    pub world: Arc<crate::space::World>,
    /// Destiny events pushed by the solar system this client observes.
    pub destiny_tx: crate::space::DestinyTx,
    destiny_rx: mpsc::UnboundedReceiver<Vec<Value>>,
    /// Notifications other tasks address to this client's character (crate::space::World::register_pilot).
    pub notify_tx: crate::space::NotifyTx,
    notify_rx: mpsc::UnboundedReceiver<(String, Vec<Value>)>,
    pub cfg: Arc<Config>,
    pub db: Db,
    pub sde: Arc<crate::sde::Sde>,
    pub chat: std::sync::Arc<crate::chat::Hub>,
    pub login: Login,
    pub session: Session,
    /// Packets queued by handlers (session changes, notifications) to send after the response.
    pub outbox: Vec<Packet>,
    /// Bound objects handed to this client: OID -> (service, bindParams).
    pub bound: std::collections::HashMap<String, (String, Value)>,
    next_oid: i64,
}

impl crate::services::ship::observer::DockObserver for Client {
    fn accepted(&mut self, station: i64) {
        self.notify("OnDockingAccepted", vec![Value::Int(station)]);
    }

    async fn wait_pumping(&mut self, system: i64, delay_ms: u64) -> std::result::Result<bool, CallError> {
        Client::wait_pumping(self, system, delay_ms).await.map_err(CallError::Internal)
    }

    fn unobserve(&mut self) {
        self.world.unobserve(self.login.client_id);
    }
}

/// Abbreviated repr for logs.
pub fn short(v: &Value) -> String {
    let s = format!("{v:?}");
    if s.len() > 600 {
        format!("{}... ({} chars)", &s[..s.char_indices().take_while(|(i, _)| *i < 600).last().map(|(i, _)| i).unwrap_or(0)], s.len())
    } else {
        s
    }
}

impl Client {
    pub fn new(conn: Conn, cfg: Arc<Config>, db: Db, sde: Arc<crate::sde::Sde>, chat: Arc<crate::chat::Hub>, world: Arc<crate::space::World>, login: Login) -> Self {
        let session = Session::new(login.session_id);
        let (conn, inbox) = conn.split();
        let (destiny_tx, destiny_rx) = mpsc::unbounded_channel();
        let (notify_tx, notify_rx) = mpsc::unbounded_channel();
        Client {
            conn,
            inbox,
            world,
            destiny_tx,
            destiny_rx,
            notify_tx,
            notify_rx,
            cfg,
            db,
            sde,
            chat,
            login,
            session,
            outbox: Vec::new(),
            bound: Default::default(),
            next_oid: 1,
        }
    }

    /// Register a bound object and return its wire form: `TY_CALLBACK(marshal((oid, refID)))`.
    pub fn bind(&mut self, service: &str, bind_params: Value) -> Value {
        let oid = format!("N={}:{}", self.cfg.server.node_id, self.next_oid);
        self.next_oid += 1;
        self.bound.insert(oid.clone(), (service.to_string(), bind_params));
        let reference = Value::tuple([Value::str(oid), Value::Int(filetime_now())]);
        Value::Callback(Box::new(Value::str(marshal::dumps(&reference))))
    }

    fn proxy_address(&self) -> Address {
        Address::Node { node_id: self.cfg.server.node_id, service: None, call_id: Value::None }
    }

    /// Change session attributes and queue a SessionChangeNotification for the client.
    pub fn change_session(&mut self, updates: Vec<(&str, Value)>) {
        let old_identity = Identity::of_client(self);
        let old = crate::session::ChatSession::from_session(&self.session);
        let change = self.session.apply(updates);
        if change.is_empty() {
            return;
        }
        let new = crate::session::ChatSession::from_session(&self.session);
        if old != new {
            let new_identity = Identity::of_client(self);
            if let Some(previous) = &old {
                if new.as_ref().map(|s| s.character_id) != Some(previous.character_id) {
                    presence::offline(&old_identity, &self.world, &self.chat);
                }
            }
            if let Some(current) = new {
                presence::online(&new_identity, &self.world, &self.chat, self.notify_tx.clone(), old.as_ref(), current);
            }
        }
        info!(user = self.login.user_id, "session change {}", short(&marshal::Value::Dict(change.clone())));
        let packet = Packet::new(
            cmd::SESSIONCHANGENOTIFICATION,
            self.proxy_address(),
            Address::Client { client_id: self.login.client_id, call_id: Value::None, service: None },
            Value::Int(self.login.user_id),
            vec![Value::Int(self.session.sid), Value::tuple([Value::Int(0), Value::Dict(change)])],
        );
        self.outbox.push(packet);
    }

    /// Queue a broadcast notification (`sm.ScatterEvent(event, *args)` on the client).
    pub fn notify(&mut self, event: &str, args: Vec<Value>) {
        let payload = Value::tuple([Value::Int(1), Value::Tuple(args)]);
        let packet = Packet::new(
            cmd::NOTIFICATION,
            self.proxy_address(),
            Address::Broadcast { broadcast_id: Value::str(event), narrowcast: Value::list([]), idtype: Value::str("clientID") },
            Value::Int(self.login.user_id),
            vec![Value::tuple([Value::Int(0), Value::str(marshal::dumps(&payload))])],
        );
        self.outbox.push(packet);
    }

    async fn send(&mut self, p: &Packet) -> Result<()> {
        self.conn.send(&p.to_value()).await
    }

    async fn flush(&mut self) -> Result<()> {
        for p in std::mem::take(&mut self.outbox) {
            self.send(&p).await?;
        }
        Ok(())
    }

    /// Wait on the system actor while forwarding destiny updates and queued packets.
    /// Returns `Ok(false)` if the actor stops and `Err` if a transport flush fails.
    pub async fn wait_pumping(&mut self, system: i64, delay_ms: u64) -> Result<bool> {
        self.flush().await?;
        let world = self.world.clone();
        let timer = world.after(system, delay_ms);
        tokio::pin!(timer);
        loop {
            tokio::select! {
                result = &mut timer => return Ok(result),
                Some(events) = self.destiny_rx.recv() => {
                    self.notify("DoDestinyUpdate", vec![Value::List(events), Value::Bool(false)]);
                    self.flush().await?;
                }
            }
        }
    }

    pub async fn run(mut self, registration: &mut crate::live_sessions::Registration) -> Result<()> {
        let _sender_guard = registration.attach_sender(self.notify_tx.clone());
        let result: Result<()> = async {
            if registration.is_cancelled() {
                return Ok(());
            }
            let l = &self.login;
            let (uid, role, lang, addr) = (l.user_id, l.role, l.language_id.clone(), self.conn.peer.ip().to_string());
            self.change_session(vec![
                ("userid", Value::Int(uid)),
                ("role", Value::Int(role)),
                ("languageID", Value::str(lang)),
                ("address", Value::str(addr)),
                ("userType", Value::Int(20)),
            ]);
            self.flush().await?;

            loop {
                tokio::select! {
                    biased;
                    _ = registration.cancelled() => break,
                    incoming = self.inbox.recv() => {
                        let Some(v) = incoming.transpose()?.flatten() else { break };
                        let packet = match Packet::from_value(&v) {
                            Ok(p) => p,
                            Err(e) => {
                                warn!("unparseable packet ({e}): {}", short(&v));
                                continue;
                            }
                        };
                        self.handle(packet).await?;
                    }
                    Some(events) = self.destiny_rx.recv() => {
                        self.notify("DoDestinyUpdate", vec![Value::List(events), Value::Bool(false)]);
                    }
                    Some((event, args)) = self.notify_rx.recv() => {
                        if event == "__ship_destroyed" {
                            services::ship_destroyed(&mut self, &args).await;
                        } else if event == BRAIN_REFRESH {
                            if let Err(error) = services::dogma::brain::update(&mut self).await {
                                match error {
                                    CallError::Internal(error) => warn!("skill brain refresh failed: {error:#}"),
                                    CallError::User { msg, .. } => warn!("skill brain refresh failed: {msg}"),
                                }
                            }
                        } else if event == services::chat::PERMISSIONS_REFRESH {
                            if let Some(room) = args.first().and_then(|value| value.as_text()) {
                                if let Err(error) = self.chat.refresh_permissions(&room).await {
                                    warn!("chat permission refresh failed: {error:?}");
                                }
                            }
                        } else if event == "__session" {
                            // Another task changed this pilot's session (corporation membership): [(key, value)].
                            let pairs: Vec<(String, Value)> = args
                                .iter()
                                .filter_map(|p| match p.items()? {
                                    [k, v] => Some((k.as_text()?.into_owned(), v.clone())),
                                    _ => None,
                                })
                                .collect();
                            self.change_session(pairs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect());
                        } else {
                            self.notify(&event, args);
                        }
                    }
                }
                if registration.is_cancelled() {
                    break;
                }
                self.flush().await?;
            }
            Ok(())
        }.await;
        if registration.is_cancelled() {
            if let Err(e) = self.conn.send(&crate::conn::usurped_close()).await {
                warn!(user = self.login.user_id, "usurp close failed: {e:#}");
            }
        }
        let char_id = self.session.int("charid");
        let cleanup = self.leave_world().await;
        if let Err(e) = &cleanup {
            warn!(user = self.login.user_id, "leave world failed: {e:#}");
        } else if let Some(char_id) = char_id {
            let mut outbox = Outbox::default();
            outbox.event(GameEvent::CharacterLoggedOff { user_id: self.login.user_id, char_id });
            if dispatch_client((), outbox, &mut self).is_err() {
                warn!(user = self.login.user_id, "logoff event dispatch failed");
            }
        }
        let close = self.conn.close().await;
        if let Err(e) = &close {
            warn!(user = self.login.user_id, "connection close failed: {e:#}");
        }
        info!(user = self.login.user_id, "client disconnected");
        result.and(cleanup).and(close)
    }

    /// The character leaves the game (disconnect, or logging off to character selection):
    /// out of its ballpark, and its ship out of space with its position remembered.
    pub async fn leave_world(&mut self) -> Result<()> {
        if let Some(char_id) = self.session.int("charid").filter(|id| *id != 0) {
            services::fleets::lifecycle::disconnect(&self.world, char_id, self.session.sid);
            self.change_session(vec![("fleetid", Value::None), ("wingid", Value::None), ("squadid", Value::None), ("fleetrole", Value::None)]);
        }
        let identity = Identity::of_client(self);
        let system = self.session.int("solarsystemid");
        let world = &self.world;
        let client_id = self.login.client_id;
        presence::leave(&identity, &self.db, world, &self.chat, &self.sde, system,
            move || world.unobserve(client_id)).await
    }

    async fn handle(&mut self, p: Packet) -> Result<()> {
        match p.command {
            cmd::CALL_REQ => self.handle_call(p).await,
            cmd::PING_REQ => {
                let mut times = p.body.first().and_then(|t| t.items()).map(|t| t.to_vec()).unwrap_or_default();
                // The client's ping stats expect every hop of a proxy -> sol -> proxy round trip.
                let now = filetime_now();
                for hop in [
                    "proxy::handle_message",
                    "proxy::writing",
                    "server::handle_message",
                    "server::turnaround",
                    "server::writing",
                    "proxy::handle_message",
                    "proxy::writing",
                ] {
                    times.push(Value::tuple([Value::Int(now), Value::Int(now), Value::str(hop)]));
                }
                let rsp = p.response(cmd::PING_RSP, vec![Value::List(times)]);
                self.send(&rsp).await
            }
            cmd::NOTIFICATION => {
                let payload = p.body.first().map(unwrap_pickle).transpose()?;
                info!(user = self.login.user_id, "client notification to {:?}: {}", p.destination.service(), payload.as_ref().map(short).unwrap_or_default());
                Ok(())
            }
            other => {
                warn!("unhandled packet command {other}: {:?}", p);
                Ok(())
            }
        }
    }

    async fn handle_call(&mut self, p: Packet) -> Result<()> {
        let outer = p.body.first().ok_or_else(|| anyhow!("CallReq without payload"))?;
        let parts = outer.items().ok_or_else(|| anyhow!("CallReq payload is not a tuple"))?;
        let is_object_call = parts.first().and_then(|v| v.as_int()) == Some(1);
        let inner = unwrap_pickle(outer)?;
        let tup = inner.items().ok_or_else(|| anyhow!("call tuple expected, got {inner:?}"))?;
        // Service call: (1, method, args, kwargs); object call: (oid, method, args, kwargs).
        let (target, method) = if is_object_call {
            (tup.first().and_then(|v| v.as_text()).map(|s| s.into_owned()).unwrap_or_default(), tup.get(1))
        } else {
            (p.destination.service().unwrap_or("").to_string(), tup.get(1))
        };
        let method = method.and_then(|m| m.as_text()).map(|m| m.into_owned()).unwrap_or_default();
        let args = tup.get(2).and_then(|a| a.items()).map(|a| a.to_vec()).unwrap_or_default();
        let kwargs = tup.get(3).cloned().unwrap_or(Value::Dict(vec![]));

        // Bound-object calls are addressed by OID; map back to (service, bindParams).
        let (service, bind_params) = if is_object_call {
            match self.bound.get(&target) {
                Some((svc, params)) => (svc.clone(), Some(params.clone())),
                None => (target.clone(), None),
            }
        } else {
            (target.clone(), None)
        };
        let call = Call {
            service: &service,
            method: &method,
            args: &args,
            kwargs: &kwargs,
            bound: is_object_call,
            bind_params: bind_params.as_ref(),
        };
        // The client asks these on every inventory listing (several a second in space).
        const UNLOGGED: [&str; 2] = ["ListSoulBound", "ListServiceLifeDamage"];
        if !UNLOGGED.contains(&method.as_str()) {
            debug!(
                user = self.login.user_id,
                "call {}{service}::{method}{}",
                if is_object_call { "bound " } else { "" },
                short(&marshal::Value::Tuple(args.clone()))
            );
        }
        let result = services::dispatch(self, &call).await;
        // Session changes caused by the call reach the client before its response.
        self.flush().await?;
        let rsp = match result {
            Ok(ret) => p.response(cmd::CALL_RSP, vec![Value::str(marshal::dumps(&ret))]),
            Err(CallError::User { msg, dict }) => {
                info!(user = self.login.user_id, "{target}::{method} -> UserError {msg}");
                let exc = user_error(&msg, dict);
                p.error_response(ERR_WRAPPEDEXCEPTION, Value::tuple([Value::str(marshal::dumps(&exc))]))
            }
            Err(CallError::Internal(e)) => {
                warn!(user = self.login.user_id, "{target}::{method} failed: {e:#}");
                let exc = Value::Reduce {
                    data: Box::new(Value::tuple([Value::global("exceptions.RuntimeError"), Value::tuple([Value::str(format!("{e:#}"))])])),
                    list: vec![],
                    dict: vec![],
                };
                p.error_response(ERR_WRAPPEDEXCEPTION, Value::tuple([Value::str(marshal::dumps(&exc))]))
            }
        };
        self.send(&rsp).await
    }
}

/// `(flag, pickle)` -> the unmarshalled pickle.
fn unwrap_pickle(outer: &Value) -> Result<Value> {
    let pickle = outer.items().and_then(|t| t.get(1)).ok_or_else(|| anyhow!("expected (flag, pickle), got {outer:?}"))?;
    let bytes = pickle.as_bytes().ok_or_else(|| anyhow!("pickle is not a string: {pickle:?}"))?;
    Ok(marshal::loads(bytes)?)
}

/// `eveexceptions.UserError(msg, dict)` as BaseException.__reduce__ marshals it.
pub fn user_error(msg: &str, dict: Vec<(String, Value)>) -> Value {
    let d = Value::Dict(dict.into_iter().map(|(k, v)| (Value::str(k), v)).collect());
    Value::Reduce {
        data: Box::new(Value::tuple([
            Value::global("eveexceptions.UserError"),
            Value::tuple([Value::str(msg), d.clone()]),
            Value::dict([("msg", Value::str(msg)), ("dict", d)]),
        ])),
        list: vec![],
        dict: vec![],
    }
}

/// Convert a millisecond duration to FILETIME ticks.
pub fn filetime_ticks_from_ms(ms: f64) -> i64 {
    (ms * FILETIME_TICKS_PER_MS as f64) as i64
}

/// Convert a signed FILETIME duration to whole milliseconds, truncating toward zero.
pub fn ms_from_filetime_ticks(ticks: i64) -> i64 {
    ticks / FILETIME_TICKS_PER_MS
}
/// Convert a FILETIME timestamp to Unix seconds, truncating toward zero.
pub fn unix_seconds_from_filetime(ticks: i64) -> i64 {
    ((i128::from(ticks) - i128::from(FILETIME_UNIX_EPOCH)) / i128::from(FILETIME_SECOND)) as i64
}

/// Convert a FILETIME timestamp to checked Unix nanoseconds without losing ticks.
pub(crate) fn unix_ns_from_filetime(ticks: i64) -> Option<i64> {
    i64::try_from((i128::from(ticks) - i128::from(FILETIME_UNIX_EPOCH)) * i128::from(FILETIME_TICK_NS)).ok()
}

/// Convert a signed whole-second duration to FILETIME ticks.
pub fn filetime_ticks_from_seconds(seconds: i64) -> i64 {
    seconds * FILETIME_SECOND
}
/// Convert the SDE datetime unit (days since Unix epoch) as
/// `dogma/attributes/datetime.py::float_as_time`, rounded to whole seconds.
pub fn filetime_from_dogma_days(days: f64) -> Option<i64> {
    let seconds = ((days * FILETIME_DAY as f64 + FILETIME_UNIX_EPOCH as f64) / FILETIME_SECOND as f64).round();
    if !seconds.is_finite() || seconds < i64::MIN as f64 || seconds >= i64::MAX as f64 { return None; }
    i64::try_from((seconds as i128) * i128::from(FILETIME_SECOND)).ok()
}

/// Convert a finite hour duration to FILETIME ticks, rejecting overflow.
pub fn filetime_ticks_from_hours(hours: f64) -> Option<i64> {
    let ticks = hours * FILETIME_HOUR as f64;
    if !ticks.is_finite() || ticks < i64::MIN as f64 || ticks >= i64::MAX as f64 { return None; }
    Some(ticks as i64)
}

/// Convert a FILETIME timestamp to Unix milliseconds, rounding toward positive infinity.
pub fn unix_ms_from_filetime_ceil(ticks: i64) -> i64 {
    let delta = i128::from(ticks) - i128::from(FILETIME_UNIX_EPOCH);
    let divisor = i128::from(FILETIME_TICKS_PER_MS);
    (delta.div_euclid(divisor) + i128::from(delta.rem_euclid(divisor) != 0)) as i64
}


/// Convert Unix milliseconds to Windows FILETIME without losing integer precision.
pub fn filetime_from_unix_ms(ms: i64) -> i64 {
    FILETIME_UNIX_EPOCH + ms * FILETIME_TICKS_PER_MS
}

/// Windows FILETIME (100ns since 1601), the unit of `blue.os.GetWallclockTime()`.
pub fn filetime_now() -> i64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    FILETIME_UNIX_EPOCH + (unix.as_nanos() / u128::from(FILETIME_TICK_NS)) as i64
}

#[cfg(test)] mod tests;
