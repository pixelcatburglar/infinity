//! Hacking: the data and relic analyzer minigame (eve/client/script/ui/hacking).
//!
//! Activating an analyzer (`doHacking`) on a data or relic site container starts a game.
//! Analyzer range uses ship and container positions sampled at the same system-actor time.
//! The client sends each click (hackingMgr.ClickedOnTile, UsedUtilityElement,
//! UsedUtilityElementOnVirus, QuitHackingAttempt) and draws the turn's results from
//! OnHackingTurnComplete([{eventID, eventData}]) (hackingcommon.hackingConstants).
//!
//! The live server's board generator and node stats never shipped with the client: a
//! board here is a random hex blob grown from the entry node, with the core at its far
//! end and defense nodes, utility subsystems and data caches scattered by the site's tier.
//! Flipping a defense node blocks its hidden neighbours until it is destroyed; the virus
//! and a node trade strength against coherence on each attack; destroying the core wins
//! and opens the container, losing all coherence fails the attempt (it can be retried).

use super::{db_err, notify_error, rand01, Call, CallError, CallResult};
use crate::client::Client;
use crate::db::{Db, outbox::Notification};
use crate::sde::{types::TypeData, Sde};
use crate::services::{combat, effects::online::OnlinePolicy, identity::Identity};
use infinity_core::consts::{dogma::{ATTR_MAX_RANGE, ATTR_REQUIRED_SKILLS}, SKILL_ARCHAEOLOGY, SKILL_HACKING};
use infinity_core::messages;
use crate::space::{dist, metres_to_km, NotifyTx, World};
use marshal::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tracing::info;

// hackingcommon.hackingConstants
const GAMETYPE_HACKING: i64 = 0;
const GAMETYPE_ARCHEOLOGY: i64 = 1;
const TYPE_NONE: i64 = -1;
const TYPE_SEGMENT: i64 = 0;
const TYPE_VIRUS: i64 = 1;
const TYPE_CORE: i64 = 2;
const TYPE_DEFENSESOFTWARE: i64 = 3;
const TYPE_UTILITYELEMENTTILE: i64 = 5;
const TYPE_DATACACHE: i64 = 6;
const SUBTYPE_NONE: i64 = -1;
const UE_SELFREPAIR: i64 = 0;
const UE_KERNALROT: i64 = 1;
const UE_SECONDARYVECTOR: i64 = 2;
const UE_POLYMORPHICSHIELD: i64 = 3;
const CORE_HIGH: i64 = 10;
const CORE_LOW: i64 = 11;
const CORE_MEDIUM: i64 = 12;
const DS_ANTIVIRUS: i64 = 100;
const DS_FIREWALL: i64 = 101;
const DS_SUPPRESSOR: i64 = 102; // SUBTYPE_DS_HONEYPOT_STRENGTH
const DS_RESTORATION: i64 = 103; // SUBTYPE_DS_HONEYPOT_HEALING
const EVENT_GAME_LOST: i64 = 0;
const EVENT_GAME_WON: i64 = 1;
const EVENT_GAME_START: i64 = 2;
const EVENT_VIRUS_CREATED: i64 = 3;
const EVENT_OBJECT_KILLED: i64 = 4;
const EVENT_GAME_STYLE: i64 = 5;
const EVENT_TILE_FLIPPED: i64 = 100;
const EVENT_TILE_CREATED: i64 = 101;
const EVENT_TILE_BLOCKED: i64 = 102;
const EVENT_UE_PICKEDUP: i64 = 200;
const EVENT_UE_REMOVED: i64 = 201;
const EVENT_UE_INUSE: i64 = 202;
const EVENT_DATACACHE_OPEN: i64 = 300;
const EVENT_ATTACK: i64 = 301;
const EVENT_KERNALROT: i64 = 302;
const EVENT_SELFREPAIR: i64 = 303;
const EVENT_SECONDARYVECTOR: i64 = 304;
const EVENT_POLYMORPHICSHIELDHIT: i64 = 305;
const EVENT_HONEYPOT_STRENGTH: i64 = 306;
const EVENT_HONEYPOT_HEALING: i64 = 307;
const EVENT_DISTANCEINDICATOR: i64 = 308;

