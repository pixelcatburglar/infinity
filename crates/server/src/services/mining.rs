//! Mining: asteroid belts, ore/ice/gas sites' fields, mining lasers, strip miners, ice and
//! gas harvesters, the ship mining scanner and reprocessing at stations.
//!
//! Every asteroid belt of a system gets a field of rocks the first time a pilot enters
//! the system: ores by the system's security band, sized in the thousands of m3 each.
//! Exploration's ore, ice and gas sites get fields of their own (services/exploration).
//! Rocks are stored in game.asteroids and shrink as they are mined; an emptied belt
//! grows back after the configured regrowth interval, scheduled on its system actor. Depletion
//! timestamps persist; timers are restored when the system's fields are loaded.
//!
//! A mining module runs through combat's activation loop (the `miningLaser` /
//! `miningClouds` effects): at the end of each cycle `extract` moves its yield
//! (miningAmount m3, with critical successes and residue waste) from the rock to the
//! mining hold, or the cargo hold of ships without one, and tells the client
//! (OnOreMined -> notifications.oreMinedNotificationAdapter, OnItemChange).
//! Hold occupancy follows client item-aware packaged and assembled volumes.
//! Extraction range uses ship and rock positions sampled at the same system-actor time.
//! Laser extraction revalidates the online fitted module under row locks. Per-cycle
//! rounding is randomized as in `packages/mining/__init__.py`, with no fraction carry.
//! Attribute 3154 is percentage points, compared as in `mining/__init__.py:54`.
//! Completed cycles use clamp_factor=1 and server efficiency=1; partial cycles are
//! not delivered. Critical and waste consumption retain server-policy caps, which the
//! client shared formula does not specify. An owned task completes committed depletion
//! updates even if the initiating cycle is cancelled.
//! Each delivered extraction appends a personal fact to game.character_mining_ledger in
//! the caller's transaction, before post-commit notices. The client refreshes its
//! personal ledger normally; corporate observers are unsupported.
//!
//! Reprocessing (the station's bound reprocessingSvc) turns items into their
//! canonical SDE type materials per portion: the configured station yield, raised by Reprocessing,
//! Reprocessing Efficiency and the item's own processing skill, less the configured ISK tax.
//! Output that exceeds a single stack's capacity is refused without consuming inputs.

mod in_space_compression_mgr;
mod ledger;
pub(crate) mod retention;
mod mining_scan_mgr;
mod reprocessing_svc;
pub(crate) mod rounding;
use super::fit::{Fit, Key};
use super::{db_err, keyval, notify_error, rand01, rand_dir, value_id_list, Call, CallError, CallResult};
use crate::client::{filetime_now, unix_ms_from_filetime_ceil, Client};
use crate::consts::{dogma::ATTR_CAPACITY, dogma::ATTR_MAX_RANGE, FILETIME_SECOND, FLAG_CARGO, FLAG_HANGAR, GROUP_ASTEROID_BELT, IX_FLAG, IX_LOCATION_ID, IX_STACK_SIZE};
use crate::db::Db;
use crate::messages;
use crate::space::{dist, now_ms, Ball, NotifyTx, World, MODE_RIGID, V3};
use marshal::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tracing::{info, warn};

const ATTR_MINING_AMOUNT: i32 = 77;
const ATTR_MINING_HOLD: i32 = 1556; // generalMiningHoldCapacity
const ATTR_WASTE_MULTIPLIER: i32 = 3153;
const ATTR_WASTE_PROBABILITY: i32 = 3154;
const ATTR_CRIT_CHANCE: i32 = 5967;
const ATTR_CRIT_BONUS: i32 = 5969;
const ATTR_PROCESSING_SKILL: i32 = 790; // reprocessingSkillType
const FLAG_MINING_HOLD: i32 = 134; // flagGeneralMiningHold
const SKILL_REPROCESSING: i32 = 3385;
const SKILL_REPROCESSING_EFFICIENCY: i32 = 3389;
/// SDE invCategories: asteroid resources.
const CATEGORY_ASTEROID: i32 = 25;
const SKILL_SCRAPMETAL: i32 = 12196;
/// Inclusive waste check maximum from `mining/__init__.py:54`.
const WASTE_CHECK_MAX: u32 = 100;


