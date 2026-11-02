//! Remote service routing and shared error builders, wire-field readers, randomness,
//! corporation queries and site placement. Individual modules document their client
//! contracts; scanning consumers include `eve/client/script/parklife/scanSvc.py:193-198`.
//! Placement is server design, not a client-provided position generator: it uses
//! celestials from the shared SDE, while each service retains its own fallback and
//! offset policy. Runtime corporation data comes from the game database. Unsupported
//! services fall through the dispatch chain; feature limitations are documented in
//! their modules.
//! Scheduler registration supplies the durable periodic maintenance scans persisted in
//! game.jobs; there are no detached maintenance sleeps. Successful periodic completion
//! atomically reschedules the job. Boot seeds only absent or dead jobs, preserving existing
//! deadlines and leases. Local active IDs are excluded from reclaim to prevent overlapping
//! scans in one process; leases are not renewed.

mod access_groups;
mod agency;
mod alert;
mod alliances;
mod bills;
mod bookmarks;
pub(crate) mod bounty;
mod calendar;
mod character;
pub(crate) mod chat;
mod clones;
pub(crate) mod combat;
mod charsel;
mod config;
mod contracts;
pub(crate) mod corps;
mod corpwallet;
pub(crate) mod crimewatch;
pub(crate) mod dogma;
pub(crate) mod drones;
mod dynamic_items;
pub(crate) mod effects;
mod exploration;
mod event_log;
pub(crate) mod fit;
mod fixed;
pub(crate) mod fleets;
mod fitting;
mod hacking;
pub(crate) mod identity;
pub(crate) mod industry;
mod insurance;
pub(crate) mod inventory;
mod item_volume;
pub(crate) mod items;
pub(crate) mod mail;
pub(crate) mod market;
pub(crate) mod map;
mod npe;
mod offices;
pub(crate) mod online_status;
mod orgs;
pub(crate) mod mining;
mod missions;
pub(crate) mod presence;
pub(crate) mod ship;
mod settings_sharing;
pub(crate) mod skills;
pub(crate) mod standings;
mod slash;
mod sql_pattern;
mod station;
mod store;
pub(crate) mod trade;
mod universe;
pub(crate) mod wallets;
pub(crate) mod wars;

pub(crate) use combat::recover_pending_reloads;
pub use exploration::unbury_sites;

pub(crate) fn scheduler_registrations() -> Vec<crate::scheduler::Registration> {
    let game = crate::config::game();
    use crate::scheduler::{periodic, MARKET, CONTRACTS, BILLS, MINING_LEDGER, MAP_HISTORY};
    vec![
        periodic(MARKET, market::expire_orders, game.market.maintenance_secs as u64),
        periodic(CONTRACTS, contracts::settle_auctions, game.contracts.maintenance_secs as u64),
        periodic(BILLS, bills::run_billing, game.corporations.bills_maintenance_secs as u64),
        periodic(MINING_LEDGER, mining::retention::handle, game.mining.ledger_maintenance_secs as u64),
        periodic(MAP_HISTORY, map::retention::handle, game.map.maintenance_secs as u64),
        trade::timeout_registration(),
        crate::scheduler::register(bounty::kill_rights::EXPIRY_KIND, bounty::kill_rights::expire),
        crate::scheduler::register(bounty::payout::KIND, bounty::payout::handle),
        crate::scheduler::register(skills::completion::KIND, skills::completion::handle),
        crate::scheduler::register(industry::completion::KIND, industry::completion::handle),
        crate::scheduler::register(chat::expiry::KIND, chat::expiry::handle),
        crate::scheduler::register(wars::deadlines::KIND, wars::deadlines::handle),
        crate::scheduler::register(corps::vote_close::KIND, corps::vote_close::handle),
    ]
}

use crate::client::{short, Client};
use crate::consts::GROUP_PLANET;
use marshal::{DbRowDescriptor, DbType, Value};
use crate::db::executor::Executor;
use crate::messages;
use crate::sde::Sde;
use crate::space::V3;
use tracing::warn;