const ATTR_DURATION: i32 = 73;
const ATTR_VIRUS_COHERENCE: i32 = 1909;
const ATTR_VIRUS_STRENGTH: i32 = 1910;
const ATTR_VIRUS_SLOTS: i32 = 1911;
/// Hacking / Archaeology: +10 virus coherence per level.
const COHERENCE_PER_SKILL_LEVEL: f64 = 10.0;
const VIRUS_ID: i64 = 1;
const BOARD: i64 = 8;
/// Turns a durational utility subsystem lasts.
const UE_TURNS: i64 = 3;
/// Strength a virus suppressor takes while it stands.
const SUPPRESSION: i64 = 10;
const RESTORATION_HEAL: i64 = 10;

/// Match PostgreSQL's float-to-integer conversion for a required skill.
fn analyzer_skill(value: f64) -> Result<i32, CallError> {
    let rounded = value.round_ties_even();
    if !rounded.is_finite() || rounded < i32::MIN as f64 || rounded > i32::MAX as f64 {
        return Err(CallError::Internal(anyhow::anyhow!("analyzer required skill is out of integer range: {value}")));
    }
    Ok(rounded as i32)
}

/// Resolve the first hacking or archaeology prerequisite in dogma slot order.
pub(super) fn required_analyzer_skill(ty: &TypeData) -> Result<Option<i32>, CallError> {
    let mut found = None;
    for skill_attr in ATTR_REQUIRED_SKILLS {
        let Some(value) = ty.attribute(skill_attr) else { continue };
        let skill = analyzer_skill(value)?;
        if found.is_none() && matches!(skill, SKILL_HACKING | SKILL_ARCHAEOLOGY) {
            found = Some(skill);
        }
    }
    Ok(found)
}

type Coord = (i64, i64);

fn rand_below(n: usize) -> usize {
    (rand01() * n as f64) as usize % n.max(1)
}

/// Hex neighbours (hackingUIConst.NEIGHBOR_OFFSETS1 on odd rows, ..2 on even rows).
fn neighbours((x, y): Coord) -> [Coord; 6] {
    let offsets = if y.rem_euclid(2) == 1 {
        [(0, -1), (1, -1), (-1, 0), (1, 0), (0, 1), (1, 1)]
    } else {
        [(-1, -1), (0, -1), (-1, 0), (1, 0), (-1, 1), (0, 1)]
    };
    offsets.map(|(dx, dy)| (x + dx, y + dy))
}

#[derive(Clone)]
struct Tile {
    id: i64,
    kind: i64,
    sub: i64,
    coord: Coord,
    hidden: bool,
    blocked: bool,
    strength: i64,
    coherence: i64,
    max_coherence: i64,
}

impl Tile {
    fn hostile(&self) -> bool {
        !self.hidden && matches!(self.kind, TYPE_DEFENSESOFTWARE | TYPE_CORE) && self.coherence > 0
    }

    fn value(&self) -> Value {
        let hidden = self.hidden;
        Value::dict([
            ("id", Value::Int(self.id)),
            ("type", Value::Int(if hidden { TYPE_NONE } else { self.kind })),
            ("subtype", Value::Int(if hidden { SUBTYPE_NONE } else { self.sub })),
            ("coord", Value::tuple([Value::Int(self.coord.0), Value::Int(self.coord.1)])),
            ("blocked", Value::Bool(self.blocked)),
            ("hidden", Value::Bool(hidden)),
            ("strength", Value::Int(if hidden { 0 } else { self.strength })),
            ("coherence", Value::Int(if hidden { 0 } else { self.coherence })),
        ])
    }
}

#[derive(Clone)]
struct Ue {
    id: i64,
    sub: i64,
    info: i64,
    turns: i64,
    in_use: bool,
    target: Option<Coord>,
}

impl Ue {
    fn empty() -> Ue {
        Ue { id: 0, sub: SUBTYPE_NONE, info: 0, turns: 0, in_use: false, target: None }
    }

    fn of(sub: i64, id: i64) -> Ue {
        let info = match sub {
            UE_SELFREPAIR => 15,
            UE_SECONDARYVECTOR => 20,
            UE_POLYMORPHICSHIELD => 2,
            _ => 0,
        };
        Ue { id, sub, info, turns: 0, in_use: false, target: None }
    }

    /// UtilityElementData(**element) at creation takes only id, subtype and info.
    fn created_value(&self) -> Value {
        Value::dict([("id", Value::Int(self.id)), ("subtype", Value::Int(self.sub)), ("info", Value::Int(self.info))])
    }

