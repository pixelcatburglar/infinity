//! Ship health and memory-only capacitor state; routing lives in the parent module.
//! The client reads capacitor charge through clientDogmaLocation; player attributes come
//! from fitted dogma and NPC attributes from the SDE. Capacitor state is not persisted;
//! Hostile capacitor mutations validate their source and range in the system actor.
//! Confirmed NPC destruction emits a kill event for a known attacker before rewards are attempted.
//! NPC bounty net and corporation tax commit together; wallet notices and events follow commit.
//! NPC repairs use the damage cache in the system actor and are not persisted; missing
//! cache entries represent undamaged NPCs. Persistent unpiloted ships use the fitted
//! ship condition cache and commit damage and repairs before publishing health.
//! Positive pilot damage with a supplied source ball emits after actor health update or actual removal.
use super::*;
use crate::consts::CATEGORY_MODULE;


// ---------------------------------------------------------------- capacitor and hit points

/// Capacitor or shield after `dt` seconds of passive recharge from `level` (the game's
/// curve: recharge peaks at 25%, ~98% after one recharge time `tau_ms`).
pub(super) fn recharge(level: f64, max: f64, tau_ms: f64, dt: f64) -> f64 {
    if max <= 0.0 {
        return 0.0;
    }
    let tau = (tau_ms / 1000.0).max(0.001);
    let c = 1.0 + ((level / max).clamp(0.0, 1.0).sqrt() - 1.0) * (-5.0 * dt.max(0.0) / tau).exp();
    (max * c * c).clamp(0.0, max)
}


/// Use `amount` of the ship's capacitor; false (and nothing used) when there isn't enough.
pub(in crate::services) fn drain_capacitor(cx: &Ctx, fit: &Fit, amount: f64) -> bool {
    if !amount.is_finite() || amount <= 0.0 {
        return true;
    }
    let (max, tau) = (fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), fit.value(Key::Ship, ATTR_RECHARGE_RATE));
    let now = filetime_now();
    let current = {
        let mut st = state();
        let result = mutate(&mut st.capacitor, cx.ship_id, max, tau, now, Operation::RefuseDrain(amount));
        if result.amount < amount {
            return false;
        }
        result.before
    };
    let change = attribute_change(cx.char_id, Value::Int(cx.ship_id), ATTR_CHARGE, Value::Float(current - amount), Value::Float(current));
    cx.notify("OnModuleAttributeChanges", vec![Value::list([change])]);
    true
}

/// Current capacitor capacity and recharged level.
pub(crate) struct CapacitorSnapshot {
    pub capacity: f64,
    pub level: f64,
}

enum Operation {
    Snapshot,
    Inject(f64),
    Drain(f64),
    RefuseDrain(f64),
}

struct Mutation {
    before: f64,
    after: f64,
    amount: f64,
}

fn mutate(entries: &mut HashMap<i64, (f64, i64)>, ship: i64, capacity: f64, tau: f64, now: i64, op: Operation) -> Mutation {
    let capacity = if capacity.is_finite() { capacity.max(0.0) } else { 0.0 };
    let (level, at) = entries.get(&ship).copied().unwrap_or((capacity, now));
    let dt = now.saturating_sub(at).max(0) as f64 / FILETIME_SECOND as f64;
    let before = recharge(level, capacity, tau, dt);
    let positive = |amount: f64| if amount.is_finite() { amount.max(0.0) } else { 0.0 };
    let (after, amount) = match op {
        Operation::Snapshot => (before, 0.0),
        Operation::Inject(amount) => {
            let accepted = positive(amount).min(capacity - before);
            (before + accepted, accepted)
        }
        Operation::Drain(amount) => {
            let drained = positive(amount).min(before);
            (before - drained, drained)
        }
        Operation::RefuseDrain(amount) => {
            let amount = positive(amount);
            if amount > before {
                return Mutation { before, after: before, amount: 0.0 };
            }
            (before - amount, amount)
        }
    };
    entries.insert(ship, (after, now));
    Mutation { before, after, amount }
}
/// Read an NPC's recharged fraction synchronously from the shared combat state.
pub(crate) fn npc_capacitor_fraction(sde: &crate::sde::Sde, ball: &Ball) -> Option<f64> {
    let capacity = attr(sde, ball.type_id, ATTR_CAPACITOR_CAPACITY, 0.0);
    if !capacity.is_finite() || capacity <= 0.0 { return None; }
    let tau = attr(sde, ball.type_id, ATTR_RECHARGE_RATE, 0.0);
    let now = filetime_now();
    let (level, at) = state().capacitor.get(&ball.id).copied().unwrap_or((capacity, now));
    let dt = now.saturating_sub(at).max(0) as f64 / FILETIME_SECOND as f64;
    Some(recharge(level, capacity, tau, dt) / capacity)
}

