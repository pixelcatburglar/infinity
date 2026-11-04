//! Targeting, ammunition, weapon groups and weapons fire.
//!
//! The client runs its own dogma for the active ship (clientDogmaLocation) and asks the
//! server through the bound dogmaIM: `AddTarget` (targetMgr._LockTarget), `LoadAmmo` /
//! `UnloadAmmo`, `LinkWeapons`..., and `Activate` / `Deactivate` (godma). The server
//! answers with notifications: `OnTarget`, `OnModuleAttributeChanges` (charge quantities
//! of the (shipID, flagID, typeID) charge sublocation), `OnGodmaShipEffect` (cycle
//! start/stop), `OnDamageMessage`, and destiny events for everyone in the system
//! (`OnSpecialFX`, `OnDamageStateChange`, missiles, `TerminalPlayDestructionEffect`).
//!
//! Weapons use their effective attributes (services::fit: skills, hull bonuses, damage
//! mods, charges): turret hit chance from optimal/falloff with a random hit quality,
//! missiles from flight range and explosion radius vs. signature, applied through shield,
//! armor and structure with their resonances. Every cycle draws capacitor (recharging on
//! the game's curve), reloading takes the module's reload time in space, and spawned NPCs
//! (spawn_npc) engage their mission runner and whoever shoots them: orbit, lock, shoot back
//! and repair themselves; a pilot's ship can be destroyed. Active modules apply their effects
//! (hardeners through fit, propulsion speed via queued destiny commands, repairers each cycle), and a kill
//! pays the NPC's bounty and leaves a wreck with loot.
//! NPC effects, charge dimensions and wreck mappings come from immutable SDE datasets;
//! NPC projected effects use SDE effect kinds and duration attributes, with
//! actor-owned cycles and the shared projected-effect evaluator.
//! loot prices come from `game.retail_prices` and static eligibility is applied before sampling.
//! Target locks, reloads, module cycles, pulses and missile impacts use system
//! actor timers. Database-backed effects run on deferred workers outside the actor.
//! Targeting and weapon range decisions use positions sampled together by the system actor;
//! wrecks and ejected pilots use the position sampled when their ball is destroyed.
//! Reload reservations survive interruptions and return to cargo during startup; reload
//! timers remain memory-only and are not resumed.
//! Dropped cycle owners release their own in-memory activation markers without notifications.
//! Reset clears a pilot's targets and pending locks and stops only that pilot's modules.
//! Explicit lock removal also removes the matching outgoing projected effects.
//! The shared incoming-targeter query combines player locks and NPC hostiles as ball IDs.
//! Ship condition is persisted in game.item_attributes, with shield recharge time on
//! the shield override; capacitor and in-flight effects remain runtime-only.
//! Space damage, repair and GM condition changes serialize with final condition saves;
//! removed balls reject queued effects from their former system.
//! Persistent unpiloted hulls use the same condition cache and durable overrides;
//! their condition is prepared before publication and installed by the winning loader.
//! Their destruction removes inventory without wrecks, payouts or pilot consequences.
//! Missiles in flight use a temporary, nonpersistent ID namespace.
//! LoadAmmo accepts an owned active or assembled docked fitting ship and owned charge
//! stacks in its holds, the current hangar, or a directly held singleton container.
//! eve/client/script/dogma/clientDogmaLocation.py:980-1001,1072 forwards the actual
//! source location; eve/client/script/ui/shared/fittingScreen/tryFit.py:187-223 and
//! eve/client/script/ui/shared/fitting/fittingUtil.py:226-237 accept inventory sources.
//! eve/client/script/dogma/clientDogmaLocation.py:91-120 offers singleton charges
//! through `LoadChargesToModule` as well as stacked charges.
//! eve/common/script/util/inventoryFlagsCommon.py:9-15,53-60,217-226 describes cargo,
//! fleet and ammo holds. Remote, foreign and recursively nested containers are unsupported.