    fn value(&self) -> Value {
        let remaining = if self.in_use { Value::Int(self.turns) } else { Value::None };
        Value::dict([
            ("id", Value::Int(self.id)),
            ("subtype", Value::Int(self.sub)),
            ("info", Value::Int(self.info)),
            ("durationRemaining", remaining),
        ])
    }
}

struct Game {
    container: i64,
    system: i64,
    tier: i64,
    tiles: HashMap<Coord, Tile>,
    coherence: i64,
    max_coherence: i64,
    strength: i64,
    inventory: Vec<Ue>,
    next_id: i64,
    /// UsedUtilityElement is followed by ClickedOnTile for the same click.
    skip_click: Option<Coord>,
}

impl Game {
    fn virus(&self) -> Value {
        Value::dict([
            ("id", Value::Int(VIRUS_ID)),
            ("type", Value::Int(TYPE_VIRUS)),
            ("coord", Value::tuple([Value::Int(-1), Value::Int(-1)])),
            ("strength", Value::Int(self.strength)),
            ("coherence", Value::Int(self.coherence)),
        ])
    }

    fn inventory(&self) -> Vec<Value> {
        self.inventory.iter().map(Ue::value).collect()
    }

    fn id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// A hidden tile can be flipped when a revealed, unblocked node touches it.
    fn flippable(&self, c: Coord) -> bool {
        self.tiles.get(&c).is_some_and(|t| t.hidden && !t.blocked)
            && neighbours(c).iter().any(|n| self.tiles.get(n).is_some_and(|t| !t.hidden && !t.blocked))
    }

    /// Hidden neighbours of a revealed defense node can't be flipped while it stands.
    fn guarded(&self, c: Coord) -> bool {
        neighbours(c).iter().any(|n| self.tiles.get(n).is_some_and(|t| t.hostile() && t.kind == TYPE_DEFENSESOFTWARE))
    }

    fn reblock_around(&mut self, c: Coord, ev: &mut Vec<Value>) {
        for n in neighbours(c) {
            let guarded = self.guarded(n);
            if let Some(t) = self.tiles.get_mut(&n).filter(|t| t.hidden && t.blocked != guarded) {
                t.blocked = guarded;
                ev.push(event(EVENT_TILE_BLOCKED, t.value()));
            }
        }
    }

    /// Hops from `from` to the core over the board.
    fn distance_to_core(&self, from: Coord) -> Option<i64> {
        let mut seen = HashSet::from([from]);
        let mut queue = VecDeque::from([(from, 0)]);
        while let Some((c, d)) = queue.pop_front() {
            if self.tiles.get(&c).is_some_and(|t| t.kind == TYPE_CORE) {
                return Some(d);
            }
            for n in neighbours(c) {
                if self.tiles.contains_key(&n) && seen.insert(n) {
                    queue.push_back((n, d + 1));
                }
            }
        }
        None
    }

    fn remove_ue(&mut self, index: usize, ev: &mut Vec<Value>) {
        if let Some(slot) = self.inventory.get_mut(index) {
            *slot = Ue::empty();
        }
        ev.push(event(EVENT_UE_REMOVED, Value::dict([("index", Value::Int(index as i64)), ("inventory", Value::dict([("inventory", Value::List(self.inventory()))]))])));
    }

    /// A node's coherence ran out. Returns true when it was the core.
    fn kill(&mut self, c: Coord, ev: &mut Vec<Value>) -> bool {
        let Some(t) = self.tiles.get_mut(&c) else { return false };
        if t.kind == TYPE_CORE {
            t.coherence = 0;
            ev.push(event(EVENT_OBJECT_KILLED, t.value()));
            return true;
        }
        let suppressor = t.sub == DS_SUPPRESSOR;
        t.kind = TYPE_SEGMENT;
        t.sub = SUBTYPE_NONE;
        t.coherence = 0;
        t.strength = 0;
        ev.push(event(EVENT_OBJECT_KILLED, t.value()));
        if suppressor {
            self.strength += SUPPRESSION;
            ev.push(event(EVENT_HONEYPOT_STRENGTH, Value::dict([("virus", self.virus())])));
        }
        self.reblock_around(c, ev);
        false
    }