#[derive(Clone)]
pub(crate) enum NpcCapacitorGuard {
    Assistance { source: i64, hostile: i64, target: i64, generation: u64, optimal: f64, falloff: f64 },
    DroneAssistance { source: i64, target: i64, generation: u64, optimal: f64, falloff: f64, owner: i64 },
    Attack { source: i64, target: i64, source_guard: Option<crate::services::effects::space_source::SourceGuard>, player: Option<(i64, i64, std::sync::Arc<std::sync::atomic::AtomicBool>)>, optimal: f64, falloff: f64, offensive: bool },
}

impl NpcCapacitorGuard {
    pub(crate) fn valid(&self, s: &crate::space::System) -> bool {
        match self.clone() {
            Self::Attack { source, target, source_guard, player, optimal, falloff, .. } => {
                if source_guard.is_some_and(|guard| !guard.valid(s)) { return false; }
                if let Some((character, module, generation)) = player {
                    if generation.load(std::sync::atomic::Ordering::Relaxed)
                        || !s.pilot_ball(character).is_some_and(|ball| ball.id == source)
                        || !crate::services::combat::targeting_charges::targets_of(character).contains(&target)
                        || !state().active.get(&module).is_some_and(|active| std::sync::Arc::ptr_eq(&active.stop, &generation)) { return false; }
                }
                s.sampled_ball(source).zip(s.sampled_ball(target)).is_some_and(|(a, b)| {
                    let distance = (crate::space::dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0);
                    a.mode != crate::space::MODE_WARP && b.mode != crate::space::MODE_WARP
                        && crate::services::effects::npc_capacitor::in_range(distance, optimal, falloff)
                })
            }
            Self::DroneAssistance { source, target, generation, optimal, falloff, owner } => {
                let guard = crate::services::effects::space_source::SourceGuard::Drone { source, target, generation };
                source != target && guard.valid(s) && s.drone(source).is_some_and(|drone| drone.owner == owner)
                    && s.sampled_ball(source).zip(s.sampled_ball(target)).is_some_and(|(me, other)| {
                        let player = other.char_id.is_some_and(|pilot| s.pilot_ball(pilot).is_some_and(|ball| ball.id == target))
                            && s.sde().and_then(|sde| sde.types.get(other.type_id))
                                .is_some_and(|ty| ty.category_id == Some(crate::consts::CATEGORY_SHIP));
                        let eligible = player || s.is_npc(target) || s.drone(target).is_some();
                        let distance = (crate::space::dist(me.pos, other.pos) - f64::from(me.radius) - f64::from(other.radius)).max(0.0);
                        eligible && me.mode != crate::space::MODE_WARP && other.mode != crate::space::MODE_WARP && other.cloak == 0
                            && crate::services::effects::npc_capacitor::in_range(distance, optimal, falloff)
                    })
            }
            Self::Assistance { source, hostile, target, generation, optimal, falloff } =>
                s.npc_projection_valid(source, hostile, generation) && s.npc_assist_eligible(source, target)
                && s.sampled_ball(source).zip(s.sampled_ball(target)).is_some_and(|(me, other)| {
                    let distance = (crate::space::dist(me.pos, other.pos) - f64::from(me.radius) - f64::from(other.radius)).max(0.0);
                    crate::services::effects::npc_capacitor::in_range(distance, optimal, falloff)
                }),
        }
    }
}


async fn capacitor_operation(db: &Db, world: &World, system: i64, ship: i64, op: Operation, guard: Option<NpcCapacitorGuard>) -> Result<Option<(CapacitorSnapshot, f64, Option<i64>)>, CallError> {
    let Some((type_id, pilot)) = world.with(system, move |s| s.ball(ship).map(|b| (b.type_id, b.char_id))).await.flatten() else {
        return Ok(None);
    };
    let (capacity, tau, revision) = if let Some(pilot) = pilot {
        let fit = Fit::load_in_space(db, world, system, pilot, ship).await?;
        (fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), fit.value(Key::Ship, ATTR_RECHARGE_RATE), fit.projection_revision())
    } else {
        (attr(world.sde(), type_id, ATTR_CAPACITOR_CAPACITY, 0.0), attr(world.sde(), type_id, ATTR_RECHARGE_RATE, 0.0), None)
    };
    let capacity = if capacity.is_finite() { capacity.max(0.0) } else { 0.0 };
    Ok(world.with(system, move |s| {
        let ball = s.ball(ship)?;
        if (ball.type_id, ball.char_id) != (type_id, pilot)
            || revision.is_some_and(|revision| s.projection_revision(ship) != revision)
            || guard.as_ref().is_some_and(|guard| !guard.valid(s))
        {
            return None;
        }
        let result = {
            let mut st = state();
            mutate(&mut st.capacitor, ship, capacity, tau, filetime_now(), op)
        };
        if let Some(pilot) = pilot.filter(|_| result.amount > 0.0) {
            let change = attribute_change(pilot, Value::Int(ship), ATTR_CHARGE, Value::Float(result.after), Value::Float(result.before));
            s.notify_pilot(pilot, "OnModuleAttributeChanges", vec![Value::list([change])]);
        }
        if let Some(NpcCapacitorGuard::Attack { source, target, offensive: true, .. }) = &guard {
            if ship == *target { s.drone_attack(*source, *target); }
        }
        Some((CapacitorSnapshot { capacity, level: result.after }, result.amount, s.combat_owner(ship)))
    }).await.flatten())
}