// ---------------------------------------------------------------- fields

/// What a field is made of.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Field {
    Ore,
    Ice,
    Gas,
}

#[derive(Clone)]
struct Rock {
    system: i64,
    field: i64,
    type_id: i32,
    quantity: i64,
}

#[derive(Default)]
struct State {
    rocks: HashMap<i64, Rock>,
    loaded: HashSet<i64>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

/// Groups (by name) a field draws from: ores by security band, ice by security, gas by
/// space (Fullerite in wormhole space, the boosters' gases elsewhere).
fn field_groups(field: Field, security: f64, jspace: bool) -> (&'static [&'static str], bool) {
    // (names, are they group names (ore) or type names (ice, gas))
    match field {
        Field::Ore if jspace || security <= 0.0 => (&["Gneiss", "Dark Ochre", "Spodumain", "Crokite", "Bistot", "Arkonor", "Hemorphite", "Hedbergite"], true),
        Field::Ore if security < 0.45 => (&["Kernite", "Omber", "Jaspet", "Hemorphite", "Hedbergite", "Plagioclase"], true),
        Field::Ore if security < 0.7 => (&["Veldspar", "Scordite", "Pyroxeres", "Plagioclase", "Omber", "Kernite"], true),
        Field::Ore => (&["Veldspar", "Scordite", "Pyroxeres"], true),
        Field::Ice if security >= 0.45 => (&["Clear Icicle", "Glacial Mass", "Blue Ice", "White Glaze"], false),
        Field::Ice if security > 0.0 => (&["Glare Crust", "Dark Glitter", "Gelidus", "Blue Ice", "Clear Icicle"], false),
        Field::Ice => (&["Krystallos", "Glare Crust", "Dark Glitter", "Gelidus"], false),
        Field::Gas if jspace => (&["Fullerite-C50", "Fullerite-C60", "Fullerite-C70", "Fullerite-C72", "Fullerite-C84", "Fullerite-C28", "Fullerite-C32", "Fullerite-C320", "Fullerite-C540"], false),
        Field::Gas if security >= 0.45 => (&["Amber Mykoserocin", "Golden Mykoserocin", "Viridian Mykoserocin", "Celadon Mykoserocin", "Lime Mykoserocin"], false),
        Field::Gas => (&["Amber Cytoserocin", "Golden Cytoserocin", "Viridian Cytoserocin", "Celadon Cytoserocin", "Lime Cytoserocin", "Vermillion Cytoserocin", "Azure Cytoserocin"], false),
    }
}
fn ore_candidate(name: &str, group: &str, category: Option<i32>, names: &[&str]) -> bool {
    category == Some(CATEGORY_ASTEROID) && names.contains(&group)
        && (name == group || name.strip_prefix(group).is_some_and(|suffix| suffix == " II-Grade" || suffix == " III-Grade"))
}


/// (typeID, volume) candidates of a field: ores with their +5% / +10% grades.
fn field_types(sde: &crate::sde::Sde, field: Field, security: f64, jspace: bool) -> Vec<(i32, f64)> {
    let (names, groups) = field_groups(field, security, jspace);
    sde.types.iter().filter(|t| {
        if t.published != Some(true) { return false; }
        let Some(name) = t.name.as_deref() else { return false };
        if !groups { return names.contains(&name); }
        let Some(group) = t.group_id.and_then(|id| sde.types.group(id)) else { return false };
        let Some(group_name) = group.name.as_deref() else { return false };
        ore_candidate(name, group_name, group.category_id, names)
    }).map(|t| (t.type_id, t.volume.unwrap_or(1.0).max(0.01))).collect()
}

