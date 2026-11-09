//! Fleets (fleetObjectHandler, bound per fleet; eve/client/script/parklife/fleetSvc.py).
//!
//! Fleets, reservations, preferences, advertisements and target tags live only in
//! process memory and do not survive restart. Disconnected members retain their seats for the configured
//! reconnect window; explicit departure and expiry remove them. Creating one (`CreateFleet`
//! then `Init`) makes the creator its leader. Boosts and foreign hangars are unsupported.

use super::{keyval, notify_error, value_text, Call, CallError, CallResult};
use crate::client::Client;
use infinity_core::messages;
use crate::space::World;
use marshal::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;
use tracing::info;

/// Client evefleet.const fleet leader role.
const ROLE_LEADER: i64 = 1;
const ROLE_WING_COMMANDER: i64 = 2;
const ROLE_SQUAD_COMMANDER: i64 = 3;
/// Client evefleet.const fleet member role.
const ROLE_MEMBER: i64 = 4;
/// Client evefleet.const no fleet job.
const JOB_NONE: i64 = 0;
/// Client evefleet.const fleet creator job.
const JOB_CREATOR: i64 = 2;
/// Client evefleet.const absent wing or squad ID.
const NONE_ID: i64 = -1;
/// Server fleet policy: maximum fleet membership.
pub(super) const MAX_MEMBERS: usize = 256;
const MAX_DAMAGE_SENDERS: usize = 100;
const MAX_WINGS: usize = 25;
const MAX_SQUADS: usize = 25;
const MAX_SQUAD_MEMBERS: usize = 256;
/// Server fleet policy: maximum wing and squad name length.
const MAX_NAME_LENGTH: usize = 20;
/// Server allocation policy: first process-local fleet, wing or squad ID.
const FIRST_FLEET_ID: i64 = 1;

static NEXT_ID: AtomicI64 = AtomicI64::new(FIRST_FLEET_ID);

#[derive(Clone)]
struct ReconnectState {
    generation: u64,
    deadline: Instant,
    offered_session: Option<i64>,
}

#[derive(Clone)]
struct Member {
    wing: i64,
    squad: i64,
    role: i64,
    job: i64,
    ship_type: i64,
    opt_outs: MemberOptOuts,
    watchlist: Vec<i64>,
    damage_updates: bool,
    reconnect: Option<ReconnectState>,
    active_session: Option<i64>,
    recovered_generation: Option<u64>,
}

#[derive(Clone)]
struct MemberOptOuts {
    accepts_fleet_warp: bool,
    accepts_conduit_jumps: bool,
    accepts_fleet_regroups: bool,
}

impl Default for MemberOptOuts {
    fn default() -> Self {
        Self { accepts_fleet_warp: true, accepts_conduit_jumps: true, accepts_fleet_regroups: true }
    }
}

impl Member {
    fn new(wing: i64, squad: i64, role: i64, job: i64, ship_type: i64) -> Self {
        Self { wing, squad, role, job, ship_type, opt_outs: MemberOptOuts::default(), watchlist: Vec::new(), damage_updates: false, reconnect: None, active_session: None, recovered_generation: None }
    }
}

#[derive(Default)]
struct FleetOptions {
    free_move: bool,
    registered: bool,
    auto_join_squad: Option<i64>,
}

fn opt_outs_value(options: &MemberOptOuts) -> Value {
    keyval([
        ("acceptsFleetWarp", Value::Bool(options.accepts_fleet_warp)),
        ("acceptsConduitJumps", Value::Bool(options.accepts_conduit_jumps)),
        ("acceptsFleetRegroups", Value::Bool(options.accepts_fleet_regroups)),
    ])
}

fn options_value(options: &FleetOptions) -> Value {
    keyval([
        ("isFreeMove", Value::Bool(options.free_move)),
        ("isRegistered", Value::Bool(options.registered)),
        ("isVoiceEnabled", Value::Bool(false)),
        ("autoJoinSquadID", options.auto_join_squad.map(Value::Int).unwrap_or(Value::None)),
    ])
}
 