/// Return the current capacitor for a ship present in the system.
pub(crate) async fn capacitor_snapshot(db: &Db, world: &World, system: i64, ship: i64) -> Result<Option<CapacitorSnapshot>, CallError> {
    Ok(capacitor_operation(db, world, system, ship, Operation::Snapshot, None).await?.map(|(snapshot, _, _)| snapshot))
}

/// Inject energy, returning the amount accepted.
pub(crate) async fn inject(db: &Db, world: &World, system: i64, ship: i64, amount: f64) -> Result<f64, CallError> {
    Ok(capacitor_operation(db, world, system, ship, Operation::Inject(amount), None).await?.map_or(0.0, |(_, amount, _)| amount))
}

/// Inject energy only while the NPC projection remains valid.
pub(crate) async fn inject_guarded(db: &Db, world: &World, system: i64, ship: i64, amount: f64, guard: NpcCapacitorGuard) -> Result<f64, CallError> {
    Ok(capacitor_operation(db, world, system, ship, Operation::Inject(amount), Some(guard)).await?.map_or(0.0, |(_, amount, _)| amount))
}

/// Drain only while the guarded attack remains valid. `None` means refused;
/// an accepted drain can have a zero amount.
pub(crate) async fn drain_exact_guarded(db: &Db, world: &World, system: i64, ship: i64, amount: f64, guard: NpcCapacitorGuard) -> Result<Option<(f64, Option<i64>)>, CallError> {
    Ok(capacitor_operation(db, world, system, ship, Operation::Drain(amount), Some(guard)).await?.map(|(_, amount, owner)| (amount, owner)))
}

/// A ship's hit points and resistances, from its fit.
#[derive(Clone)]
pub(super) struct ShipStats {
    pub(super) max: [f64; 3],
    pub(super) resonance: [[f64; 4]; 3],
    pub(super) shield_tau_ms: f64,
}

impl ShipStats {
    pub(super) fn from_fit(fit: &Fit, key: Key) -> Self {
        Self {
            max: ATTR_HP.map(|a| fit.value(key, a)),
            resonance: ATTR_RESONANCE.map(|layer| layer.map(|a| fit.value(key, a))),
            shield_tau_ms: fit.value(key, ATTR_SHIELD_RECHARGE_RATE),
        }
    }
}

/// A player ship's condition: shield (recharging since `shield_at`), armor and structure left.
#[derive(Clone)]
pub(super) struct Condition {
    pub(super) stats: ShipStats,
    pub(super) now: [f64; 3],
    pub(super) shield_at: i64,
}

pub(super) async fn ship_stats(db: &Db, world: &World, system: i64, char_id: i64, ship: i64) -> Result<ShipStats, CallError> {
    let fit = Fit::load_in_space(db, world, system, char_id, ship).await?;
    Ok(ShipStats::from_fit(&fit, Key::Ship))
}

/// (shieldCharge, armorDamage, damage) of a ship that has taken damage since it was last
/// repaired, with its shield recharged to now; None for an undamaged ship.


/// Docking repairs the ship.


/// A ship being (re)loaded by its pilot (undocking, boarding) starts with a full capacitor.


/// Forget a destroyed ship.


