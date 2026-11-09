//! Ship health and memory-only capacitor state; routing lives in the parent module.
//! The client reads capacitor charge through clientDogmaLocation; player attributes come
//! from fitted dogma and NPC attributes from the SDE. Capacitor state is not persisted;
//! Ordinary warp debits the same balance synchronously at its activation boundary.
//! Guarded capacitor mutations validate source and range in the system actor; remote
//! injections also respect active Entosis assistance restrictions on capital hulls.
//! NPC death is journaled before native removal; bounty and wreck processing follow separately.
//! NPC bounty net and corporation tax commit together; wallet notices and events follow commit.
//! NPC repairs use actor-owned condition and are not persisted. Persistent ships and
//! drones publish live condition from their actor and checkpoint it in batches.
//! Positive pilot damage with a supplied source ball emits after actor health update or actual removal.
use super::*;
use infinity_core::consts::CATEGORY_MODULE;
pub(crate) mod death;
pub(crate) use death::settle_npc_loss;


// ---------------------------------------------------------------- capacitor and hit points

/// Capacitor or shield after `dt` seconds of passive recharge from `level` (the game's
/// curve: recharge peaks at 25%, ~98% after one recharge time `tau_ms`).
pub(crate) fn recharge(level: f64, max: f64, tau_ms: f64, dt: f64) -> f64 {
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
/// Result of an atomic recharge and partial capacitor debit for warp.
pub(crate) struct WarpCapacitorDrain {
    pub(crate) before: f64,
    pub(crate) after: f64,
    pub(crate) drained: f64,
}

/// Debit a validated positive warp cost using the shared capacitor authority.
pub(crate) fn drain_warp_capacitor(
    ship: i64, capacity: f64, tau: f64, full_cost: f64, at: i64,
) -> WarpCapacitorDrain {
    let mut st = state();
    let result = mutate(&mut st.capacitor, ship, capacity, tau, at, Operation::Drain(full_cost));
    WarpCapacitorDrain { before: result.before, after: result.after, drained: result.amount }
}


/// Rebase the live capacitor using its old recharge curve before changing its capacity.
pub(crate) fn rebase_capacitor(ship: i64, old_capacity: f64, old_tau: f64, new_capacity: f64, _new_tau: f64) -> (f64, f64) {
    let mut st = state();
    rebase_capacitor_at(&mut st.capacitor, ship, old_capacity, old_tau, new_capacity, filetime_now())
}

fn rebase_capacitor_at(entries: &mut HashMap<i64, (f64, i64)>, ship: i64, old_capacity: f64, old_tau: f64, new_capacity: f64, now: i64) -> (f64, f64) {
    let before = mutate(entries, ship, old_capacity, old_tau, now, Operation::Snapshot).before;
    let capacity = if new_capacity.is_finite() { new_capacity.max(0.0) } else { 0.0 };
    let after = before.min(capacity);
    entries.insert(ship, (after, now));
    (before, after)
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
    fn source(&self) -> i64 {
        match self {
            Self::Assistance { source, .. } | Self::DroneAssistance { source, .. } | Self::Attack { source, .. } => *source,
        }
    }

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
                                .is_some_and(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP));
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
            || matches!(op, Operation::Inject(_)) && guard.as_ref().is_some_and(|guard|
                guard.source() != ship && crate::space::sovereignty::links::assistance_blocked(s, ship))
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

/// Inject energy only while the source and range guard remain valid.
pub(crate) async fn inject_guarded(db: &Db, world: &World, system: i64, ship: i64, amount: f64, guard: NpcCapacitorGuard) -> Result<f64, CallError> {
    Ok(capacitor_operation(db, world, system, ship, Operation::Inject(amount), Some(guard)).await?.map_or(0.0, |(_, amount, _)| amount))
}

/// Drain only while the guarded attack remains valid. `None` means refused;
/// an accepted drain can have a zero amount.
pub(crate) async fn drain_exact_guarded(db: &Db, world: &World, system: i64, ship: i64, amount: f64, guard: NpcCapacitorGuard) -> Result<Option<(f64, Option<i64>)>, CallError> {
    Ok(capacitor_operation(db, world, system, ship, Operation::Drain(amount), Some(guard)).await?.map(|(_, amount, owner)| (amount, owner)))
}

/// A ship's hit points and resistances, from its fit.
#[derive(Clone, Debug)]
pub(crate) struct ShipStats {
    pub(crate) max: [f64; 3],
    pub(crate) resonance: [[f64; 4]; 3],
    pub(crate) shield_tau_ms: f64,
}

impl ShipStats {
    pub(crate) fn from_fit(fit: &Fit, key: Key) -> Self {
        Self {
            max: ATTR_HP.map(|a| fit.value(key, a)),
            resonance: ATTR_RESONANCE.map(|layer| layer.map(|a| fit.value(key, a))),
            shield_tau_ms: fit.value(key, ATTR_SHIELD_RECHARGE_RATE),
        }
    }
}

/// A player ship's condition: shield (recharging since `shield_at`), armor and structure left.
#[derive(Clone, Debug)]
pub(crate) struct Condition {
    pub(crate) stats: ShipStats,
    pub(crate) now: [f64; 3],
    pub(crate) shield_at: i64,
}

/// Apply selected damage fractions while preserving absolute unselected damage.
pub(crate) fn set_damage_layers(now: [f64; 3], old_max: [f64; 3], max: [f64; 3], layers: [bool; 3], fraction: f64) -> [f64; 3] {
    [0, 1, 2].map(|i| {
        if layers[i] { max[i] * (1.0 - fraction.clamp(0.0, 1.0)) }
        else if i == 0 { now[i].min(max[i]) }
        else { max[i] - (old_max[i] - now[i]).max(0.0) }
    })
}






async fn finish_npc_kill(db: &Db, world: &World, system: i64, target: i64, ball: Ball, attacker: Option<i64>) -> Result<(), CallError> {
    info!("ball {target} (type {}) destroyed", ball.type_id);
    super::super::map::events::npc_lost(db, system, filetime_now()).await;
    if let Err(e) = reward_kill(db, world, system, &ball, attacker).await {
        warn!("wreck of {target}: {}", match e {
            CallError::Internal(e) => format!("{e:#}"),
            CallError::User { msg, .. } => msg,
        });
    }
    Ok(())
}

/// NPC layer health for assistance selection; the actor publishes authoritative fractions on the ball.
pub(crate) fn npc_health_fraction(ball: &Ball, layer: usize) -> Option<f64> {
    if layer >= 3 { return None; }
    let value = ball.health[layer];
    value.is_finite().then_some(value.clamp(0.0, 1.0))
}


/// Set the damage fraction of selected layers of the active player ship.
pub(crate) async fn set_damage(c: &Client, system: i64, ship: i64, char_id: i64, layers: [bool; 3], fraction: f64) -> Result<(), CallError> {
    if !fraction.is_finite() { return Ok(()); }
    let mask = layers.into_iter().enumerate().fold(0, |mask, (layer, selected)| {
        if selected { mask | (1 << layer) } else { mask }
    });
    let changed = c.world.with(system, move |s| {
        if !s.ball(ship).is_some_and(|ball| ball.char_id == Some(char_id)) { return None; }
        let (_, old) = s.combat_health_scalars(ship)?;
        if !s.set_condition_fraction_mask(ship, mask, 1.0 - fraction.clamp(0.0, 1.0)) { return None; }
        let (max, now) = s.combat_health_scalars(ship)?;
        let health = [0, 1, 2].map(|i| if max[i] > 0.0 {
            if i == 0 { (now[i] / max[i]).clamp(0.0, 1.0) }
            else { (1.0 - now[i] / max[i]).clamp(0.0, 1.0) }
        } else { 0.0 });
        s.set_health(ship, health);
        Some((old, now))
    }).await.flatten();
    if let Some((old, now)) = changed {
        let ids = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];
        let changes = (0..3).map(|i| attribute_change(char_id, Value::Int(ship), ids[i],
            Value::Float(now[i]), Value::Float(old[i]))).collect();
        c.world.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
    }
    Ok(())
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
            db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    super::super::corps::tax::settle_reward(tx, outbox, pilot, payout, None).await
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await?;
            info!("{pilot} earned {payout:.0} ISK bounty for {}", npc.type_id);
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
        let quantity: i64 = if category == CATEGORY_CHARGE { 20 + (rand01() * 100.0) as i64 } else { 1 };
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