/// IEEE 754 binary64 has 53 significant bits; use the high bits of the random word.
const RANDOM_SIGNIFICANT_BITS: u32 = f64::MANTISSA_DIGITS;
/// Diameter of the unit sphere, mapping a unit random value to [-1, 1).
const UNIT_SPHERE_DIAMETER: f64 = 2.0;
/// Design policy: flatten random site directions toward the system plane.
const SITE_DIRECTION_FLATTENING: f64 = 0.2;
/// Design policy: guard normalization of a near-zero site direction.
const SITE_DIRECTION_EPSILON: f64 = 1e-9;

/// The pilot's ship was destroyed: eject (raised by combat through the pilot's notify channel).
pub async fn ship_destroyed(c: &mut Client, args: &[Value]) {
    let n = |i: usize| args.get(i).and_then(|v| v.as_float()).unwrap_or(0.0);
    let ship = args.first().and_then(|v| v.as_int()).unwrap_or(0);
    if let Err(CallError::Internal(e)) = ship::lose_ship(c, ship, [n(1), n(2), n(3)]).await {
        warn!(user = c.login.user_id, "ejecting from {ship} failed: {e:#}");
    }
}

pub struct Call<'a> {
    pub service: &'a str,
    pub method: &'a str,
    pub args: &'a [Value],
    pub kwargs: &'a Value,
    /// Call on a bound object (moniker) rather than a service.
    pub bound: bool,
    /// The moniker's bindParams for bound calls.
    pub bind_params: Option<&'a Value>,
}

#[derive(Debug)]
pub enum CallError {
    /// Shown to the player via `eveexceptions.UserError(msg, dict)`.
    #[allow(dead_code)]
    User { msg: String, dict: Vec<(String, Value)> },
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for CallError {
    fn from(e: anyhow::Error) -> Self {
        CallError::Internal(e)
    }
}

impl From<crate::db::Error> for CallError {
    fn from(error: crate::db::Error) -> Self {
        Self::Internal(error.into())
    }
}

impl crate::db::transaction::TransactionError for CallError {
    fn is_retryable_transaction(&self) -> bool {
        use crate::db::transaction::{retryable_postgres, TransactionError};
        match self {
            Self::User { .. } => false,
            Self::Internal(error) => error.chain().any(|cause| {
                cause.downcast_ref::<crate::db::Error>().is_some_and(TransactionError::is_retryable_transaction)
                    || cause.downcast_ref::<tokio_postgres::Error>().is_some_and(retryable_postgres)
            }),
        }
    }
}

pub type CallResult = Result<Value, CallError>;

pub fn db_err(e: impl Into<anyhow::Error>) -> CallError {
    CallError::Internal(e.into())
}

/// Raise the client's named error with its substitution dictionary.
pub fn user_error(msg: &str, dict: Vec<(String, Value)>) -> CallError {
    CallError::User { msg: msg.into(), dict }
}

pub fn notify_error(text: impl Into<String>) -> CallError {
    user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(text.into()))])
}

/// Read a call argument as text, trimming whitespace; missing or non-text arguments become empty.
pub(super) fn trimmed_arg_text(call: &Call<'_>, index: usize) -> String {
    call.args.get(index).and_then(|v| v.as_text()).map(|t| t.trim().to_string()).unwrap_or_default()
}

/// Read client text, limiting it to `max` Unicode characters.
pub(super) fn value_text(v: Option<&Value>, max: usize) -> String {
    v.and_then(|v| v.as_text()).map(|t| t.chars().take(max).collect()).unwrap_or_default()
}

/// Read integer IDs from a single integer or an iterable client value.
pub(super) fn value_ids(v: Option<&Value>) -> Vec<i64> {
    match v {
        Some(Value::Int(i)) => vec![*i],
        Some(v) => v.items().map(|it| it.iter().filter_map(|x| x.as_int()).collect()).unwrap_or_default(),
        None => vec![],
    }
}

/// Read integer IDs from an iterable client value.
pub(super) fn value_id_list(v: Option<&Value>) -> Vec<i64> {
    v.and_then(|v| v.items()).unwrap_or(&[]).iter().filter_map(|v| v.as_int()).collect()
}

fn unit_from_bits(bits: u64) -> f64 {
    (bits >> (u64::BITS - RANDOM_SIGNIFICANT_BITS)) as f64 / (1u64 << RANDOM_SIGNIFICANT_BITS) as f64
}

/// A uniformly distributed binary64 value in [0, 1).
pub fn rand01() -> f64 {
    unit_from_bits(crate::handshake::random_u64())
}

