//! One logged-in client's machoNet packet loop and ordered cleanup.
//! `Client::run` hands the session to the owned `lifecycle` boundary: a request
//! child runs the packet loop and a supervisor performs ordinary cleanup
//! exactly once, whether the loop ends normally, the caller is dropped, or a
//! request panics. Only a replacement emits the usurp close defined by
//! `carbon/common/script/net/machoNet.py`; a dropped caller is an ordinary
//! disconnect whose in-flight request finishes first. Session state uses the
//! game database and in-memory world; account ownership is process-local and
//! does not coordinate separate server processes.
//! A successful disconnect cleanup commits the selected character's logoff event.
//! Actor-timed transitions flush queued notifications and forward destiny batches while waiting.
//! `Client::leave_space` runs once per committed transition out of space: it severs the old
//! system's destiny channel and drops its unsent scene-bound packets while keeping other
//! notifications. A committed ship loss instead waits for the old system's actor and writes
//! the pending destruction batches to the transport before the session switch
//! (`Client::flush_destiny_actions`). Each destiny notification contains one logical stamp,
//! retaining action order within that stamp.
//! Initial state and incremental updates share an ordered channel; rebinding replaces it.
//! Narrow skill-queue diagnostics observe selected RPCs and notification writes.
//! The internal pending-lock failure request keeps the failed attempt's
//! identity through dequeue; the `target` service wire stays unchanged.

use infinity_wire::conn::{Conn, Writer};
use infinity_core::consts::{FILETIME_DAY, FILETIME_HOUR, FILETIME_SECOND, FILETIME_UNIX_EPOCH};
use crate::db::Db;
use crate::handshake::Login;
use infinity_wire::macho::{cmd, Address, Packet, ERR_WRAPPEDEXCEPTION};
use crate::services::{self, identity::Identity, presence, Call, CallError};
use infinity_core::session::Session;
use crate::config::Config;
use anyhow::{anyhow, Result};
use marshal::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

use tracing::{debug, info, warn};

pub(crate) mod lifecycle;
mod skill_queue_diagnostics;

/// Windows FILETIME uses 100 ns ticks, hence 10,000 ticks per millisecond.
const FILETIME_TICKS_PER_MS: i64 = 10_000;
/// Windows FILETIME tick duration in nanoseconds.
const FILETIME_TICK_NS: u32 = 100;
/// Internal pilot-channel request to recompile a brain.
pub(crate) const BRAIN_REFRESH: &str = "__brain";
/// Internal pilot-channel request to answer a discarded pending lock with the
/// client's `target` service `FailLockTarget`; never a broadcast name.
pub(crate) const TARGET_LOCK_FAILED: &str = "__target_lock_failed";
/// Identity of one discarded pending lock attempt. The internal request
/// carries it so a queued failure stays bound to the attempt that actually
/// failed; the client wire itself only carries the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TargetLockFailure {
    pub(crate) character: i64,
    pub(crate) target: i64,
    pub(crate) attempt: u64,
}

impl TargetLockFailure {
    /// Decode the request's `[character, target, low, high]` payload; the two
    /// u32 words carry `attempt` losslessly, and any other shape is refused.
    fn from_args(args: &[Value]) -> Option<Self> {
        let [Value::Int(character), Value::Int(target), Value::Int(low), Value::Int(high)] = args else {
            return None;
        };
        let attempt = u64::from(u32::try_from(*low).ok()?) | (u64::from(u32::try_from(*high).ok()?) << u32::BITS);
        Some(Self { character: *character, target: *target, attempt })
    }
}

/// The internal pending-lock failure request the combat publisher sends to the
/// originating connection: `(channel name, [character, target, low, high])`.
pub(crate) fn target_lock_failure(character: i64, target: i64, attempt: u64) -> (String, Vec<Value>) {
    let [b0, b1, b2, b3, b4, b5, b6, b7] = attempt.to_le_bytes();
    (TARGET_LOCK_FAILED.into(), vec![
        Value::Int(character), Value::Int(target),
        Value::Int(i64::from(u32::from_le_bytes([b0, b1, b2, b3]))),
        Value::Int(i64::from(u32::from_le_bytes([b4, b5, b6, b7]))),
    ])
}