/// Take raw damage (em, thermal, kinetic, explosive) through layers with resonances;
/// returns the damage dealt.
pub(super) fn absorb(now: &mut [f64; 3], resonance: &[[f64; 4]; 3], raw: [f64; 4]) -> f64 {
    let mut left = raw;
    let mut dealt = 0.0;
    for layer in 0..3 {
        let effective: f64 = (0..4).map(|i| left[i] * resonance[layer][i]).sum();
        if effective <= 0.0 {
            break;
        }
        if effective <= now[layer] {
            now[layer] -= effective;
            dealt += effective;
            break;
        }
        // This layer breaks; what it didn't absorb carries on to the next.
        let used = now[layer] / effective;
        dealt += now[layer];
        now[layer] = 0.0;
        left = left.map(|d| d * (1.0 - used));
    }
    dealt
}

fn damage_npc_cached(target: i64, initial: impl FnOnce() -> Hp, raw: [f64; 4]) -> (f64, bool, [f64; 3]) {
    let mut st = state();
    let hp = st.hp.entry(target).or_insert_with(initial);
    let dealt = absorb(&mut hp.now, &hp.resonance, raw);
    let destroyed = hp.now[2] <= 0.0 && (hp.max[2] > 0.0 || hp.now.iter().all(|v| *v <= 0.0));
    let health = [0, 1, 2].map(|i| if hp.max[i] > 0.0 { (hp.now[i] / hp.max[i]).clamp(0.0, 1.0) } else { 0.0 });
    (dealt, destroyed, health)
}

fn restore_drone_hp(target: i64, previous: Option<Hp>) {
    let mut st = state();
    match previous {
        Some(hp) => { st.hp.insert(target, hp); }
        None => { st.hp.remove(&target); }
    }
}

enum NpcDamage {
    Drone { owner: i64, generation: u64, type_id: i32 },
    Applied { dealt: f64, destroyed: Option<Ball>, npc: bool },
}

fn apply_npc_damage(s: &mut crate::space::System, sde: &crate::sde::Sde, target: i64, raw: [f64; 4], attacker: Option<i64>) -> Option<NpcDamage> {
    let ball = s.ball(target)?;
    if ball.char_id.is_some() || (s.persistent_generation(target).is_some()
        && sde.types.get(ball.type_id).and_then(|t| t.category_id) == Some(crate::consts::CATEGORY_SHIP)) {
        return None;
    }
    let type_id = ball.type_id;
    if let Some(drone) = s.drone(target) {
        return Some(NpcDamage::Drone { owner: drone.owner, generation: drone.generation, type_id });
    }
    let npc = s.is_npc(target);
    if let Some(pilot) = attacker { s.add_aggressor(target, pilot); }
    let (dealt, destroyed, health) = damage_npc_cached(target, || {
        let max = ATTR_HP.map(|id| attr(sde, type_id, id, 0.0));
        Hp { now: max, max, resonance: ATTR_RESONANCE.map(|layer| layer.map(|id| attr(sde, type_id, id, 1.0))) }
    }, raw);
    let destroyed = if destroyed {
        let ball = s.destroy_ball(target);
        forget_ship(target);
        ball
    } else {
        s.set_health(target, health);
        None
    };
    Some(NpcDamage::Applied { dealt, destroyed, npc })
}

/// Damage an NPC (or any non-player ball); destroys it at zero structure.
pub(super) async fn damage_npc(db: &Db, world: &World, system: i64, target: i64, raw: [f64; 4], attacker: Option<i64>) -> Result<f64, CallError> {
    let sde = world.sde().clone();
    let Some(result) = world.with(system, move |s| apply_npc_damage(s, &sde, target, raw, attacker)).await.flatten() else { return Ok(0.0) };
    let (owner, generation, type_id) = match result {
        NpcDamage::Applied { dealt, destroyed, npc } => {
            if let Some(ball) = destroyed {
                forget_target(world, target);
                if npc { finish_npc_kill(db, world, system, target, ball, attacker).await?; }
            }
            return Ok(dealt);
        }
        NpcDamage::Drone { owner, generation, type_id } => (owner, generation, type_id),
    };
    let _lock = super::super::market::lock().await;
    super::drone_health::refresh_locked(db, world, system, target, owner).await?;
    if !world.with(system, move |s| s.drone_current(target, generation) && s.drone(target).is_some_and(|d| d.owner == owner)).await.unwrap_or(false) {
        return Ok(0.0);
    }
    let previous = state().hp.get(&target).cloned();
    if let Some(pilot) = attacker {
        world.post(system, move |s| {
            s.add_aggressor(target, pilot);
        });
    }
    let (dealt, destroyed, health) = damage_npc_cached(target, || {
        let max = ATTR_HP.map(|id| attr(world.sde(), type_id, id, 0.0));
        Hp { now: max, max, resonance: ATTR_RESONANCE.map(|layer| layer.map(|id| attr(world.sde(), type_id, id, 1.0))) }
    }, raw);
    if destroyed {
        let result = db.transaction(
            |tx, _: &mut Outbox| Box::pin(async move {
                crate::dao::items::drones::destroy(tx, target, owner, system).await.map_err(db_err)
            }),
            |value, _| async move { Ok(value) },
        ).await;
        match result {
            Ok(true) => {}
            Ok(false) => {
                forget_ship(target);
                return Ok(0.0);
            }
            Err(error) => {
                restore_drone_hp(target, previous);
                return Err(error);
            }
        }
        let destroyed = world.with(system, move |s| {
            if s.drone_current(target, generation) && s.drone(target).is_some_and(|d| d.owner == owner)
                && s.ball(target).is_some_and(|b| b.char_id.is_none()) {
                s.destroy_ball(target)
            } else { None }
        }).await.flatten();
        if destroyed.is_some() {
            forget_ship(target);
            forget_target(world, target);
        }
    } else {
        if let Err(error) = super::drone_health::save_condition_in(db, target).await {
            restore_drone_hp(target, previous);
            return Err(error);
        }
        world.set_health(system, target, health);
    }
    Ok(dealt)
}