use crate::dao::items::loading::{Item, ItemQuery};
use super::items::load_items;
use super::inventory::notifications::{notify_item_change, notify_item_removed};
use crate::dao::items::quantities::{self, QuantityChange};
use super::fit::{Fit, Key};
use super::{db_err, notify_error, rand01, user_error, value_id_list, Call, CallError, CallResult};
use super::identity::Identity;
use crate::client::{filetime_now, filetime_ticks_from_ms, Client};
use crate::consts::{ACCOUNT_CASH, dogma::ATTR_ARMOR_DAMAGE, dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_CAPACITY, dogma::ATTR_CHARGE, dogma::ATTR_EXPLOSION_DELAY, dogma::ATTR_HULL_DAMAGE, dogma::ATTR_MAX_RANGE, dogma::ATTR_MAX_VELOCITY, dogma::ATTR_QUANTITY, dogma::ATTR_SHIELD_CAPACITY, dogma::ATTR_SHIELD_CHARGE, dogma::ATTR_SIGNATURE_RADIUS, dogma::ATTR_STRUCTURE_HP, dogma::ATTR_FALLOFF, dogma::ATTR_AOE_CLOUD_SIZE, dogma::ATTR_MISSILE_DAMAGE_MULTIPLIER, CATEGORY_CHARGE, DAMAGE_TYPE_COUNT, FILETIME_SECOND, FLAG_CARGO, FLAG_HANGAR, IX_FLAG, IX_LOCATION_ID, IX_STACK_SIZE};
use crate::consts::{dogma::ATTR_DAMAGE_MULTIPLIER, dogma::ATTR_RATE_OF_FIRE, dogma::ATTR_EM_DAMAGE, dogma::ATTR_THERMAL_DAMAGE, dogma::ATTR_KINETIC_DAMAGE, dogma::ATTR_EXPLOSIVE_DAMAGE};
use crate::consts::{dogma::ATTR_ARMOR_HP, dogma::ATTR_SHIELD_EM_DAMAGE_RESONANCE, dogma::ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, dogma::ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, dogma::ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, dogma::ATTR_ARMOR_EM_DAMAGE_RESONANCE, dogma::ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE, dogma::ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, dogma::ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, dogma::ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, dogma::ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE, dogma::ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, dogma::ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, dogma::ATTR_SCAN_RESOLUTION, dogma::ATTR_MAX_TARGET_RANGE, dogma::ATTR_MAX_LOCKED_TARGETS, dogma::ATTR_RECHARGE_RATE, dogma::ATTR_SHIELD_RECHARGE_RATE, dogma::ATTR_RELOAD_TIME, dogma::ATTR_NEXT_ACTIVATION_TIME, dogma::ATTR_ENTITY_FLY_RANGE, dogma::ATTR_ENTITY_ATTACK_RANGE, dogma::ATTR_ENTITY_MISSILE_TYPE, dogma::ATTR_ENTITY_KILL_BOUNTY, dogma::ATTR_ENTITY_SHIELD_BOOST_AMOUNT, dogma::ATTR_ENTITY_SHIELD_BOOST_DURATION, dogma::ATTR_ENTITY_ARMOR_REPAIR_AMOUNT, dogma::ATTR_ENTITY_ARMOR_REPAIR_DURATION};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{Outbox, dispatch_client, dispatch_world};
use crate::messages;
use crate::sde::Sde;
use crate::space::{dist, lock_time_ms, metres_to_km, ms_to_seconds, now_ms, Ball, NotifyTx, NpcAction, NpcBrain, NpcConfig, NpcEffects, NpcFuture, World};
use marshal::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tracing::{debug, info, warn};

const HI_SLOTS: std::ops::RangeInclusive<i32> = 27..=34;
/// inventorycommon/const.py:177, flagSpecializedAmmoHold.
const FLAG_AMMO_HOLD: i32 = 143;
/// inventorycommon/const.py:189, flagFleetHangar.
const FLAG_FLEET_HANGAR: i32 = 155;
pub(super) const ATTR_DAMAGE: [i32; 4] = [ATTR_EM_DAMAGE, ATTR_THERMAL_DAMAGE, ATTR_KINETIC_DAMAGE, ATTR_EXPLOSIVE_DAMAGE];
const ATTR_HP: [i32; 3] = [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP];
const ATTR_RESONANCE: [[i32; 4]; 3] = [
    [ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE],
    [ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE, ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE],
    [ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE, ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE],
];