    /// The node at `c` hits back; returns true when the virus is destroyed.
    fn counterattack(&mut self, c: Coord, ev: &mut Vec<Value>) -> bool {
        let Some(t) = self.tiles.get(&c).cloned() else { return false };
        if let Some(index) = self.inventory.iter().position(|u| u.in_use && u.sub == UE_POLYMORPHICSHIELD) {
            let shield = &mut self.inventory[index];
            shield.turns -= 1;
            ev.push(event(EVENT_POLYMORPHICSHIELDHIT, Value::dict([("index", Value::Int(index as i64)), ("durationRemaining", Value::Int(shield.turns))])));
            if shield.turns <= 0 {
                self.remove_ue(index, ev);
            }
            return false;
        }
        self.coherence = (self.coherence - t.strength).max(0);
        ev.push(event(EVENT_ATTACK, Value::dict([("attackerResult", t.value()), ("defenderResult", self.virus())])));
        self.coherence == 0
    }

    /// Restoration nodes mend their neighbours and running subsystems tick down.
    /// Returns (core destroyed, virus destroyed).
    fn end_turn(&mut self, ev: &mut Vec<Value>) -> (bool, bool) {
        let healers: Vec<Coord> = self.tiles.values().filter(|t| t.hostile() && t.sub == DS_RESTORATION).map(|t| t.coord).collect();
        for h in healers {
            let wounded: Vec<Coord> = self.tiles.values().filter(|t| t.hostile() && t.coord != h && t.coherence < t.max_coherence).map(|t| t.coord).collect();
            if wounded.is_empty() {
                continue;
            }
            let c = wounded[rand_below(wounded.len())];
            let t = self.tiles.get_mut(&c).unwrap();
            let amount = RESTORATION_HEAL.min(t.max_coherence - t.coherence);
            t.coherence += amount;
            let healed = t.value();
            ev.push(event(
                EVENT_HONEYPOT_HEALING,
                Value::dict([("coord", Value::tuple([Value::Int(h.0), Value::Int(h.1)])), ("healedObject", healed), ("healingAmount", Value::Int(amount))]),
            ));
        }
        let mut won = false;
        for index in 0..self.inventory.len() {
            let ue = self.inventory[index].clone();
            if !ue.in_use || !matches!(ue.sub, UE_SELFREPAIR | UE_SECONDARYVECTOR) {
                continue;
            }
            let turns = ue.turns - 1;
            self.inventory[index].turns = turns;
            if ue.sub == UE_SELFREPAIR {
                self.coherence = (self.coherence + ue.info).min(self.max_coherence);
                ev.push(event(EVENT_SELFREPAIR, Value::dict([("index", Value::Int(index as i64)), ("durationRemaining", Value::Int(turns)), ("target", self.virus())])));
            } else if let Some(c) = ue.target.filter(|c| self.tiles.get(c).is_some_and(Tile::hostile)) {
                let t = self.tiles.get_mut(&c).unwrap();
                t.coherence = (t.coherence - ue.info).max(0);
                let Value::Dict(mut fields) = t.value() else { unreachable!() };
                fields.push((Value::str("index"), Value::Int(index as i64)));
                fields.push((Value::str("durationRemaining"), Value::Int(turns)));
                ev.push(event(EVENT_SECONDARYVECTOR, Value::Dict(fields)));
                if self.tiles[&c].coherence == 0 {
                    won |= self.kill(c, ev);
                }
            }
            if turns <= 0 {
                self.remove_ue(index, ev);
            }
        }
        (won, self.coherence == 0)
    }
}

fn event(id: i64, data: Value) -> Value {
    Value::dict([("eventID", Value::Int(id)), ("eventData", data)])
}

/// Defense node (subtype, strength, coherence) for a board tier.
fn defense(tier: i64) -> (i64, i64, i64) {
    let kinds: &[i64] = if tier >= 2 { &[DS_FIREWALL, DS_ANTIVIRUS, DS_FIREWALL, DS_ANTIVIRUS, DS_SUPPRESSOR, DS_RESTORATION] } else { &[DS_FIREWALL, DS_ANTIVIRUS] };
    match kinds[rand_below(kinds.len())] {
        DS_FIREWALL => (DS_FIREWALL, 8 + 2 * tier, 60 + 10 * tier),
        DS_ANTIVIRUS => (DS_ANTIVIRUS, 15 + 5 * tier, 30 + 5 * tier),
        DS_SUPPRESSOR => (DS_SUPPRESSOR, 10, 40 + 5 * tier),
        _ => (DS_RESTORATION, 8, 50 + 5 * tier),
    }
}