fn rock_ball(id: i64, type_id: i32, pos: V3, radius: f32) -> Ball {
    Ball {
        id,
        type_id,
        owner_id: 1,
        corp_id: 1,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: MODE_RIGID,
        radius,
        mass: 1e34,
        max_vel: 0.0,
        agility: 1.0,
        speed_fraction: 0.0,
        pos,
        vel: [0.0; 3],
        old_pos: pos, old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: false,
        global: false,
        massive: true,
        interactive: false,
        warp: crate::space::WarpState::default(),
        warp_factor: 0,
        last_ms: now_ms(),
        interpolation_anchor_ms: 0,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim: vec![],
    }
}

/// A rock's size from its volume (m3): a few km for ore, more for ice and gas clouds.
fn rock_radius(field: Field, volume_m3: f64) -> f32 {
    let r = match field {
        Field::Ore => volume_m3.cbrt() * 150.0,
        Field::Ice => 6_000.0 + volume_m3.cbrt() * 30.0,
        Field::Gas => 12_000.0 + rand01() * 10_000.0,
    };
    r.clamp(800.0, 25_000.0) as f32
}

/// Sample the existing half-open generation range; equal endpoints remain fixed.
fn sample_range(range: [f64; 2], random: f64) -> f64 {
    range[0] + random * (range[1] - range[0])
}

/// Grow a field of rocks around `center` and put it in space. Returns the rock IDs.
pub async fn grow_field(db: &Db, world: &World, system: i64, field_id: i64, center: V3, field: Field, security: f64, jspace: bool) -> Result<Vec<i64>, CallError> {
    let types = field_types(world.sde(), field, security, jspace);
    if types.is_empty() {
        return Ok(vec![]);
    }
    let policy = &crate::config::game().mining;
    let count_range = match field {
        Field::Ore => policy.ore_rocks,
        Field::Ice => policy.ice_rocks,
        Field::Gas => policy.gas_clouds,
    };
    let count = count_range[0] + (rand01() * (count_range[1] - count_range[0]) as f64) as i64;
    let mut ids = vec![];
    for _ in 0..count {
        let (type_id, unit_volume) = types[(rand01() * types.len() as f64) as usize % types.len()];
        let volume = match field {
            Field::Ore => sample_range(policy.ore_rock_m3, rand01()),
            Field::Ice => sample_range([policy.ice_rock_units[0] as f64, policy.ice_rock_units[1] as f64], rand01()) * unit_volume,
            Field::Gas => sample_range(policy.gas_cloud_m3, rand01()),
        };
        let quantity = (volume / unit_volume).round().max(1.0) as i64;
        let radius = rock_radius(field, volume);
        let spread = if field == Field::Gas { 60_000.0 } else { 40_000.0 };
        let v = rand_dir();
        let d = 3_000.0 + rand01() * spread;
        let pos = [center[0] + v[0] * d, center[1] + v[1] * d * 0.3, center[2] + v[2] * d];
        let id = crate::dao::asteroids::insert(db, system, field_id, type_id, quantity, radius, pos).await.map_err(db_err)?;
        state().rocks.insert(id, Rock { system, field: field_id, type_id, quantity });
        world.add_ship(system, rock_ball(id, type_id, pos, radius)).await.map_err(CallError::Internal)?;
        ids.push(id);
    }
    Ok(ids)
}

/// (security, wormhole space) of a system.
fn system_band(sde: &crate::sde::Sde, system: i64) -> Result<(f64, bool), CallError> {
    let row = sde.map.system(system as i32 as i64).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing solar system {system}")))?;
    Ok((row.security.unwrap_or(0.0), (31_000_000..32_000_000).contains(&system)))
}