async fn finish_npc_kill(db: &Db, world: &World, system: i64, target: i64, ball: Ball, attacker: Option<i64>) -> Result<(), CallError> {
    info!("ball {target} (type {}) destroyed", ball.type_id);
    super::super::map::events::npc_lost(db, system, filetime_now()).await;
    if let Some(char_id) = attacker {
        let mut outbox = Outbox::default();
        outbox.event(crate::lua::events::GameEvent::NpcKilled { char_id, npc_id: target, type_id: ball.type_id, system_id: system });
        dispatch_world((), outbox, world)?;
    }
    if let Err(e) = reward_kill(db, world, system, &ball, attacker).await {
        warn!("wreck of {target}: {}", match e {
            CallError::Internal(e) => format!("{e:#}"),
            CallError::User { msg, .. } => msg,
        });
    }
    Ok(())
}

/// Damage a player's ship; at zero structure it explodes and its pilot is told to eject
/// (`__ship_destroyed`, handled by the pilot's connection).
pub(super) async fn damage_player(db: &Db, world: &World, system: i64, ship: i64, char_id: i64, raw: [f64; 4], attacker: Option<i64>, attacker_ball_id: Option<i64>) -> Result<f64, CallError> {
    let _lock = super::super::market::lock().await;
    if world.ball(system, ship).await.is_none_or(|ball| ball.char_id != Some(char_id)) {
        return Ok(0.0);
    }
    // Resistances and hit points as fitted right now (hardeners running, modules offline...).
    let stats = ship_stats(db, world, system, char_id, ship).await?;
    let known = state().condition.get(&ship).cloned();
    let mut cond = match known {
        Some(mut c) => {
            c.now = [0, 1, 2].map(|i| c.now[i].min(stats.max[i]));
            c.stats = stats;
            c
        }
        None => Condition { now: stats.max, stats, shield_at: filetime_now() },
    };
    let now = filetime_now();
    let old = cond.now;
    cond.now[0] = recharge(cond.now[0], cond.stats.max[0], cond.stats.shield_tau_ms, (now - cond.shield_at) as f64 / FILETIME_SECOND as f64);
    cond.shield_at = now;
    let dealt = absorb(&mut cond.now, &cond.stats.resonance, raw);
    if let Some(attacker) = attacker {
        super::super::crimewatch::note_damage(ship, attacker, dealt);
    }
    let max = cond.stats.max;
    let changes = vec![
        attribute_change(char_id, Value::Int(ship), ATTR_SHIELD_CHARGE, Value::Float(cond.now[0]), Value::Float(old[0])),
        attribute_change(char_id, Value::Int(ship), ATTR_ARMOR_DAMAGE, Value::Float(max[1] - cond.now[1]), Value::Float(max[1] - old[1])),
        attribute_change(char_id, Value::Int(ship), ATTR_HULL_DAMAGE, Value::Float(max[2] - cond.now[2]), Value::Float(max[2] - old[2])),
    ];
    world.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
    if cond.now[2] <= 0.0 {
        info!("ship {ship} of {char_id} destroyed");
        let removed = destroy_player_ship(world, system, ship, char_id).await;
        if let Some(attacker_ball_id) = attacker_ball_id.filter(|_| removed && dealt > 0.0) {
            crate::lua::on_event(crate::lua::events::GameEvent::Damaged { char_id, ship_id: ship, attacker_ball_id, system_id: system });
        }
    } else {
        let health = [0, 1, 2].map(|i| if max[i] > 0.0 { (cond.now[i] / max[i]).clamp(0.0, 1.0) } else { 0.0 });
        state().condition.insert(ship, cond);
        world.post(system, move |s| {
            s.set_health(ship, health);
            if s.ball(ship).is_some_and(|ball| ball.char_id == Some(char_id)) {
                if let Some(attacker_ball_id) = attacker_ball_id.filter(|_| dealt > 0.0) {
                    crate::lua::on_event(crate::lua::events::GameEvent::Damaged { char_id, ship_id: ship, attacker_ball_id, system_id: system });
                }
            }
        });
    }
    Ok(dealt)
}