pub struct Client {
    pub conn: Writer,
    inbox: mpsc::UnboundedReceiver<Result<Option<Value>>>,
    pub world: Arc<crate::space::World>,
    /// Destiny events pushed by the solar system this client observes.
    pub destiny_tx: crate::space::DestinyTx,
    destiny_rx: mpsc::UnboundedReceiver<crate::space::SceneDelivery>,
    /// Notifications other tasks address to this client's character (crate::space::World::register_pilot).
    pub notify_tx: crate::space::NotifyTx,
    pub(crate) failure: Option<crate::live_sessions::FailureTarget>,
    pub(crate) binding: Option<std::sync::Arc<crate::space::containment::PilotBinding>>,
    notify_rx: mpsc::UnboundedReceiver<(String, Vec<Value>)>,
    pub cfg: Arc<Config>,
    pub db: Db,
    pub sde: Arc<crate::sde::Sde>,
    pub chat: std::sync::Arc<crate::chat::Hub>,
    pub login: Login,
    pub session: Session,
    /// Packets queued by handlers for delivery at existing flush points.
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
        self.leave_space();
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
            failure: None,
            binding: None,
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
        let old = infinity_core::session::OnlineSession::from_session(&self.session);
        let change = self.session.apply(updates);
        if change.is_empty() {
            return;
        }
        let new = infinity_core::session::OnlineSession::from_session(&self.session);
        if old != new {
            let new_identity = Identity::of_client(self);
            if let Some(previous) = &old {
                if new.as_ref().map(|s| s.character_id) != Some(previous.character_id) {
                    presence::offline_generation(&old_identity, &self.world, &self.chat, Some(previous.session_id));
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
        self.queue_notification(event, args);
    }

    /// The one receiver entry for the ballpark channel: a Destiny batch keeps
    /// the original stamp-grouped encoding, a scene-bound notification keeps
    /// its position in the channel's order. Only packets appended here are
    /// marked scene-bound so a ballpark reset drops exactly this stream.
    fn queue_scene_delivery(&mut self, delivery: crate::space::SceneDelivery, wait_for_bubble: bool) {
        let before = self.outbox.len();
        match delivery {
            crate::space::SceneDelivery::Destiny(events) => self.queue_destiny_updates(events, wait_for_bubble),
            crate::space::SceneDelivery::Notify { event, args } => self.queue_notification(event, args),
        }
        for packet in &mut self.outbox[before..] { packet.scene_bound = true; }
    }
    fn queue_destiny_updates(&mut self, mut events: Vec<Value>, wait_for_bubble: bool) {
        let stamp = |event: &Value| match event {
            Value::Tuple(fields) => match fields.as_slice() {
                [Value::Int(stamp), Value::Tuple(call)]
                    if matches!(call.as_slice(), [Value::Bytes(_) | Value::Unicode(_), Value::Tuple(_)]) => Some(*stamp),
                _ => None,
            },
            _ => None,
        };
        let mut previous = None;
        let mut mixed = false;
        let mut ordered = true;
        for event in &events {
            let Some(current) = stamp(event) else {
                self.queue_notification("DoDestinyUpdate", vec![Value::List(events), Value::Bool(wait_for_bubble)]);
                return;
            };
            if let Some(last) = previous {
                mixed |= current != last;
                ordered &= current >= last;
            }
            previous = Some(current);
        }
        if !mixed {
            self.queue_notification("DoDestinyUpdate", vec![Value::List(events), Value::Bool(wait_for_bubble)]);
            return;
        }
        if !ordered { events.sort_by_key(|event| stamp(event).unwrap()); }
        let mut events = events.into_iter().peekable();
        while let Some(first) = events.next() {
            let current = stamp(&first).unwrap();
            let mut group = vec![first];
            while events.peek().is_some_and(|event| stamp(event) == Some(current)) {
                group.push(events.next().unwrap());
            }
            self.queue_notification("DoDestinyUpdate", vec![Value::List(group), Value::Bool(wait_for_bubble)]);
        }
    }

    fn queue_notification(&mut self, event: &str, args: Vec<Value>) {
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

    /// `target.FailLockTarget` as a service-destination notification: the body
    /// wraps `(1, method, (targetID,))`, which machoNet routes to the `target`
    /// service's method instead of a broadcast listener. The packet is built
    /// before the State check so no marshal or allocation work happens under
    /// the combat lock.
    fn queue_target_lock_failure(&mut self, args: &[Value]) {
        let Some(failure) = TargetLockFailure::from_args(args) else { return };
        if self.session.int("charid") != Some(failure.character) { return; }
        let payload = Value::tuple([Value::Int(1), Value::str("FailLockTarget"), Value::tuple([Value::Int(failure.target)])]);
        let packet = Packet::new(
            cmd::NOTIFICATION,
            self.proxy_address(),
            Address::Any { service: Some("target".into()), call_id: Value::None },
            Value::Int(self.login.user_id),
            vec![Value::tuple([Value::Int(0), Value::str(marshal::dumps(&payload))])],
        );
        self.outbox.reserve(1);
        let outbox = &mut self.outbox;
        services::combat::targeting_charges::with_pending_failure_current(failure, move || outbox.push(packet));
    }

    async fn send(&mut self, p: &Packet) -> Result<()> {
        let result = self.conn.send(&p.to_value()).await;
        skill_queue_diagnostics::notification_write(self, p, result.is_ok());
        result
    }

    async fn flush(&mut self) -> Result<()> {
        for p in std::mem::take(&mut self.outbox) {
            self.send(&p).await?;
        }
        Ok(())
    }

    /// Leave the currently observed solar system after a committed transition:
    /// unobserve, replace the destiny channel so the old system's sender can no
    /// longer reach this client, and drop queued but unsent DoDestinyUpdate
    /// packets from the old ballpark. Other notifications stay queued. Callers
    /// that stay in space re-observe with the new `destiny_tx`.
    pub(crate) fn leave_space(&mut self) {
        self.world.unobserve(self.login.client_id);
        self.reset_destiny();
    }

    /// Replace the ballpark stream and discard its unsent scene-bound packets.
    pub(crate) fn reset_destiny(&mut self) {
        let (destiny_tx, destiny_rx) = mpsc::unbounded_channel();
        self.destiny_tx = destiny_tx;
        self.destiny_rx = destiny_rx;
        self.outbox.retain(|packet| !packet.scene_bound);
    }

    /// Consume only the batches present at entry, preserving channel order.
    pub(crate) fn drain_destiny(&mut self) {
        for _ in 0..self.destiny_rx.len() {
            let Ok(delivery) = self.destiny_rx.try_recv() else { break };
            self.queue_scene_delivery(delivery, false);
        }
    }

    /// Wait until `system`'s actor has pushed every destiny action queued so far
    /// to this client's destiny channel, forward the pending batches as
    /// DoDestinyUpdate notifications through the usual path, then flush the
    /// outbox. A committed ship loss calls this before its session switch so the
    /// old ball's destruction reaches the transport first.
    pub(crate) async fn flush_destiny_actions(&mut self, system: i64) -> Result<()> {
        let (ack, receipt) = tokio::sync::oneshot::channel();
        self.world.with(system, move |s| s.queue_destiny_flush(ack)).await
            .ok_or_else(|| anyhow!("system actor stopped"))?;
        receipt.await.map_err(|_| anyhow!("destiny flush acknowledgement dropped"))?;
        self.drain_destiny();
        self.flush().await
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
                    self.queue_scene_delivery(events, false);
                    self.flush().await?;
                }
            }
        }
    }