/// Bring a system's rocks into space (once per run): the stored ones, and fields for
/// belts that have never been grown or have grown back.
pub async fn ensure_fields(db: &Db, world: &Arc<World>, system: i64) -> Result<(), CallError> {
    if !state().loaded.insert(system) {
        return Ok(());
    }
    let rows = crate::dao::asteroids::load(db, system).await.map_err(db_err)?;
    let mut fields_with_rocks = HashSet::new();
    for r in &rows {
        let (id, field) = (r.id, r.field);
        fields_with_rocks.insert(field);
        state().rocks.insert(id, Rock { system, field, type_id: r.type_id, quantity: r.quantity });
        let ball = rock_ball(id, r.type_id, [r.x, r.y, r.z], r.radius.unwrap_or(2_000.0));
        world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    }
    let belts: Vec<_> = world.sde().map.celestials(system as i32 as i64).filter(|b| b.group_id == Some(GROUP_ASTEROID_BELT as i64)).collect();
    if belts.is_empty() {
        return Ok(());
    }
    let (security, jspace) = system_band(world.sde(), system)?;
    let now = filetime_now();
    for b in belts {
        let belt = b.item_id;
        if fields_with_rocks.contains(&belt) {
            continue;
        }
        let depleted = crate::dao::asteroids::depleted(db, belt).await.map_err(db_err)?;
        let regrow_at = depleted.flatten().map(|t| t.saturating_add(crate::config::game().mining.belt_regrowth_secs.saturating_mul(FILETIME_SECOND)));
        if regrow_at.is_some_and(|t| t > now) {
            world.system(system).await.map_err(CallError::Internal)?;
            schedule_regrow(db.clone(), world.clone(), system, belt, [b.x.expect("belt x"), b.y.expect("belt y"), b.z.expect("belt z")], regrow_at.unwrap_or(now));
            continue;
        }
        grow_belt(db, world, system, belt, [b.x.expect("belt x"), b.y.expect("belt y"), b.z.expect("belt z")], security, jspace).await?;
    }
    Ok(())
}

async fn grow_belt(db: &Db, world: &World, system: i64, belt: i64, center: V3, security: f64, jspace: bool) -> Result<(), CallError> {
    let ids = grow_field(db, world, system, belt, center, Field::Ore, security, jspace).await?;
    crate::dao::asteroids::grown(db, belt, system, filetime_now()).await.map_err(db_err)?;
    info!("mining: belt {belt} in {system} grew {} rocks", ids.len());
    Ok(())
}

fn schedule_regrow(db: Db, world: Arc<World>, system: i64, belt: i64, center: V3, at: i64) {
    let deadline_ms = unix_ms_from_filetime_ceil(at);
    let world_ref = Arc::downgrade(&world);
    world.post(system, move |s| {
        s.schedule_at(deadline_ms, move |s| {
            s.defer(async move {
                let Some(world) = world_ref.upgrade() else { return };
                let result = async {
                    let (security, jspace) = system_band(world.sde(), system)?;
                    grow_belt(&db, &world, system, belt, center, security, jspace).await
                };
                if let Err(e) = result.await {
                    warn!("mining: regrowing belt {belt}: {}", match e {
                        CallError::Internal(e) => format!("{e:#}"),
                        CallError::User { msg, .. } => msg,
                    });
                }
            });
        });
    });
}

/// A rock is gone: out of space and the database; an emptied belt starts growing back.
pub(crate) async fn reconcile_depleted(db: &Db, world: &Arc<World>, id: i64) -> Result<(), CallError> {
    let Some(rock) = state().rocks.remove(&id) else { return Ok(()) };
    world.remove_ball(rock.system, id).await;
    crate::dao::asteroids::delete(db, id).await.map_err(db_err)?;
    let field_empty = !state().rocks.values().any(|r| r.field == rock.field);
    if !field_empty {
        return Ok(());
    }
    let belt = world.sde().map.celestial(rock.field).filter(|b| b.group_id == Some(GROUP_ASTEROID_BELT as i64));
    if let Some(b) = belt {
        let now = filetime_now();
        crate::dao::asteroids::mark_depleted(db, rock.field, now).await.map_err(db_err)?;
        schedule_regrow(db.clone(), world.clone(), rock.system, rock.field, [b.x.expect("belt x"), b.y.expect("belt y"), b.z.expect("belt z")], now.saturating_add(crate::config::game().mining.belt_regrowth_secs.saturating_mul(FILETIME_SECOND)));
        info!("mining: belt {} in {} mined out", rock.field, rock.system);
    }
    Ok(())
}