/// A random unit vector, uniform on the sphere.
pub fn rand_dir() -> V3 {
    let (z, a) = (rand01() * UNIT_SPHERE_DIAMETER - 1.0, rand01() * std::f64::consts::TAU);
    let r = (1.0 - z * z).sqrt();
    [r * a.cos(), z, r * a.sin()]
}

/// Read a field from a client `utillib.KeyVal` (or dict).
pub fn value_field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    match v {
        Value::Instance(_, state) => state.get(name),
        _ => v.get(name),
    }
}

/// Accept a member of `corp_id` with any role in `role_mask` or who is its CEO.
/// Membership and CEO lookup errors propagate; the CEO is queried only if needed.
pub(super) async fn has_role_or_is_ceo(db: &(impl Executor + ?Sized), char_id: i64, corp_id: i64, role_mask: i64) -> Result<bool, CallError> {
    let (my_corp, roles) = corps::membership(db, char_id).await?;
    Ok(my_corp == corp_id && (roles & role_mask != 0 || corps::ceo_of(db, corp_id).await? == Some(char_id)))
}


/// Choose a planet and its radius; callers retain their fallback and offset policy.
pub fn site_position(sde: &Sde, system: i64) -> Option<(V3, f64)> {
    let planets = || sde.map.celestials(system).filter(|p| p.group_id == Some(i64::from(GROUP_PLANET)));
    let count = planets().count();
    if count == 0 { return None; }
    let p = planets().nth((rand01() * count as f64) as usize)?;
    Some(([p.x.expect("planet x"), p.y.expect("planet y"), p.z.expect("planet z")], p.radius.unwrap_or(0.0)))
}

/// A point `distance` above a body's surface, in a random direction near the system plane.
pub(super) fn off_surface(center: V3, radius: f64, distance: f64) -> V3 {
    let v = rand_dir();
    let flat = [v[0], v[1] * SITE_DIRECTION_FLATTENING, v[2]];
    let len = crate::space::dist(flat, [0.0; 3]).max(SITE_DIRECTION_EPSILON);
    let d = radius + distance;
    [center[0] + flat[0] / len * d, center[1] + flat[1] / len * d, center[2] + flat[2] / len * d]
}

/// `utillib.KeyVal(**kw)`: attribute-access record.
pub fn keyval<K: AsRef<[u8]>>(fields: impl IntoIterator<Item = (K, Value)>) -> Value {
    Value::instance("utillib.KeyVal", fields)
}

/// `carbon.common.script.sys.row.Row` with the supplied header and list-valued line.
pub(super) fn row(header: Value, line: Vec<Value>) -> Value {
    Value::instance("carbon.common.script.sys.row.Row", [("header", header), ("line", Value::List(line))])
}

/// `eve.common.script.sys.rowset.Rowset(header, lines)` (old-style instance, plain __dict__).
pub fn rowset(header: &[&str], lines: Vec<Vec<Value>>) -> Value {
    Value::instance(
        "eve.common.script.sys.rowset.Rowset",
        [
            ("header", Value::list(header.iter().map(|h| Value::str(*h)))),
            ("lines", Value::List(lines.into_iter().map(Value::List).collect())),
            ("RowClass", Value::global("carbon.common.script.sys.row.Row")),
        ],
    )
}

/// `dbutil.CRowset(header, rows)`: a list of `blue.DBRow`s sharing `header`.
pub fn crowset(header: &DbRowDescriptor, rows: Vec<Value>) -> Value {
    Value::reduce("carbon.common.script.sys.crowset.CRowset", vec![header.to_value(), Value::List(rows)], None)
}

/// Columns of an inventory row (invbroker.GetItemDescriptor).
pub fn item_descriptor() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("itemID", DbType::I8),
        ("typeID", DbType::I4),
        ("ownerID", DbType::I4),
        ("locationID", DbType::I8),
        ("flagID", DbType::I2),
        ("quantity", DbType::I4),
        ("groupID", DbType::I2),
        ("categoryID", DbType::I1),
        ("customInfo", DbType::Str),
    ])
    .with_virtual([("stacksize", "eve.common.script.sys.eveCfg.StackSize"), ("singleton", "eve.common.script.sys.eveCfg.Singleton")])
}