/// Existing server reload fallback when dogma has no reload time.
const DEFAULT_RELOAD_MS: f64 = 10_000.0;
/// Server fallback missile velocity in metres per second when NPC charge dogma is missing it.
const NPC_MISSILE_VELOCITY_MPS: f64 = 3_000.0;
/// Server fallback missile flight time in milliseconds when NPC charge dogma is missing it.
const NPC_MISSILE_FLIGHT_MS: f64 = 5_000.0;
/// Server fallback explosion radius in metres when NPC charge dogma is missing it.
const NPC_MISSILE_EXPLOSION_RADIUS_METRES: f64 = 40.0;
/// Server policy for NPC turret reach beyond optimal, in multiples of falloff.
const NPC_TURRET_REACH_FALLOFF_MULTIPLIER: f64 = 3.0;
static NEXT_LOCK: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------- state

#[derive(Clone)]
struct Hp {
    now: [f64; 3],
    max: [f64; 3],
    resonance: [[f64; 4]; 3],
}

pub(super) struct ActiveModule {
    pub(super) char_id: i64,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) forced_inactive: bool,
}

#[derive(Default)]
pub(super) struct State {
    /// charID -> locked targets
    targets: HashMap<i64, HashSet<i64>>,
    /// (charID, targetID) -> lock attempt in progress
    locking: HashMap<(i64, i64), u64>,
    /// shipID -> independent jammer identities and their deadlines.
    jammed: HashMap<i64, HashMap<(i64, i64), jamming::Jam>>,
    /// Announced locks, with source ship and target pilot for symmetric loss notices.
    announced_locks: HashMap<(i64, i64), (i64, i64)>,
    /// moduleID -> owner and stop flag of its running cycle
    pub(super) active: HashMap<i64, ActiveModule>,
    /// Most recent activation, retained to invalidate deferred propulsion updates.
    pub(super) activation_generation: HashMap<i64, Arc<AtomicBool>>,
    /// ballID -> hit points left (NPCs)
    hp: HashMap<i64, Hp>,
    /// shipID -> (capacitor level, FILETIME it was measured)
    capacitor: HashMap<i64, (f64, i64)>,
    /// shipID -> a player ship's damage
    condition: HashMap<i64, Condition>,
    /// moduleID -> FILETIME its reload finishes
    pub(super) reloading: HashMap<i64, i64>,
}