struct Squad {
    id: i64,
    name: String,
}

struct Wing {
    id: i64,
    name: String,
    squads: Vec<Squad>,
}

struct Fleet {
    creator: i64,
    creator_session: Option<i64>,
    initialized: bool,
    options: FleetOptions,
    members: HashMap<i64, Member>,
    wings: Vec<Wing>,
    motd: String,
    /// Invited pilot -> (wing, squad, role) they'll join as.
    invites: HashMap<i64, (i64, i64, i64)>,
    max_members: usize,
    join_requests: HashMap<i64, Value>,
    advert: Option<Value>,
    target_tags: HashMap<(i64, i64), String>,
}

impl Fleet {
    fn new(creator: i64, wings: Vec<Wing>) -> Self {
        Self { creator, creator_session: None, initialized: false, options: FleetOptions::default(), members: HashMap::new(), wings, motd: String::new(), invites: HashMap::new(), max_members: MAX_MEMBERS, join_requests: HashMap::new(), advert: None, target_tags: HashMap::new() }
    }

    fn rename_structure(&mut self, id: i64, name: &str, wing: bool) -> bool {
        let slot = if wing {
            self.wings.iter_mut().find(|w| w.id == id).map(|w| &mut w.name)
        } else {
            self.wings.iter_mut().flat_map(|w| w.squads.iter_mut()).find(|s| s.id == id).map(|s| &mut s.name)
        };
        if let Some(slot) = slot { *slot = name.to_owned(); true } else { false }
    }
}


#[derive(Default)]
struct State {
    fleets: HashMap<i64, Fleet>,
    /// Pilot -> fleet.
    of: HashMap<i64, i64>,
    generation: u64,
}

impl State {
    fn can_create(&self, char_id: i64) -> bool {
        self.of.get(&char_id).is_none_or(|&id| self.has_member(id, char_id) && !self.is_member(id, char_id)) && !self.fleets.values().any(|f| f.creator == char_id && !f.initialized)
    }

    fn clear_pending(&mut self, char_id: i64) {
        self.fleets.retain(|_, f| f.initialized || f.creator != char_id);
        for fleet in self.fleets.values_mut() {
            fleet.invites.remove(&char_id);
            fleet.join_requests.remove(&char_id);
        }
    }

    fn clear_disconnected_pending(&mut self, char_id: i64, sid: i64) {
        // A replacement connection owns its shell; an older cleanup cannot remove it.
        self.fleets.retain(|_, f| f.initialized || f.creator != char_id || f.creator_session.is_some_and(|owner| owner != sid));
        for fleet in self.fleets.values_mut() {
            fleet.invites.remove(&char_id);
            fleet.join_requests.remove(&char_id);
        }
    }

    fn clear_member_references(&mut self, char_id: i64) {
        for fleet in self.fleets.values_mut() {
            for member in fleet.members.values_mut() {
                member.watchlist.retain(|&id| id != char_id);
            }
        }
    }

    fn has_member(&self, fleet_id: i64, char_id: i64) -> bool {
        self.of.get(&char_id) == Some(&fleet_id)
            && self.fleets.get(&fleet_id).is_some_and(|fleet| fleet.members.contains_key(&char_id))
    }

    fn is_member(&self, fleet_id: i64, char_id: i64) -> bool {
        self.has_member(fleet_id, char_id)
            && self.fleets[&fleet_id].members[&char_id].reconnect.is_none()
    }

    fn initialize(&mut self, fleet_id: i64, char_id: i64, ship_type: i64) -> Option<Member> {
        if self.of.contains_key(&char_id) { return None; }
        let fleet = self.fleets.get_mut(&fleet_id)?;
        if fleet.creator != char_id || fleet.initialized { return None; }
        let member = Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_CREATOR, ship_type);
        fleet.initialized = true;
        fleet.creator_session = None;
        fleet.members.insert(char_id, member.clone());
        self.of.insert(char_id, fleet_id);
        Some(member)
    }
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