/// `MachoBindObject(bindParams, call)` -> `(boundObject, result of call or None)`.
async fn bind_object(c: &mut Client, call: &Call<'_>) -> CallResult {
    let params = call.args.first().cloned().unwrap_or(Value::None);
    let object = c.bind(call.service, params.clone());
    let mut ret = Value::None;
    if let Some([method, args, kwargs]) = call.args.get(1).and_then(|v| v.items()) {
        let method = method.as_text().map(|m| m.into_owned()).unwrap_or_default();
        let args = args.items().map(|a| a.to_vec()).unwrap_or_default();
        let inner = Call { service: call.service, method: &method, args: &args, kwargs, bound: true, bind_params: Some(&params) };
        ret = Box::pin(dispatch(c, &inner)).await?;
    }
    // Binding the remote ballpark is the client's signal that it is ready for destiny state.
    if call.service == "beyonce" {
        ship::send_state(c).await?;
        if let (Some(char_id), Some(system_id)) = (c.session.int("charid"), c.session.int("solarsystemid")) {
            let mut outbox = crate::db::outbox::Outbox::default();
            outbox.event(crate::lua::events::GameEvent::BallparkBound { char_id, system_id });
            crate::db::outbox::dispatch_client((), outbox, c)?;
        }
    }
    Ok(Value::tuple([object, ret]))
}

pub(crate) fn lua_fixed_claimed(call: &Call<'_>) -> bool { fixed::root(call).is_some() }

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    // What the retail server answered for this build, where the client cached it.
    if let Some(v) = crate::retail::cached_call(call.service, call.method, call.args) {
        return Ok(v);
    }
    if call.method != "MachoResolveObject" && call.method != "MachoBindObject" {
        if crate::lua::enabled() {
            if let Some(reply) = crate::lua::dispatch(c, call).await { return reply; }
        }
        if let Some(reply) = fixed::root(call) { return reply; }
        if let Some(reply) = chat::root(c, call).await { return reply; }
        if let Some(reply) = station::root(c, call).await { return reply; }
    }
    match (call.service, call.method) {
        // Monikers: every bindable service lives on this node.
        (_, "MachoResolveObject") => Ok(Value::Int(c.cfg.server.node_id)),
        (_, "MachoBindObject") => bind_object(c, call).await,


        ("trademgr", _) => trade::route(c, call).await,
        (inventory::INVENTORY, _) => inventory::inventory(c, call).await,

        ("config", _) => config::dispatch(c, call).await,


        ("charUnboundMgr", _) => charsel::dispatch(c, call).await,
        ("userSvc", "UserLogOffCharacter") => charsel::log_off(c).await,
        ("dogmaIM", _) => dogma::dispatch(c, call).await,

        _ => {
            if let Some(r) = alert::dispatch(c, call).await { return r; }
            if let Some(r) = event_log::dispatch(c, call).await { return r; }
            if let Some(r) = settings_sharing::dispatch(c, call).await { return r; }
            if call.service == "invbroker" {
                if let Some(r) = inventory::broker(c, call).await {
                    return r;
                }
            }
            if let Some(r) = dynamic_items::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = crimewatch::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = bounty::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = calendar::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = bookmarks::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = access_groups::dispatch(c, call).await {
                return r;
            }
            if matches!(call.service, "calendarMgr" | "accessGroupBookmarkMgr") {
                if let Some(r) = fixed::social_early(call) { return r; }
            }
            if let Some(r) = clones::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = insurance::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = fleets::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = orgs::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = corpwallet::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = offices::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = bills::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = alliances::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = corps::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = standings::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = ship::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = drones::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = fitting::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = skills::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = wars::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = fixed::social(call) {
                return r;
            }
            if let Some(r) = online_status::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = character::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = store::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = market::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = contracts::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = industry::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = missions::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = slash::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = hacking::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = mining::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = exploration::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = agency::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = npe::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = mail::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = fixed::social_early(call) { return r; }
            if let Some(r) = map::dispatch(c, call).await { return r; }
            if let Some(r) = universe::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = station::dispatch(c, call).await {
                return r;
            }
            warn!(
                user = c.login.user_id,
                "UNHANDLED {}{}::{} args={} kwargs={}",
                if call.bound { "bound " } else { "" },
                call.service,
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}

#[cfg(test)] mod tests;