/// A player's ship explodes: its pilot is told to eject (`__ship_destroyed`, handled by
/// the pilot's connection). Also how CONCORD ends a criminal.


/// Damage whatever `target` is: a player's ship or an NPC. `attacker` is the pilot
/// responsible, if any (NPCs remember them; kills pay them).
pub(crate) async fn damage(db: &Db, world: &World, system: i64, target: i64, raw: [f64; DAMAGE_TYPE_COUNT], attacker: Option<i64>, attacker_ball_id: Option<i64>) -> Result<f64, CallError> {
    let Some(char_id) = world.with(system, move |s| s.ball(target).map(|ball| ball.char_id)).await.flatten() else { return Ok(0.0) };
    match char_id {
        Some(char_id) => damage_player(db, world, system, target, char_id, raw, attacker, attacker_ball_id).await,
        None => match super::unpiloted::damage(db, world, system, target, raw).await? {
            Some(dealt) => Ok(dealt),
            None => damage_npc(db, world, system, target, raw, attacker).await,
        },
    }
}
/// NPC layer health for assistance selection; an uncached NPC is undamaged.
pub(crate) fn npc_health_fraction(ball: &Ball, layer: usize) -> Option<f64> {
    if layer >= 3 { return None; }
    let st = state();
    let (now, max) = if let Some(cond) = st.condition.get(&ball.id) {
        (cond.now[layer], cond.stats.max[layer])
    } else if let Some(hp) = st.hp.get(&ball.id) {
        (hp.now[layer], hp.max[layer])
    } else { return Some(1.0) };
    if !now.is_finite() || !max.is_finite() || max <= 0.0 { return None; }
    Some((now / max).clamp(0.0, 1.0))
}

/// Apply a repair only to a live, valid cached NPC health record.
fn repair_npc_hp(hp: &mut Hp, layer: usize, amount: f64) -> Option<[f64; 3]> {
    if layer >= 3 || !amount.is_finite() || amount <= 0.0
        || !hp.now[2].is_finite() || hp.now[2] <= 0.0
        || hp.now.iter().chain(hp.max.iter()).any(|v| !v.is_finite())
        || hp.max[layer] <= 0.0 || hp.now[layer] >= hp.max[layer]
    {
        return None;
    }
    hp.now[layer] += amount.min(hp.max[layer] - hp.now[layer]);
    Some([0, 1, 2].map(|i| if hp.max[i] > 0.0 { (hp.now[i] / hp.max[i]).clamp(0.0, 1.0) } else { 0.0 }))
}

/// Repair a present unpiloted NPC or ship and publish health in the actor turn.
pub(crate) fn repair_npc_in_system(s: &mut crate::space::System, sde: &crate::sde::Sde, target: i64, layer: usize, amount: f64) -> bool {
    let Some(ball) = s.ball(target) else { return false };
    let category = sde.types.get(ball.type_id).and_then(|ty| ty.category_id);
    if ball.char_id.is_some() || (category == Some(crate::consts::CATEGORY_SHIP) && s.persistent_generation(target).is_some())
        || !(category == Some(crate::consts::CATEGORY_SHIP)
            || (category == Some(crate::consts::CATEGORY_ENTITY) && s.is_npc(target))
            || (category == Some(crate::consts::CATEGORY_DRONE) && s.drone(target).is_some()))
    {
        return false;
    }
    let health = {
        let mut st = state();
        st.hp.get_mut(&target).and_then(|hp| repair_npc_hp(hp, layer, amount))
    };
    if let Some(health) = health {
        s.set_health(target, health);
        true
    } else {
        false
    }
}