pub(crate) fn same_fleet(a: i64, b: i64) -> bool {
    let state = state();
    state.of.get(&a).is_some_and(|&fleet| state.is_member(fleet, a) && state.is_member(fleet, b))
}

fn next_id() -> i64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn member_value(char_id: i64, m: &Member) -> Value {
    keyval([
        ("charID", Value::Int(char_id)),
        ("wingID", Value::Int(m.wing)),
        ("squadID", Value::Int(m.squad)),
        ("role", Value::Int(m.role)),
        ("job", Value::Int(m.job)),
        ("shipTypeID", Value::Int(m.ship_type)),
        ("memberOptOuts", opt_outs_value(&m.opt_outs)),
    ])
}

fn wings_value(f: &Fleet) -> Value {
    Value::Dict(
        f.wings
            .iter()
            .map(|w| {
                let squads = Value::Dict(w.squads.iter().map(|s| (Value::Int(s.id), keyval([("squadID", Value::Int(s.id)), ("name", Value::unicode(s.name.as_str()))]))).collect());
                (Value::Int(w.id), keyval([("wingID", Value::Int(w.id)), ("name", Value::unicode(w.name.as_str())), ("squads", squads)]))
            })
            .collect(),
    )
}

/// Tell everyone in the fleet (but `except`).
fn broadcast(world: &World, f: &Fleet, except: Option<i64>, event: &str, args: Vec<Value>) {
    for (ch, member) in &f.members {
        if Some(*ch) != except && member.reconnect.is_none() {
            world.notify_pilot(*ch, event, args.clone());
        }
    }
}

fn session_pairs(fleet: Option<i64>, m: Option<&Member>) -> Vec<Value> {
    let pair = |k: &str, v: Value| Value::tuple([Value::str(k), v]);
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    vec![
        pair("fleetid", opt(fleet)),
        pair("wingid", opt(m.map(|m| m.wing))),
        pair("squadid", opt(m.map(|m| m.squad))),
        pair("fleetrole", opt(m.map(|m| m.role))),
    ]
}

/// The fleet a bound call is for.
fn bound_fleet(call: &Call<'_>) -> i64 {
    call.bind_params.and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int()))).unwrap_or(0)
}

fn changed(char_id: i64, fleet_id: i64, before: &Member, after: &Member, only: bool) -> Vec<Value> {
    vec![
        Value::Int(char_id),
        Value::Int(fleet_id),
        Value::Int(before.wing),
        Value::Int(before.squad),
        Value::Int(before.role),
        Value::Int(before.job),
        opt_outs_value(&before.opt_outs),
        Value::Int(after.wing),
        Value::Int(after.squad),
        Value::Int(after.role),
        Value::Int(after.job),
        opt_outs_value(&after.opt_outs),
        Value::Bool(only),
    ]
}

fn is_leader(f: &Fleet, char_id: i64) -> bool {
    f.members.get(&char_id).is_some_and(|m| m.role == ROLE_LEADER)
}

mod arrangement;
mod broadcasts;
pub(crate) mod damage;
mod fleet_mgr;
mod fleet_object_handler;
mod fleet_proxy;
pub(crate) mod lifecycle;
pub(crate) mod navigation;
pub(crate) mod space_state;
pub(crate) mod warp;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "fleetObjectHandler" => {
            if let Some(reply) = fleet_object_handler::dispatch(c, call).await { return Some(reply); }
            if !call.bound { return None; }
        }
        "fleetMgr" => {
            if let Some(reply) = fleet_mgr::dispatch(c, call).await { return Some(reply); }
        }
        "fleetProxy" => {
            if let Some(reply) = fleet_proxy::dispatch(c, call).await { return Some(reply); }
        }
        _ => {}
    }
    super::fixed::fleets(call)
}

/// Apply session pairs to the calling client directly.
fn set_own_session(c: &mut Client, pairs: &[Value]) {
    let owned = infinity_core::session::decode_pairs(pairs);
    c.change_session(owned.iter().map(|(k, v)| (k.as_str(), v.clone())).collect());
}

#[cfg(test)]
mod tests;