/// Is this ball a rock that can be mined (and with what kind of module)?
pub fn is_rock(id: i64) -> bool {
    state().rocks.contains_key(&id)
}

// ---------------------------------------------------------------- extraction


fn extraction_amounts(units: i64, waste: i64, available: i64, room: i64) -> (i64, i64) {
    let taken = units.max(0).min(available.max(0)).min(room.max(0));
    let wasted = if taken == 0 { 0 } else { waste.max(0).min(available - taken) };
    (taken, wasted)
}

/// Capture fitted values before entering an actor turn.
pub(crate) fn yield_values(fit: &Fit, key: Key) -> [f64; 5] {
    [fit.value(key, ATTR_MINING_AMOUNT).max(0.0), fit.value(key, ATTR_CRIT_CHANCE),
     fit.value(key, ATTR_CRIT_BONUS), fit.value(key, ATTR_WASTE_PROBABILITY), fit.value(key, ATTR_WASTE_MULTIPLIER)]
}

/// A source's fitted yield is an intent until a transaction commits it.
pub(crate) fn yield_intent(values: [f64; 5], target: i64, type_id: i32, unit_volume: f64) -> crate::space::drone::PendingMining {
    let [volume, crit_probability, crit_quantity_multiplier, waste_probability, waste_volume_multiplier] = values;
    crate::space::drone::PendingMining {
        rock: target, type_id, unit_volume,
        inputs: rounding::MiningInputs { clamp_factor: 1.0, volume, waste_volume_multiplier, waste_probability, crit_quantity_multiplier, crit_probability, efficiency: 1.0 },
        draws: rounding::MiningDraws {
            waste_check: (rand01() * f64::from(WASTE_CHECK_MAX + 1)) as u32,
            normal_round: rand01(), waste_round: rand01(), crit_check: rand01(), crit_round: rand01(),
        },
    }
}

pub(crate) fn rock_type(target: i64, system: i64) -> Option<i32> {
    state().rocks.get(&target).filter(|rock| rock.system == system).map(|rock| rock.type_id)
}

pub(crate) struct Delivery {
    rock: i64,
    left: i64,
    removed: i64,
    full: bool,
}

impl Delivery {
    pub(crate) fn repeat(&self) -> bool { self.left > 0 && !self.full }
}

pub(crate) enum DeliveryOutcome {
    Missing,
    Depleted { rock: i64 },
    NoYield,
    Full { flag: i32 },
    Delivered(Delivery),
}

pub(crate) fn destination(fit: &Fit) -> (i32, f64) {
    let hold = fit.value(Key::Ship, ATTR_MINING_HOLD);
    if hold > 0.0 { (FLAG_MINING_HOLD, hold) } else { (FLAG_CARGO, fit.value(Key::Ship, ATTR_CAPACITY)) }
}