/// Repair a player's ship layer (0 shield, 1 armor, 2 structure) by `amount`.
pub(crate) async fn repair_player(
    db: &Db, world: &World, system: i64, ship: i64, char_id: i64, layer: usize,
    amount: f64, guard: impl FnOnce(&crate::space::System) -> bool + Send + 'static,
) -> Result<(), CallError> {
    let _lock = super::super::market::lock().await;
    if layer >= 3 || !amount.is_finite() || amount <= 0.0 { return Ok(()); }
    if world.ball(system, ship).await.is_none_or(|ball| ball.char_id != Some(char_id)) {
        return Ok(());
    }
    let stats = ship_stats(db, world, system, char_id, ship).await?;
    world.with(system, move |s| {
        if s.ball(ship).is_none_or(|ball| ball.char_id != Some(char_id)) || !guard(s) {
            return;
        }
        let mut st = state();
        let Some(cond) = st.condition.get_mut(&ship) else { return };
        let old_max = cond.stats.max;
        let old = cond.now;
        cond.now = [0, 1, 2].map(|i| cond.now[i].min(stats.max[i]));
        cond.stats = stats;
        let now = filetime_now();
        cond.now[0] = recharge(cond.now[0], cond.stats.max[0], cond.stats.shield_tau_ms, (now - cond.shield_at) as f64 / FILETIME_SECOND as f64);
        cond.shield_at = now;
        cond.now[layer] = (cond.now[layer] + amount).min(cond.stats.max[layer]);
        let max = cond.stats.max;
        let changes = vec![
            attribute_change(char_id, Value::Int(ship), ATTR_SHIELD_CHARGE, Value::Float(cond.now[0]), Value::Float(old[0])),
            attribute_change(char_id, Value::Int(ship), ATTR_ARMOR_DAMAGE, Value::Float(max[1] - cond.now[1]), Value::Float(old_max[1] - old[1])),
            attribute_change(char_id, Value::Int(ship), ATTR_HULL_DAMAGE, Value::Float(max[2] - cond.now[2]), Value::Float(old_max[2] - old[2])),
        ];
        let health = [0, 1, 2].map(|i| if max[i] > 0.0 { (cond.now[i] / max[i]).clamp(0.0, 1.0) } else { 0.0 });
        drop(st);
        s.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
        s.set_health(ship, health);
    }).await;
    Ok(())
}
/// Set the damage fraction of selected layers of the active player ship.
pub(crate) async fn set_damage(c: &Client, system: i64, ship: i64, char_id: i64, layers: [bool; 3], fraction: f64) -> Result<(), CallError> {
    let _lock = super::super::market::lock().await;
    if c.world.ball(system, ship).await.is_none_or(|ball| ball.char_id != Some(char_id)) {
        return Ok(());
    }
    let stats = ship_stats(&c.db, &c.world, system, char_id, ship).await?;
    let now = filetime_now();
    let mut cond = state().condition.get(&ship).cloned().unwrap_or_else(|| Condition {
        now: stats.max, stats: stats.clone(), shield_at: now,
    });
    cond.now[0] = recharge(cond.now[0], cond.stats.max[0], cond.stats.shield_tau_ms, (now - cond.shield_at) as f64 / FILETIME_SECOND as f64);
    let old = cond.now;
    let old_max = cond.stats.max;
    cond.now = set_damage_layers(cond.now, old_max, stats.max, layers, fraction);
    let max = stats.max;
    cond.stats = stats;
    cond.shield_at = now;
    persist_condition_in(&c.db, ship, &cond).await?;
    let changes = vec![
        attribute_change(char_id, Value::Int(ship), ATTR_SHIELD_CHARGE, Value::Float(cond.now[0]), Value::Float(old[0])),
        attribute_change(char_id, Value::Int(ship), ATTR_ARMOR_DAMAGE, Value::Float(max[1] - cond.now[1]), Value::Float(old_max[1] - old[1])),
        attribute_change(char_id, Value::Int(ship), ATTR_HULL_DAMAGE, Value::Float(max[2] - cond.now[2]), Value::Float(old_max[2] - old[2])),
    ];
    let health = [0, 1, 2].map(|i| if max[i] > 0.0 { (cond.now[i] / max[i]).clamp(0.0, 1.0) } else { 0.0 });
    state().condition.insert(ship, cond);
    c.world.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
    c.world.set_health(system, ship, health);
    Ok(())
}
fn set_damage_layers(now: [f64; 3], old_max: [f64; 3], max: [f64; 3], layers: [bool; 3], fraction: f64) -> [f64; 3] {
    [0, 1, 2].map(|i| {
        if layers[i] { max[i] * (1.0 - fraction) }
        else if i == 0 { now[i].min(max[i]) }
        else { (max[i] - (old_max[i] - now[i])).clamp(0.0, max[i].max(0.0)) }
    })
}



fn bounty_payout(bounty: f64, multiplier: f64) -> Option<f64> {
    let payout = bounty * multiplier;
    payout.is_finite().then_some(payout)
}