/// A board: (tiles, entry). The entry node starts revealed.
fn board(tier: i64, mut next_id: impl FnMut() -> i64) -> (HashMap<Coord, Tile>, Coord) {
    let tier = tier.clamp(1, 4);
    let start: Coord = (1 + rand_below((BOARD - 2) as usize) as i64, BOARD - 1);
    let mut cells = vec![start];
    let mut set = HashSet::from([start]);
    let size = 26 + 4 * tier as usize;
    let mut guard = 0;
    while cells.len() < size && guard < 10_000 {
        guard += 1;
        let from = cells[rand_below(cells.len())];
        let n = neighbours(from)[rand_below(6)];
        if (0..BOARD).contains(&n.0) && (0..BOARD).contains(&n.1) && set.insert(n) {
            cells.push(n);
        }
    }
    // The core goes as far from the entry as the board reaches.
    let mut dist: HashMap<Coord, i64> = HashMap::from([(start, 0)]);
    let mut queue = VecDeque::from([start]);
    while let Some(c) = queue.pop_front() {
        let d = dist[&c];
        for n in neighbours(c) {
            if set.contains(&n) && !dist.contains_key(&n) {
                dist.insert(n, d + 1);
                queue.push_back(n);
            }
        }
    }
    let far = dist.values().copied().max().unwrap_or(0);
    let candidates: Vec<Coord> = dist.iter().filter(|(_, d)| **d == far).map(|(c, _)| *c).collect();
    let core = candidates[rand_below(candidates.len())];
    let mut rest: Vec<Coord> = cells.iter().copied().filter(|c| *c != start && *c != core).collect();
    for i in (1..rest.len()).rev() {
        rest.swap(i, rand_below(i + 1));
    }
    // A guard beside the core, then the rest anywhere.
    if let Some(i) = rest.iter().position(|c| neighbours(core).contains(c)) {
        let c = rest.remove(i);
        rest.insert(0, c);
    }
    let mut tiles = HashMap::new();
    let mut place = |c: Coord, kind: i64, sub: i64, strength: i64, coherence: i64, hidden: bool| {
        tiles.insert(c, Tile { id: next_id(), kind, sub, coord: c, hidden, blocked: false, strength, coherence, max_coherence: coherence });
    };
    place(start, TYPE_SEGMENT, SUBTYPE_NONE, 0, 0, false);
    let core_sub = match tier {
        1 => CORE_LOW,
        2 | 3 => CORE_MEDIUM,
        _ => CORE_HIGH,
    };
    place(core, TYPE_CORE, core_sub, 5 + 5 * tier, 40 + 15 * tier, true);
    let defenses = (2 + tier) as usize;
    let utilities = 2;
    let caches = 1 + (tier / 2) as usize;
    for (i, c) in rest.into_iter().enumerate() {
        if i < defenses {
            let (sub, strength, coherence) = defense(tier);
            place(c, TYPE_DEFENSESOFTWARE, sub, strength, coherence, true);
        } else if i < defenses + utilities {
            place(c, TYPE_UTILITYELEMENTTILE, rand_below(4) as i64, 0, 0, true);
        } else if i < defenses + utilities + caches {
            place(c, TYPE_DATACACHE, SUBTYPE_NONE, 0, 0, true);
        } else {
            place(c, TYPE_SEGMENT, SUBTYPE_NONE, 0, 0, true);
        }
    }
    (tiles, start)
}

fn games() -> MutexGuard<'static, HashMap<i64, Game>> {
    static GAMES: OnceLock<Mutex<HashMap<i64, Game>>> = OnceLock::new();
    GAMES.get_or_init(Default::default).lock().unwrap()
}