    /// Hand this connection's session to its owned lifecycle: construction is
    /// synchronous and the returned `SessionRun` is the supervisor's future.
    pub(crate) fn run(self, registration: crate::live_sessions::Registration) -> lifecycle::SessionRun {
        lifecycle::SessionRun::new(self, registration)
    }

    /// The request child's whole packet loop, run once under the session lock.
    async fn run_loop(&mut self, registration: &mut crate::live_sessions::Registration) -> Result<()> {
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
                    self.queue_scene_delivery(events, false);
                }
                Some((event, args)) = self.notify_rx.recv() => {
                    if event == "__ship_destroyed" {
                        services::ship_destroyed(self, &args).await?;
                    } else if event == BRAIN_REFRESH {
                        if let Err(error) = services::dogma::brain::update(self).await {
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
                        let pairs = infinity_core::session::decode_pairs(&args);
                        self.change_session(pairs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect());
                    } else if event == TARGET_LOCK_FAILED {
                        crate::runtime_watchdog::watch("notification", "target_lock_failure", async {
                            self.queue_target_lock_failure(&args);
                        }).await;
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
    }

    /// The supervisor's one ordinary cleanup, after the request loop ended or
    /// its child panicked: usurp close only when a replacement took over, then
    /// leave and close exactly once, keeping the result's error priority.
    async fn finish(&mut self, registration: &crate::live_sessions::Registration, result: Result<()>) -> Result<()> {
        if registration.is_replaced() {
            if let Err(e) = self.conn.send(&infinity_wire::conn::usurped_close()).await {
                warn!(user = self.login.user_id, "usurp close failed: {e:#}");
            }
        }
        let cleanup = self.leave_world().await;
        if let Err(e) = &cleanup {
            warn!(user = self.login.user_id, "leave world failed: {e:#}");
        }
        let close = self.conn.close().await;
        if let Err(e) = &close {
            warn!(user = self.login.user_id, "connection close failed: {e:#}");
        }
        info!(user = self.login.user_id, "client disconnected");
        registration.record_cleanup(cleanup.is_ok(), close.is_ok());
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
        let client_id = self.login.client_id;
        let world = std::sync::Arc::downgrade(&self.world);
        presence::leave(&identity, Some(self.login.user_id), &self.db, &self.world, &self.chat, &self.sde, system, Some(self.session.sid),
            move || { if let Some(world) = world.upgrade() { world.unobserve(client_id); } }).await
    }

    async fn handle(&mut self, p: Packet) -> Result<()> {
        #[cfg(all(test, feature = "tests-runtime"))]
        tests::panic_containment::inject(&p);
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
        let mut key = (String::new(), String::new());
        let (result, timing) = crate::runtime_watchdog::measure(self.handle_call_inner(p, &mut key)).await;
        crate::runtime_watchdog::rpc(key, timing);
        result
    }

    async fn handle_call_inner(&mut self, p: Packet, key: &mut (String, String)) -> Result<()> {
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
        let mut kwargs = tup.get(3).cloned().unwrap_or(Value::Dict(vec![]));
        strip_macho_version(&mut kwargs);

        // Bound-object calls are addressed by OID; map back to (service, bindParams).
        let (service, bind_params) = if is_object_call {
            match self.bound.get(&target) {
                Some((svc, params)) => (svc.clone(), Some(params.clone())),
                None => (target.clone(), None),
            }
        } else {
            (target.clone(), None)
        };
        *key = (service, method);
        let (service, method) = (&key.0, &key.1);
        let call = Call {
            service: &service,
            method: &method,
            args: &args,
            kwargs: &kwargs,
            bound: is_object_call,
            bind_params: bind_params.as_ref(),
        };
        let mut diagnostic = skill_queue_diagnostics::begin(self, &p, &call);
        // The client asks these on every inventory listing (several a second in space).
        const UNLOGGED: [&str; 2] = ["ListSoulBound", "ListServiceLifeDamage"];
        if diagnostic.is_none() && !UNLOGGED.contains(&method.as_str()) {
            debug!(
                user = self.login.user_id,
                "call {}{service}::{method}{}",
                if is_object_call { "bound " } else { "" },
                short(&marshal::Value::Tuple(args.clone()))
            );
        }
        let result = services::dispatch(self, &call).await;
        if let Some(diagnostic) = diagnostic.as_mut() {
            diagnostic.observe_result(&result);
        }
        // Session changes caused by the call reach the client before its response.
        // A retiring system is the one exception: the connection ends before the
        // queued messages of that call are flushed or answered.
        let rsp = match result {
            Ok(ret) => p.response(cmd::CALL_RSP, vec![Value::str(marshal::dumps(&ret))]),
            Err(CallError::User { msg, dict }) => {
                info!(user = self.login.user_id, "{target}::{method} -> UserError {msg}");
                let exc = user_error(&msg, dict);
                p.error_response(ERR_WRAPPEDEXCEPTION, Value::tuple([Value::str(marshal::dumps(&exc))]))
            }
            Err(CallError::Internal(e)) => {
                if e.downcast_ref::<crate::space::containment::ActorRetiring>().is_some() {
                    if let Some(diagnostic) = diagnostic {
                        diagnostic.finish(None, skill_queue_diagnostics::RpcWrite::NotAttempted);
                    }
                    return Err(e);
                }
                warn!(user = self.login.user_id, "{target}::{method} failed: {e:#}");
                let exc = Value::Reduce {
                    data: Box::new(Value::tuple([Value::global("exceptions.RuntimeError"), Value::tuple([Value::str(format!("{e:#}"))])])),
                    list: vec![],
                    dict: vec![],
                };
                p.error_response(ERR_WRAPPEDEXCEPTION, Value::tuple([Value::str(marshal::dumps(&exc))]))
            }
        };
        if let Err(error) = self.flush().await {
            if let Some(diagnostic) = diagnostic {
                diagnostic.finish(None, skill_queue_diagnostics::RpcWrite::NotAttempted);
            }
            return Err(error);
        }
        let result = self.send(&rsp).await;
        if let Some(diagnostic) = diagnostic {
            let write = if result.is_ok() {
                skill_queue_diagnostics::RpcWrite::Ok
            } else {
                skill_queue_diagnostics::RpcWrite::Failed
            };
            diagnostic.finish(Some(&rsp), write);
        }
        result
    }
}

/// Remove the transport-only protocol kwarg before service dispatch.
fn strip_macho_version(kwargs: &mut Value) {
    if let Value::Dict(entries) = kwargs {
        entries.retain(|(key, _)| key.as_text().as_deref() != Some("machoVersion"));
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

/// Convert FILETIME to Unix seconds and nanoseconds using standard protobuf Timestamp normalization.
pub(crate) fn unix_parts_from_filetime(ticks: i64) -> (i64, i32) {
    let delta = i128::from(ticks) - i128::from(FILETIME_UNIX_EPOCH);
    let second = i128::from(FILETIME_SECOND);
    // The full i64 FILETIME domain fits in i64 seconds and the subsecond remainder fits in i32 nanoseconds.
    (delta.div_euclid(second) as i64, (delta.rem_euclid(second) * i128::from(FILETIME_TICK_NS)) as i32)
}

/// Convert a FILETIME timestamp to checked Unix nanoseconds without losing ticks.
pub(crate) fn unix_ns_from_filetime(ticks: i64) -> Option<i64> {
    i64::try_from((i128::from(ticks) - i128::from(FILETIME_UNIX_EPOCH)) * i128::from(FILETIME_TICK_NS)).ok()
}

/// Convert a signed whole-second duration to FILETIME ticks.
pub fn filetime_ticks_from_seconds(seconds: i64) -> i64 {
    seconds * FILETIME_SECOND
}

/// Start of `at`'s client day in FILETIME ticks, truncating toward zero.
pub(crate) fn filetime_day_start(at: i64) -> i64 {
    at / FILETIME_DAY * FILETIME_DAY
}

/// Add whole seconds, saturating the tick duration before the deadline.
pub(crate) fn filetime_deadline_from_seconds_saturating(now: i64, seconds: i64) -> i64 {
    now.saturating_add(seconds.saturating_mul(FILETIME_SECOND))
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

#[cfg(all(test, feature = "tests-runtime"))] mod tests;