/// Join the caller's transaction; all notices remain in its post-commit outbox.
pub(crate) async fn deliver(ex: &(impl crate::db::executor::Executor + ?Sized), outbox: &mut crate::db::outbox::Outbox, sde: &crate::sde::Sde, owner: i64, ship: i64, system: i64, source: i64, pending: &crate::space::drone::PendingMining, (flag, capacity): (i32, f64)) -> Result<DeliveryOutcome, CallError> {
    let Some(available) = crate::dao::asteroids::lock(ex, system, pending.rock, pending.type_id).await.map_err(db_err)? else { return Ok(DeliveryOutcome::Missing) };
    if available <= 0 { return Ok(DeliveryOutcome::Depleted { rock: pending.rock }); }
    let result = rounding::mining_result(pending.inputs, pending.unit_volume, available, pending.draws)
        .map_err(|error| CallError::Internal(anyhow::anyhow!("{error}")))?;
    let units = result.normal_quantity.saturating_add(result.critical_hit_quantity);
    if units <= 0 { return Ok(DeliveryOutcome::NoYield); }
    let t = sde.types.get(pending.type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing type {}", pending.type_id)))?;
    let volume = t.volume.unwrap_or(1.0).max(0.01);
    let rows = crate::dao::items::loading::load_items(ex, crate::dao::items::loading::ItemQuery::LocationFlags(ship, &[flag])).await.map_err(db_err)?;
    let used = rows.iter().try_fold(0.0, |sum, item| Ok::<_, CallError>(sum + crate::services::item_volume::item_volume(sde, item.type_id, item.singleton, i64::from(item.quantity), None)?))?;
    let room = ((capacity - used) / volume).floor().max(0.0) as i64;
    let stack = crate::dao::items::transfers::stack(ex, ship, flag, pending.type_id, owner).await.map_err(db_err)?;
    let stack_room = stack.as_ref().map_or(i64::from(i32::MAX), |r| i64::from(i32::MAX - r.quantity));
    let (taken, wasted) = extraction_amounts(units, result.wasted_quantity, available, room.min(stack_room));
    if taken <= 0 { return Ok(DeliveryOutcome::Full { flag }); }
    let removed = taken + wasted;
    if !crate::dao::asteroids::take(ex, pending.rock, available, removed).await.map_err(db_err)? {
        return Err(CallError::Internal(anyhow::anyhow!("asteroid extraction guard failed")));
    }
    let quantity = taken as i32;
    let (item_id, total, change) = match stack {
        Some(r) => {
            if !crate::dao::items::transfers::add(ex, r.item_id, quantity).await.map_err(db_err)? {
                return Err(CallError::Internal(anyhow::anyhow!("mining stack transfer guard failed")));
            }
            (r.item_id, r.quantity + quantity, (IX_STACK_SIZE, Value::Int(r.quantity as i64)))
        }
        None => {
            let id = crate::dao::items::transfers::insert(ex, pending.type_id, owner, ship, flag, quantity).await.map_err(db_err)?;
            (id, quantity, (IX_LOCATION_ID, Value::Int(0)))
        }
    };
    crate::dao::mining_ledger::append(ex, owner, system, pending.type_id, taken, wasted, filetime_now()).await.map_err(db_err)?;
    let item = crate::dao::items::loading::Item {
        item_id, type_id: pending.type_id, owner_id: owner, location_id: ship, flag_id: flag,
        quantity: total, singleton: false, group_id: t.group_id.unwrap_or(0) as i64,
        category_id: t.category_id.unwrap_or(0) as i64, custom_info: None,
    };
    outbox.notify_pilot(owner, "OnItemChange", vec![item.row(), Value::Dict(vec![(Value::Int(change.0), change.1)]), Value::None]);
    outbox.notify_pilot(owner, "OnOreMined", vec![Value::dict([
        ("oreType", Value::Int(pending.type_id as i64)), ("quantity_added", Value::Int(taken)),
        ("quantity_removed", Value::Int(taken)), ("hasRewards", Value::Bool(false)),
        ("amountWasted", Value::Int(wasted)), ("amountCritBonus", Value::Int(result.critical_hit_quantity.max(0).min(taken))),
        ("itemID", Value::Int(pending.rock)), ("moduleItemID", Value::Int(source)),
    ])]);
    Ok(DeliveryOutcome::Delivered(Delivery { rock: pending.rock, left: available - removed, removed, full: taken < units.min(available) }))
}

/// Apply only committed results to the space cache and observers.
pub(crate) async fn delivered(db: &Db, world: &Arc<World>, system: i64, owner: i64, delivery: Delivery) -> Result<(), CallError> {
    if let Some(rock) = state().rocks.get_mut(&delivery.rock) { rock.quantity = rock.quantity.min(delivery.left); }
    let pilots = world.with(system, move |s| s.pilots().filter_map(|b| b.char_id.filter(|c| *c != owner)).collect::<Vec<_>>()).await.unwrap_or_default();
    for pilot in pilots {
        world.notify_pilot(pilot, "OnOreMinedByOther", vec![Value::Int(delivery.rock), Value::Int(delivery.removed)]);
    }
    if delivery.left == 0 { reconcile_depleted(db, world, delivery.rock).await?; }
    Ok(())
}

fn tell(notify: &NotifyTx, text: &str) {
    let _ = notify.send(("OnRemoteMessage".into(), vec![Value::str(messages::MSG_CUSTOM_NOTIFY), Value::dict([("notify", Value::unicode(text))])]));
}

fn full_notice(flag: i32) -> &'static str {
    if flag == FLAG_MINING_HOLD { messages::MINING_HOLD_FULL } else { messages::CARGO_HOLD_FULL }
}