/// Activate(analyzerID, 'doHacking', containerID): open a game on the container.
/// `login_user` is the owning account used only for the hacking-start info log.
pub async fn start(
    identity: &Identity, db: &Db, world: &Arc<World>, sde: &Arc<Sde>, space_system: Option<i64>,
    sender: NotifyTx, login_user: i64, module: i64, target: Option<i64>,
    mut notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> CallResult {
    let Some(cx) = combat::Ctx::from_identity(identity, db, world, space_system, sender.clone()) else {
        return Ok(Value::None);
    };
    let (system, ship, char_id) = (cx.system_id, cx.ship_id, cx.char_id);
    let Some(container) = target else { return Ok(Value::None) };
    let at = crate::client::filetime_now();
    let Some(site) = super::exploration::hack_target(container) else {
        return Err(notify_error(messages::ANALYZER_TARGET_MUST_BE_DATA_OR_RELIC));
    };
    if site.hacked {
        return Err(notify_error(messages::HACK_CONTAINER_ALREADY_HACKED));
    }
    let Some(module_type) = crate::dao::items::hacking::analyzer(db, module, ship).await.map_err(db_err)? else { return Ok(Value::None) };
    let attr = |id, default| sde.types.attribute(module_type, id).unwrap_or(default);
    let skill = match sde.types.get(module_type) {
        Some(ty) => required_analyzer_skill(ty)?,
        None => None,
    };
    let range = attr(ATTR_MAX_RANGE, 6000.0);
    let duration = attr(ATTR_DURATION, 5000.0);
    let needed = if site.relic { SKILL_ARCHAEOLOGY } else { SKILL_HACKING };
    if skill != Some(needed) {
        return Err(notify_error(if site.relic { messages::RELIC_CONTAINER_REQUIRES_RELIC_ANALYZER } else { messages::DATA_CONTAINER_REQUIRES_DATA_ANALYZER }));
    }
    let Some(gap) = world.with(system, move |s| {
        let (me, can) = (s.ball(ship)?, s.ball(container)?);
        Some(dist(s.sample_position(ship)?, s.sample_position(container)?) - me.radius as f64 - can.radius as f64)
    }).await.flatten() else {
        return Err(CallError::User { msg: messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT.into(), dict: vec![] });
    };
    if gap > range {
        return Err(notify_error(&messages::analyzer_range(metres_to_km(gap), metres_to_km(range))));
    }
    let skills = crate::services::skills::effective::snapshot_at(db, sde, char_id, at).await?;
    let level = skills.level(needed) as f64;
    let coherence = (attr(ATTR_VIRUS_COHERENCE, 40.0) + COHERENCE_PER_SKILL_LEVEL * level).round() as i64;
    let strength = attr(ATTR_VIRUS_STRENGTH, 20.0).round() as i64;
    let slots = attr(ATTR_VIRUS_SLOTS, 3.0).round().clamp(1.0, 6.0) as usize;

    let mut next_id = 100;
    let (tiles, _) = board(site.tier, || {
        next_id += 1;
        next_id
    });
    let game = Game {
        container,
        system,
        tier: site.tier,
        tiles,
        coherence,
        max_coherence: coherence,
        strength,
        inventory: vec![Ue::empty(); slots],
        next_id,
        skip_click: None,
    };
    let mut ev = vec![event(EVENT_GAME_STYLE, Value::dict([("style", Value::Int(if site.relic { GAMETYPE_ARCHEOLOGY } else { GAMETYPE_HACKING }))]))];
    ev.extend(game.tiles.values().map(|t| event(EVENT_TILE_CREATED, t.value())));
    let Value::Dict(mut virus) = game.virus() else { unreachable!() };
    virus.push((Value::str("inventory"), Value::List(game.inventory.iter().map(Ue::created_value).collect())));
    ev.push(event(EVENT_VIRUS_CREATED, Value::Dict(virus)));
    ev.push(event(EVENT_GAME_START, Value::dict([("moduleTypeID", Value::Int(module_type as i64))])));
    let policy = OnlinePolicy::from_identity(identity, sender)?;
    super::effects::hacking::start_effect(cx, sde, &policy, module, container, duration).await?;
    games().insert(char_id, game);
    super::exploration::set_container_state(world, system, container, 1).await;
    info!(user = login_user, "hacking {container} (tier {}, virus {coherence}/{strength}, {slots} slots)", site.tier);
    notify(Notification::DirectClient { event: "OnHackingTurnComplete".into(), args: vec![Value::List(ev)] })?;
    Ok(Value::None)
}

enum Outcome {
    Playing,
    Won,
    Lost,
}

fn coord_arg(v: Option<&Value>) -> Option<Coord> {
    match v?.items()? {
        [x, y] => Some((x.as_int()?, y.as_int()?)),
        _ => None,
    }
}

/// One player action; returns the turn's events and how the game stands.
fn play(g: &mut Game, action: &str, call: &Call<'_>) -> (Vec<Value>, Outcome) {
    let mut ev = vec![];
    match action {
        "ClickedOnTile" => {
            let Some(c) = coord_arg(call.args.first()) else { return (ev, Outcome::Playing) };
            if g.skip_click.take() == Some(c) {
                return (ev, Outcome::Playing);
            }
            let Some(t) = g.tiles.get(&c).cloned() else { return (ev, Outcome::Playing) };
            if t.hidden {
                if !g.flippable(c) {
                    return (ev, Outcome::Playing);
                }
                let t = g.tiles.get_mut(&c).unwrap();
                t.hidden = false;
                ev.push(event(EVENT_TILE_FLIPPED, t.value()));
                match t.kind {
                    TYPE_DEFENSESOFTWARE => {
                        let suppressor = t.sub == DS_SUPPRESSOR;
                        g.reblock_around(c, &mut ev);
                        if suppressor {
                            g.strength = (g.strength - SUPPRESSION).max(1);
                            ev.push(event(EVENT_HONEYPOT_STRENGTH, Value::dict([("virus", g.virus())])));
                        }
                    }
                    TYPE_UTILITYELEMENTTILE => pick_up(g, c, &mut ev),
                    TYPE_SEGMENT => {
                        if let Some(d) = g.distance_to_core(c).filter(|d| *d <= 5) {
                            let Value::Dict(mut fields) = g.tiles[&c].value() else { unreachable!() };
                            fields.push((Value::str("distanceIndicator"), Value::Int(d)));
                            ev.push(event(EVENT_DISTANCEINDICATOR, Value::Dict(fields)));
                        }
                    }
                    _ => {}
                }
            } else if t.hostile() {
                let target = g.tiles.get_mut(&c).unwrap();
                target.coherence = (target.coherence - g.strength).max(0);
                let defender = target.value();
                let dead = target.coherence == 0;
                ev.push(event(EVENT_ATTACK, Value::dict([("attackerResult", g.virus()), ("defenderResult", defender)])));
                if dead {
                    if g.kill(c, &mut ev) {
                        ev.push(event(EVENT_GAME_WON, Value::Dict(vec![])));
                        return (ev, Outcome::Won);
                    }
                } else if g.counterattack(c, &mut ev) {
                    ev.push(event(EVENT_OBJECT_KILLED, g.virus()));
                    ev.push(event(EVENT_GAME_LOST, Value::Dict(vec![])));
                    return (ev, Outcome::Lost);
                }
            } else if t.kind == TYPE_DATACACHE {
                // A cache holds a subsystem, or a defense node that wakes up.
                let ds = rand01() < 0.4;
                let fresh = if ds { defense(g.tier) } else { (rand_below(4) as i64, 0, 0) };
                let tile = g.tiles.get_mut(&c).unwrap();
                if ds {
                    (tile.kind, tile.sub, tile.strength, tile.coherence, tile.max_coherence) = (TYPE_DEFENSESOFTWARE, fresh.0, fresh.1, fresh.2, fresh.2);
                } else {
                    (tile.kind, tile.sub) = (TYPE_UTILITYELEMENTTILE, fresh.0);
                }
                ev.push(event(EVENT_DATACACHE_OPEN, tile.value()));
                if ds {
                    g.reblock_around(c, &mut ev);
                }
            } else if t.kind == TYPE_UTILITYELEMENTTILE {
                pick_up(g, c, &mut ev);
            }
        }
        "UsedUtilityElementOnVirus" => {
            let index = call.args.first().and_then(|v| v.as_int()).unwrap_or(-1) as usize;
            let Some(ue) = g.inventory.get_mut(index).filter(|u| !u.in_use && matches!(u.sub, UE_SELFREPAIR | UE_POLYMORPHICSHIELD)) else {
                return (ev, Outcome::Playing);
            };
            ue.in_use = true;
            ue.turns = if ue.sub == UE_POLYMORPHICSHIELD { ue.info } else { UE_TURNS };
            ev.push(event(EVENT_UE_INUSE, Value::dict([("index", Value::Int(index as i64)), ("value", Value::Int(1))])));
        }
        "UsedUtilityElement" => {
            let index = call.args.first().and_then(|v| v.as_int()).unwrap_or(-1) as usize;
            let Some(c) = coord_arg(call.args.get(1)) else { return (ev, Outcome::Playing) };
            if !g.tiles.get(&c).is_some_and(Tile::hostile) {
                return (ev, Outcome::Playing);
            }
            let Some(ue) = g.inventory.get(index).filter(|u| !u.in_use).cloned() else { return (ev, Outcome::Playing) };
            g.skip_click = Some(c);
            match ue.sub {
                UE_KERNALROT => {
                    let t = g.tiles.get_mut(&c).unwrap();
                    t.coherence -= t.coherence / 2;
                    ev.push(event(EVENT_KERNALROT, t.value()));
                    g.remove_ue(index, &mut ev);
                }
                UE_SECONDARYVECTOR => {
                    let u = &mut g.inventory[index];
                    u.in_use = true;
                    u.turns = UE_TURNS;
                    u.target = Some(c);
                    ev.push(event(EVENT_UE_INUSE, Value::dict([("index", Value::Int(index as i64)), ("value", Value::Int(1))])));
                }
                _ => return (ev, Outcome::Playing),
            }
        }
        _ => return (ev, Outcome::Playing),
    }
    match g.end_turn(&mut ev) {
        (true, _) => {
            ev.push(event(EVENT_GAME_WON, Value::Dict(vec![])));
            (ev, Outcome::Won)
        }
        (_, true) => {
            ev.push(event(EVENT_OBJECT_KILLED, g.virus()));
            ev.push(event(EVENT_GAME_LOST, Value::Dict(vec![])));
            (ev, Outcome::Lost)
        }
        _ => (ev, Outcome::Playing),
    }
}

/// Take a revealed utility subsystem into a free slot (it stays on the board otherwise).
fn pick_up(g: &mut Game, c: Coord, ev: &mut Vec<Value>) {
    let Some(slot) = g.inventory.iter().position(|u| u.sub == SUBTYPE_NONE) else { return };
    let sub = g.tiles[&c].sub;
    let id = g.id();
    g.inventory[slot] = Ue::of(sub, id);
    let t = g.tiles.get_mut(&c).unwrap();
    t.kind = TYPE_SEGMENT;
    t.sub = SUBTYPE_NONE;
    let replacement = t.value();
    ev.push(event(
        EVENT_UE_PICKEDUP,
        Value::dict([("coord", Value::tuple([Value::Int(c.0), Value::Int(c.1)])), ("replacementObject", replacement), ("inventory", Value::List(g.inventory()))]),
    ));
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "hackingMgr" {
        return None;
    }
    let char_id = c.session.int("charid")?;
    if call.method == "QuitHackingAttempt" {
        let game = games().remove(&char_id);
        if let Some(g) = game {
            super::exploration::set_container_state(&c.world, g.system, g.container, 0).await;
        }
        return Some(Ok(Value::None));
    }
    if !matches!(call.method, "ClickedOnTile" | "UsedUtilityElement" | "UsedUtilityElementOnVirus") {
        // StartNewGameInstance is the client's developer test board.
        return Some(Ok(Value::None));
    }
    let (events, outcome, container, system) = {
        let mut all = games();
        let Some(g) = all.get_mut(&char_id) else { return Some(Ok(Value::None)) };
        let (events, outcome) = play(g, call.method, call);
        (events, outcome, g.container, g.system)
    };
    if !events.is_empty() {
        c.notify("OnHackingTurnComplete", vec![Value::List(events)]);
    }
    match outcome {
        Outcome::Playing => {}
        Outcome::Won => {
            games().remove(&char_id);
            info!(user = c.login.user_id, "hacked {container}");
            let context = super::exploration::fact_context(c, crate::lua::events::resources::FactOrigin::Natural);
            if let Err(e) = super::exploration::container_hacked(&c.db, &c.world, container, char_id, context).await {
                return Some(Err(e));
            }
        }
        Outcome::Lost => {
            games().remove(&char_id);
            info!(user = c.login.user_id, "failed to hack {container}");
            super::exploration::set_container_state(&c.world, system, container, 0).await;
        }
    }
    Some(Ok(Value::None))
}

#[cfg(test)] mod tests;