/// A destroyed NPC pays its bounty to the pilot who killed it and leaves a wreck (its
/// group's wreck type, sde_native.wrecks) holding loot worth about its raw SDE bounty.
pub(super) async fn reward_kill(db: &Db, world: &World, system: i64, npc: &Ball, killer: Option<i64>) -> Result<(), CallError> {
    let bounty = attr(world.sde(), npc.type_id, ATTR_ENTITY_KILL_BOUNTY, 0.0);
    if let Some(pilot) = killer.filter(|_| bounty > 0.0) {
        let payout = bounty_payout(bounty, crate::config::game().combat.bounty_multiplier)
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("nonfinite bounty payout for NPC type {}", npc.type_id)))?;
        if payout > 0.0 {
            let _lock = super::super::market::lock().await;
            let tax = db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    let corp_tax = crate::dao::characters::combat::corporation_tax(tx, pilot).await.map_err(db_err)?
                        .map(|r| (r.corporation_id, r.tax_rate.clamp(0.0, 1.0)));
                    let tax = corp_tax.map(|(_, rate)| super::super::market::round2(payout * rate)).unwrap_or(0.0);
                    super::super::wallets::credit(tx, outbox, pilot, payout - tax).await?;
                    if let Some((corp, _)) = corp_tax.filter(|_| tax > 0.0) {
                        super::super::wallets::corp_credit(tx, outbox, corp, ACCOUNT_CASH, tax).await?;
                    }
                    Ok(tax)
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await?;
            info!("{pilot} earned {payout:.0} ISK bounty for {} ({tax:.0} corporation tax)", npc.type_id);
        }
        super::super::crimewatch::npc_killed(db, world, pilot).await?;
    }
    let sde = world.sde();
    let Some(npc_type) = sde.types.get(npc.type_id) else { return Ok(()) };
    let Some(wreck_type) = npc_type.group_id.and_then(|group| sde.wrecks.get(group, npc_type.race_id.unwrap_or(0))) else { return Ok(()) };
    let owner = killer.unwrap_or(npc.owner_id);
    spawn_wreck(db, world, system, wreck_type, owner, npc.corp_id, npc.pos, Some(bounty)).await?;
    Ok(())
}

/// Create an inventory-backed wreck with the normal NPC loot selection.
pub(crate) async fn spawn_wreck(db: &Db, world: &World, system: i64, wreck_type: i32, owner: i64, corp: i64, pos: crate::space::V3, bounty: Option<f64>) -> Result<i64, CallError> {
    let sde = world.sde();
    let radius = sde.types.get(wreck_type).and_then(|t| t.radius).unwrap_or(100.0);
    let wreck_id = crate::dao::items::creation::positioned(db, wreck_type, owner, system, pos).await.map_err(db_err)?;
    // Loot: one or two modules or ammo stacks that traded on Serenity, worth up to the bounty.
    let picks = 1 + (rand01() * 2.0) as i64;
    let mut loot: Vec<_> = if let Some(bounty) = bounty { crate::dao::items::combat::loot(db, bounty).await.map_err(db_err)? } else { Vec::new() }.into_iter()
        .filter_map(|r| sde.types.get(r.type_id).and_then(|t| loot_category(t.published, t.category_id).map(|category| (r.type_id, category))))
        .collect();
    for i in 0..(picks as usize).min(loot.len()) {
        let j = i + (rand01() * (loot.len() - i) as f64) as usize;
        loot.swap(i, j);
    }
    loot.truncate(picks as usize);
    for &(type_id, category) in &loot {
        let quantity: i32 = if category == CATEGORY_CHARGE { 20 + (rand01() * 100.0) as i32 } else { 1 };
        crate::dao::items::combat::insert_loot(db, type_id, owner, wreck_id, quantity).await.map_err(db_err)?;
    }
    let ball = Ball {
        id: wreck_id,
        type_id: wreck_type,
        owner_id: owner,
        corp_id: corp,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: crate::space::MODE_RIGID,
        radius: radius as f32,
        mass: 10_000.0,
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
        slim: vec![("isEmpty", Value::Bool(loot.is_empty()))],
    };
    world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    debug!("wreck {wreck_id} ({wreck_type}) with {} loot stacks", loot.len());
    Ok(wreck_id)
}

fn loot_category(published: Option<bool>, category: Option<i32>) -> Option<i32> {
    category.filter(|&id| published == Some(true) && (id == CATEGORY_MODULE || id == CATEGORY_CHARGE))
}

#[cfg(test)] mod loot_tests;
#[cfg(test)] pub(in crate::services) mod tests;



#[cfg(test)] mod damage_tests;