/// The end of a mining cycle of `module` on rock `target`: move the yield to the hold.
#[allow(clippy::too_many_arguments)]
pub async fn extract(db: &Db, world: &Arc<World>, notify: &NotifyTx, char_id: i64, ship: i64, system: i64, fit: &Fit, module: i64, module_type: i32, module_flag: i32, target: i64) -> Result<bool, CallError> {
    let Some(type_id) = rock_type(target, system) else { return Ok(false) };
    let Some(gap) = world.with(system, move |s| {
        let (me, it) = (s.ball(ship)?, s.ball(target)?);
        Some(dist(s.sample_position(ship)?, s.sample_position(target)?) - me.radius as f64 - it.radius as f64)
    }).await.flatten() else { return Ok(false) };
    if gap > fit.value(Key::Module(module), ATTR_MAX_RANGE) {
        tell(notify, messages::TARGET_OUTSIDE_MINING_RANGE);
        return Ok(false);
    }
    let volume = world.sde().types.get(type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing type {type_id}")))?.volume.unwrap_or(1.0).max(0.01);
    let values = yield_values(fit, Key::Module(module));
    let destination = destination(fit);
    let db = db.clone();
    let world = world.clone();
    let notify = notify.clone();
    tokio::spawn(async move {
        let result: Result<bool, CallError> = async {
            let _lock = super::market::lock().await;
            let pending = yield_intent(values, target, type_id, volume);
            let sde = world.sde().clone();
            let result = db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| {
                    let pending = pending.clone();
                    let sde = sde.clone();
                    Box::pin(async move {
                        if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Ok(DeliveryOutcome::Missing); }
                        let hull = super::items::load_items(tx, &sde, crate::dao::items::loading::ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next();
                        if !hull.is_some_and(|h| h.owner_id == char_id && h.location_id == system && h.singleton && h.category_id == i64::from(crate::consts::CATEGORY_SHIP))
                            || !super::inventory::is_fitting_flag(module_flag)
                            || !crate::dao::items::mining::lock_module(tx, char_id, ship, module, module_type, module_flag).await.map_err(db_err)? {
                            return Ok(DeliveryOutcome::Missing);
                        }
                        deliver(tx, outbox, &sde, char_id, ship, system, module, &pending, destination).await
                    })
                },
                |value, outbox| {
                    let world = world.clone();
                    async move {
                        crate::db::outbox::dispatch_world(value, outbox, &world)
                    }
                },
            ).await?;
            match result {
                DeliveryOutcome::Missing => Ok(false),
                DeliveryOutcome::Depleted { rock } => { reconcile_depleted(&db, &world, rock).await?; Ok(false) }
                DeliveryOutcome::NoYield => Ok(true),
                DeliveryOutcome::Full { flag } => {
                    tell(&notify, full_notice(flag));
                    Ok(false)
                }
                DeliveryOutcome::Delivered(result) => {
                    let repeat = result.repeat();
                    if result.full { tell(&notify, full_notice(destination.0)); }
                    delivered(&db, &world, system, char_id, result).await?;
                    Ok(repeat)
                }
            }
        }.await;
        if let Err(error) = &result { warn!(module, ?error, "mining extraction failed"); }
        result
    }).await.map_err(|error| CallError::Internal(error.into()))?
}



pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "inSpaceCompressionMgr" => in_space_compression_mgr::dispatch(c, call).await,
        "characterMiningLedger" => ledger::dispatch(c, call).await,
        "miningScanMgr" => mining_scan_mgr::dispatch(c, call).await,
        "reprocessingSvc" => reprocessing_svc::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