impl State {
    fn reset_character(&mut self, char_id: i64) {
        self.targets.remove(&char_id);
        self.locking.retain(|(ch, _), _| *ch != char_id);
        for active in self.active.values().filter(|active| active.char_id == char_id) {
            active.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// Modules currently running their activation effect (services::fit applies them).
pub fn active_modules() -> HashSet<i64> {
    state().active.iter().filter_map(|(&id, active)| (!active.forced_inactive).then_some(id)).collect()
}

pub(super) fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

/// What a cycle task needs once the call that started it has returned.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub(super) db: Db,
    pub(super) world: Arc<World>,
    pub(super) notify: NotifyTx,
    pub(super) char_id: i64,
    pub(super) ship_id: i64,
    pub(super) system_id: i64,
}

impl Ctx {
    pub(crate) fn from_identity(identity: &Identity, db: &Db, world: &Arc<World>, space_system: Option<i64>, sender: NotifyTx) -> Option<Self> {
        if identity.char_id == 0 { return None; }
        Some(Self {
            db: db.clone(),
            world: world.clone(),
            notify: sender,
            char_id: identity.char_id,
            ship_id: identity.ship_id?,
            system_id: space_system?,
        })
    }

    pub(super) fn notify(&self, event: &str, args: Vec<Value>) {
        let _ = self.notify.send((event.to_string(), args));
    }
}

pub(super) fn ctx(c: &Client) -> Option<Ctx> {
    Ctx::from_identity(&Identity::of_client(c), &c.db, &c.world, c.session.int("solarsystemid"), c.notify_tx.clone())
}


pub(super) fn attr(sde: &Sde, type_id: i32, id: i32, default: f64) -> f64 {
    sde.types.attribute(type_id, id).unwrap_or(default)
}

/// Stop everything a character was doing in space (docking, jumping, logging off).
pub fn reset(c: &Client) {
    let Some(char_id) = c.session.int("charid") else { return };
    reset_pilot(&c.world, char_id, c.session.int("solarsystemid").zip(c.session.int("shipid")));
}

pub(crate) fn reset_pilot(world: &crate::space::World, char_id: i64, space_ship: Option<(i64, i64)>) {
    super::effects::heat::clear_character(char_id);
    clear_character_locks(world, char_id);
    state().reset_character(char_id);
    if let Some((system, ship)) = space_ship {
        world.post(system, move |s| { jamming::clear(s, ship); s.remove_projections_from(ship); });
    }
}


pub(in crate::services) mod reload_recovery;
pub(crate) use reload_recovery::recover_pending_reloads;
pub(crate) mod targeting_charges;
pub(crate) mod jamming;
use targeting_charges::*;
pub(in crate::services) mod banks;
use banks::weapon_groups;
pub(crate) mod groups_health;
pub(crate) mod unpiloted;
pub(crate) mod drone_health;
use groups_health::*;
use super::effects::notify::attribute_change;
use super::effects::lifecycle;
pub(crate) mod npc;
use npc::*;

pub(super) fn quantity_change(owner: i64, ship: i64, flag: i32, type_id: i32, new: i64, old: i64) -> Value {
    Value::list([attribute_change(owner, charge_key(ship, flag, type_id), ATTR_QUANTITY, Value::Int(new), Value::Int(old))])
}

pub async fn flag_quantities(c: &Client, ship: i64) -> Result<(Value, Vec<Value>), CallError> {
    let rows = crate::dao::items::charges::all(&c.db, ship).await.map_err(db_err)?;
    let header = marshal::DbRowDescriptor::new([
        ("locationID", marshal::DbType::I8),
        ("flagID", marshal::DbType::I2),
        ("typeID", marshal::DbType::I4),
        ("quantity", marshal::DbType::I4),
    ]);
    let mut by_flag = Vec::new();
    let mut keys = Vec::new();
    for r in &rows {
        let (flag, type_id, qty) = (r.flag_id, r.type_id, r.quantity);
        by_flag.push((
            Value::Int(flag as i64),
            header.row(&[Value::Int(ship), Value::Int(flag as i64), Value::Int(type_id as i64), Value::Int(qty as i64)]),
        ));
        keys.push(charge_key(ship, flag, type_id));
    }
    let cache = if by_flag.is_empty() { Value::Dict(vec![]) } else { Value::Dict(vec![(Value::Int(ship), Value::Dict(by_flag))]) };
    Ok((cache, keys))
}

pub async fn banks(db: &(impl Executor + ?Sized), ship: i64) -> Result<Value, CallError> {
    let rows = crate::dao::items::charges::banks(db, ship).await.map_err(db_err)?;
    let mut by_master: Vec<(i64, Vec<Value>)> = Vec::new();
    for r in rows {
        let (master, slave) = (r.master_id, r.slave_id);
        match by_master.iter_mut().find(|(m, _)| *m == master) {
            Some((_, v)) => v.push(Value::Int(slave)),
            None => by_master.push((master, vec![Value::Int(slave)])),
        }
    }
    Ok(Value::Dict(by_master.into_iter().map(|(m, s)| (Value::Int(m), Value::List(s))).collect()))
}

pub(crate) async fn targeters(world: &World, system: i64, ship: i64) -> Vec<i64> {
    let incoming = targeters_of(ship);
    let candidates = world.with(system, move |s| {
        let mut candidates = s.hostiles_targeting(ship);
        candidates.extend(incoming.into_iter().filter_map(|pilot| s.pilot_ball(pilot).map(|ball| ball.id)));
        candidates.sort_unstable();
        candidates.dedup();
        candidates
    }).await.unwrap_or_default();
    candidates
}

pub fn ship_condition(ship: i64) -> Option<(f64, f64, f64)> {
    let st = state();
    let c = st.condition.get(&ship)?;
    let shield = recharge(c.now[0], c.stats.max[0], c.stats.shield_tau_ms, (filetime_now() - c.shield_at) as f64 / FILETIME_SECOND as f64);
    Some((shield, c.stats.max[1] - c.now[1], c.stats.max[2] - c.now[2]))
}

/// Persist the current runtime condition; serialize capture with paid repair.
pub(crate) async fn save_condition(db: &Db, ship: i64) -> Result<(), CallError> {
    let _lock = super::market::lock().await;
    save_condition_in(db, ship).await
}

/// Capture and persist cached condition using an existing executor and lifecycle lock.
pub(crate) async fn save_condition_in(ex: &(impl Executor + ?Sized), ship: i64) -> Result<(), CallError> {
    let Some(cond) = state().condition.get(&ship).cloned() else { return Ok(()) };
    persist_condition_in(ex, ship, &cond).await
}

async fn persist_condition_in(ex: &(impl Executor + ?Sized), ship: i64, cond: &Condition) -> Result<(), CallError> {
    let now = filetime_now();
    let shield = recharge(cond.now[0], cond.stats.max[0], cond.stats.shield_tau_ms, now.saturating_sub(cond.shield_at) as f64 / FILETIME_SECOND as f64);
    crate::dao::items::dogma::save_condition(ex, ship, &[ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE], &[shield, cond.stats.max[1] - cond.now[1], cond.stats.max[2] - cond.now[2]], ATTR_SHIELD_CHARGE, now).await.map_err(db_err)
}

/// Reload a docked ship's durable condition and advance shield recharge while offline.
/// In-space ships must use `load_condition_with_fit` with a fit loaded in space.
pub(crate) async fn load_docked_condition(db: &Db, world: &World, char_id: i64, ship: i64) -> Result<[f64; 3], CallError> {
    let fit = Fit::load(db, world.sde().clone(), char_id, ship).await?;
    load_condition_with_fit(db, ship, &fit).await
}

/// A restored condition that is not visible until installed.
pub(crate) struct ConditionSeed(Condition);

impl ConditionSeed {
    pub(crate) fn fractions(&self) -> [f64; 3] {
        [0, 1, 2].map(|i| if self.0.stats.max[i] > 0.0 { self.0.now[i] / self.0.stats.max[i] } else { 0.0 })
    }

    pub(crate) fn install(self, ship: i64) {
        state().condition.insert(ship, self.0);
    }
}

pub(crate) async fn prepare_condition_with_fit(ex: &(impl Executor + ?Sized), ship: i64, fit: &Fit) -> Result<ConditionSeed, CallError> {
    let stats = ShipStats::from_fit(fit, Key::Ship);
    let rows = crate::dao::items::dogma::overrides(ex, ship).await.map_err(db_err)?;
    let at = rows.iter().find(|r| r.attribute_id == ATTR_SHIELD_CHARGE).map(|r| r.updated_at).unwrap_or(0);
    let value = |id| rows.iter().find(|r| r.attribute_id == id).map(|r| r.value);
    let now = filetime_now();
    let (levels, _) = restored_condition(&stats, [value(ATTR_SHIELD_CHARGE), value(ATTR_ARMOR_DAMAGE), value(ATTR_HULL_DAMAGE)], at, now);
    Ok(ConditionSeed(Condition { stats, now: levels, shield_at: now }))
}

pub(super) async fn load_condition_with_fit(db: &Db, ship: i64, fit: &Fit) -> Result<[f64; 3], CallError> {
    let seed = prepare_condition_with_fit(db, ship, fit).await?;
    let health = seed.fractions();
    seed.install(ship);
    Ok(health)
}

fn restored_condition(stats: &ShipStats, values: [Option<f64>; 3], at: i64, now: i64) -> ([f64; 3], [f64; 3]) {
    let shield = values[0].unwrap_or(stats.max[0]).clamp(0.0, stats.max[0].max(0.0));
    let shield = if at > 0 { recharge(shield, stats.max[0], stats.shield_tau_ms, now.saturating_sub(at).max(0) as f64 / FILETIME_SECOND as f64) } else { shield };
    let levels = [shield, (stats.max[1] - values[1].unwrap_or(0.0)).clamp(0.0, stats.max[1].max(0.0)), (stats.max[2] - values[2].unwrap_or(0.0)).clamp(0.0, stats.max[2].max(0.0))];
    let health = [0, 1, 2].map(|i| if stats.max[i] > 0.0 { levels[i] / stats.max[i] } else { 0.0 });
    (levels, health)
}

/// Drop only runtime condition after a committed repair.
pub(crate) fn invalidate_condition(ship: i64) {
    state().condition.remove(&ship);
}

/// Persist a GM's full ship repair before exposing it to the client.
pub(crate) async fn gm_repair(db: &Db, world: &World, system: i64, char_id: i64, ship: i64) -> Result<(), CallError> {
    let _lock = super::market::lock().await;
    let stats = ship_stats(db, world, system, char_id, ship).await?;
    let cond = Condition { now: stats.max, stats, shield_at: filetime_now() };
    persist_condition_in(db, ship, &cond).await?;
    invalidate_condition(ship);
    Ok(())
}

pub fn forget_ship(ship: i64) {
    let mut st = state();
    st.condition.remove(&ship);
    st.capacitor.remove(&ship);
    st.hp.remove(&ship);
}
/// Clear targeting and cached health after a GM removes a spawned ball.
pub(crate) fn forget_gm_spawn(world: &World, id: i64) {
    forget_target(world, id);
}


pub(super) async fn destroy_player_ship(world: &World, system: i64, ship: i64, char_id: i64) -> bool {
    forget_ship(ship);
    if let Some(b) = world.destroy_ball(system, ship).await {
        forget_target(world, ship);
        world.notify_pilot(char_id, "__ship_destroyed", vec![Value::Int(ship), Value::Float(b.pos[0]), Value::Float(b.pos[1]), Value::Float(b.pos[2])]);
        true
    } else {
        false
    }
}


pub async fn spawn_npc(db: Db, world: Arc<World>, system: i64, id: i64, type_id: i32, owner: Option<i64>) -> Result<(), CallError> {
    let stats = match npc_stats(&db, world.sde(), type_id).await {
        Ok(stats) => Arc::new(stats),
        Err(error) => {
            world.post(system, move |s| { s.remove_ball(id); });
            return Err(error);
        }
    };
    let config = NpcConfig {
        scan_resolution: stats.scan_resolution,
        fly_range: stats.fly_range,
        aggro_range: stats.aggro_range,
        rate_of_fire_ms: stats.rate_of_fire_ms,
        repairs: stats.repairs,
    };
    let mut brain = NpcBrain::new(owner, config, {
        let weak_world = Arc::downgrade(&world);
        let effects: NpcEffects = Arc::new(move |action| -> NpcFuture {
            let db = db.clone();
            let weak_world = weak_world.clone();
            let stats = stats.clone();
            Box::pin(async move {
                let Some(world) = weak_world.upgrade() else { return };
                match action {
                    NpcAction::Shot { me, victim, generation } => {
                        if let Err(e) = npc_shot(&db, &world, system, id, type_id, &stats, me, victim, generation).await {
                            warn!("npc {id}: {}", match e {
                                CallError::Internal(e) => format!("{e:#}"),
                                CallError::User { msg, .. } => msg,
                            });
                        }
                    }
                    NpcAction::Effect { me, victim, effect_id, source_module, duration_ms, generation } => {
                        if let Some(effect) = super::effects::registry::resolve(world.sde(), effect_id) {
                            let source = me.id;
                            let target = victim.id;
                            let Some(Some(projections)) = world.with(system, move |s|
                                s.npc_projection_valid(source, target, generation).then(|| s.projections_on(source))).await else { return };
                            let fit = Fit::for_space_type(world.sde().clone(), me.type_id, projections);
                            let cx = super::effects::space_source::SourceContext {
                                db: &db, world: &world, system, source: &me, target: &victim,
                                module: source_module, effect: &effect, duration_ms, fit: &fit, key: Key::Ship,
                                guard: super::effects::space_source::SourceGuard::Npc { source, target, generation },
                                attacker: None, drone: None, phase: crate::space::drone::DronePhase::Begin,
                            };
                            let result = super::effects::space_source::apply(&cx).await;
                            match result {
                                Err(CallError::Internal(error)) => warn!("npc {id} effect {effect_id}: {error:#}"),
                                Err(CallError::User { msg, .. }) if !matches!(effect.kind, super::effects::EffectKind::Projected { .. }) =>
                                    warn!("npc {id} effect {effect_id}: {msg}"),
                                _ => {}
                            }
                        }
                    }
                    NpcAction::Repair => npc_repair(&world, system, id, &stats),
                }
            })
        });
        effects
    });
    brain.max_target_range = world.sde().types.attribute(type_id, ATTR_MAX_TARGET_RANGE)
        .or_else(|| world.sde().dogma.attribute(ATTR_MAX_TARGET_RANGE).and_then(|a| a.default_value).filter(|value| *value > 0.0));
    for entry in world.sde().types.effects(type_id) {
        let Some(effect) = super::effects::registry::resolve(world.sde(), entry.effect_id) else { continue };
        if !matches!(effect.kind, super::effects::EffectKind::Projected { .. } | super::effects::EffectKind::Neutralizer | super::effects::EffectKind::Nosferatu | super::effects::EffectKind::CapTransfer | super::effects::EffectKind::RemoteRepair { .. } | super::effects::EffectKind::Ecm | super::effects::EffectKind::Smartbomb) { continue; }
        if matches!(effect.kind, super::effects::EffectKind::RemoteRepair { .. }) && !super::effects::npc_repair::supported_for_type(world.sde(), type_id, &effect) {
            let name = world.sde().dogma.effect(entry.effect_id).and_then(|row| row.name.as_deref()).unwrap_or("unknown");
            let _ = super::effects::unsupported::refusal(Some(entry.effect_id), name, type_id);
            continue;
        }
        let duration = if matches!(effect.kind, super::effects::EffectKind::Ecm) {
            let Some(duration) = super::effects::ecm::npc_cycle_duration(world.sde(), type_id, &effect) else { continue };
            duration
        } else {
            match effect.attribute(super::effects::registry::EffectAttribute::Duration) {
                Some(id) => match world.sde().types.attribute(type_id, id) {
                    Some(duration) => duration,
                    None => continue,
                },
                None => -1.0,
            }
        };
        brain.add_effect(entry.effect_id, duration);
    }
    world.post(system, move |s| {
        s.add_npc(id, brain);
    });
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match call.method {
        "AddTarget" => add_target(c, call.args.first().and_then(|v| v.as_int()).unwrap_or(0)).await,
        "CancelAddTarget" => {
            if let (Some(ch), Some(t)) = (c.session.int("charid"), call.args.first().and_then(|v| v.as_int())) {
                state().locking.remove(&(ch, t));
            }
            Ok(Value::None)
        }
        "RemoveTarget" => {
            let t = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let identity = Identity::of_client(c);
            let world = c.world.clone();
            let system = c.session.int("solarsystemid");
            drop_targets(&identity, &world, system, &[t], |event, args| c.notify(event, args));
            Ok(Value::None)
        }
        "RemoveTargets" => {
            let identity = Identity::of_client(c);
            let world = c.world.clone();
            let system = c.session.int("solarsystemid");
            drop_targets(&identity, &world, system, &value_id_list(call.args.first()), |event, args| c.notify(event, args));
            Ok(Value::None)
        }
        "ClearTargets" => {
            if let Some(ch) = c.session.int("charid") {
                let targets = clear_character_locks(&c.world, ch);
                remove_target_projections(&Identity::of_client(c), &c.world, c.session.int("solarsystemid"), targets);
            }
            c.notify("OnTarget", vec![Value::str("clear")]);
            Ok(Value::None)
        }
        "GetTargets" => Ok(Value::list(c.session.int("charid").map(targets_of).unwrap_or_default().into_iter().map(Value::Int))),
        "GetTargeters" => Ok(Value::list([])),
        "LoadAmmo" => load_ammo(c, call).await,
        "UnloadAmmo" => unload_ammo(c, call).await,
        "LinkWeapons" | "LinkAllWeapons" | "UnlinkModule" | "UnlinkAllModules" | "MergeModuleGroups" | "PeelAndLink" | "DestroyWeaponBank" | "GetWeaponBankInfoForShip" => {
            weapon_groups(c, call).await
        }
        "Activate" | "Deactivate" => {
            let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let effect_name = call.args.get(1).and_then(|v| v.as_text()).map(|s| s.into_owned()).unwrap_or_default();
            let target = call.args.get(2).and_then(|v| v.as_int());
            let repeat = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(0);
            let identity = Identity::of_client(c);
            let db = c.db.clone();
            let world = c.world.clone();
            let sde = c.sde.clone();
            let space_system = c.session.int("solarsystemid");
            let sender = c.notify_tx.clone();
            let login_user = c.login.user_id;
            let notify = |notification| match notification {
                notification @ crate::db::outbox::Notification::DirectClient { .. } => notification.dispatch(&world, Some(&mut *c)),
                notification => super::effects::online::dispatch_notification(notification, &c.world, &c.notify_tx, c.session.int("charid")),
            };
            if call.method == "Activate" {
                lifecycle::activate(&identity, &db, &world, &sde, space_system, sender, login_user, item_id, &effect_name, target, repeat, notify).await
            } else {
                lifecycle::deactivate(&identity, &db, &world, &sde, space_system, sender, login_user, item_id, &effect_name, notify).await
            }
        }
        _ => return None,
    })
}

#[cfg(test)] mod tests;

