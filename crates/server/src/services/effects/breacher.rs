//! Breacher pod damage-over-time applications (SCARAB pods from the Tholos and
//! Cenotaph breacher pod launchers). Mirrors the client's `dotWeapons/common/`
//! registry (`dotRegistry.py`, `dotAppsOnTarget.py`, `dotApplication.py` and
//! `dotUtil.py`) and the victim visuals in
//! `eve/client/script/environment/spaceObject/ship.py` `UpdateDamageOverTime`:
//! one flat damage application per second, resolved for the single strongest
//! application only.
//!
//! A pod flies and lands through the ordinary missile path; its contact
//! registers an application here instead of dealing typed damage. Registered
//! applications are actor-transient state like missile flights: they are not
//! persisted, do not survive a restart, and end when the target ball or the
//! source ship leaves this actor (the retail server may let damage continue
//! after the source departs; here the shared damage authority requires a live
//! source). Data sources are the resident `Fit` for the dot attributes and the
//! target's `breacherPodDamageResistance`, `Sde` type data for the charge's
//! effect, and combat facts for per-tick source admission. Not supported:
//! persistence or restart recovery, DOT that outlives its source, the GM
//! `dotWeapons` commands, and NPC-operated pods.
use std::collections::HashMap;
use std::sync::Weak;
use marshal::Value;
use crate::client::{filetime_now, filetime_ticks_from_ms};
use crate::db::Db;
use crate::space::{System, World};
use crate::space::combat_facts::{CombatSource, Refusal};
use crate::space::system::observers::BallToken;
use crate::services::combat::damage::{apply_flat_damage_in_actor, DamageSource};
use crate::services::fit::{Fit, Key};
use crate::lua::events::resources::FactOrigin;
use infinity_core::consts::dogma::{ATTR_BREACHER_POD_DAMAGE_RESISTANCE, ATTR_DAMAGE_MULTIPLIER,
    ATTR_DOT_DURATION, ATTR_DOT_MAX_DAMAGE_PER_TICK, ATTR_DOT_MAX_HP_PERCENTAGE_PER_TICK,
    EFFECT_DOT_MISSILE_LAUNCHING};

mod wire;

/// Client dotWeapons/common/dotRegistry.py: DOT_TICK_RATE_SEC = 1.
const DOT_TICK_MS: u64 = 1_000;
/// Client dotWeapons/common/dotAppsOnTarget.py: FLOAT_TOLERANCE_DAMAGE_CHANGES.
const PUBLISHED_FRACTION_TOLERANCE: f64 = 0.001;
/// Client dotWeapons/common/dotConst.py.
const DOT_IDLE: i64 = 1;
/// Client dotWeapons/common/dotConst.py.
const DOT_ACTIVE: i64 = 2;

/// The fitted damage-over-time numbers of one loaded breacher pod charge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DotPayload {
    pub(crate) duration_ms: f64,
    pub(crate) max_damage_per_tick: f64,
    pub(crate) max_hp_fraction_per_tick: f64,
}

/// One registered pod application on a target; all scalar state lives in the
/// actor's `DotState` like a missile flight's does in its ball entry.
#[derive(Clone, Copy)]
pub(crate) struct DotApplication {
    id: i64,
    attacker: Option<i64>,
    source_ship: i64,
    source_type: i32,
    source_generation: u64,
    module: Option<i64>,
    weapon_type: i32,
    illegal: bool,
    expires_ft: i64,
    max_damage: f64,
    max_hp_fraction: f64,
    generation: u64,
    sequence: u64,
    active: bool,
}

struct DotTarget {
    applications: Vec<DotApplication>,
    /// Last damage fraction published on the slim item as
    /// `dotHpPercentageApplied` (`100 * fraction` on the wire).
    published_fraction: Option<f64>,
    /// Pending tick timer: its generation and the fenced target ball token.
    armed: Option<(u64, BallToken)>,
}

/// Actor-owned breacher pod applications keyed by target ball. Transient like
/// missile flights: dropped on restart and on ball departure.
#[derive(Default)]
pub(crate) struct DotState {
    targets: HashMap<i64, DotTarget>,
    next_application: i64,
    timer_sequence: u64,
}

impl DotTarget {
    /// Drop expired applications and ones whose source fails `valid`; returns
    /// the removed applications so the caller can retire their combat
    /// generations and notify their attackers. The vector stays unallocated
    /// while nothing is removed.
    fn prune(&mut self, now_ft: i64, valid: impl Fn(&DotApplication) -> bool) -> Vec<DotApplication> {
        let mut removed = Vec::new();
        self.applications.retain(|app| {
            if app.expires_ft <= now_ft || !valid(app) {
                removed.push(*app);
                return false;
            }
            true
        });
        removed
    }

    /// The single application allowed to deal damage this tick and its
    /// effective amount: highest damage wins, and a currently active
    /// application wins ties (client dotAppsOnTarget.py keeps the last applied
    /// application on equal damage).
    fn select(&self, total_hp: f64, resistance: f64) -> Option<(usize, f64)> {
        let mut best: Option<(usize, f64)> = None;
        for (index, app) in self.applications.iter().enumerate() {
            let damage = effective_tick_damage(app.max_damage, app.max_hp_fraction, total_hp, resistance);
            let better = match best {
                None => true,
                Some((winner, amount)) =>
                    damage > amount || (damage == amount && app.active && !self.applications[winner].active),
            };
            if better { best = Some((index, damage)); }
        }
        best
    }

    /// Mark `index` as the applied application; returns (any state change,
    /// previous active attacker, new active attacker) for notifications.
    fn set_active(&mut self, index: usize) -> (bool, Option<i64>, Option<i64>) {
        let old_attacker = self.applications.iter().find(|app| app.active).and_then(|app| app.attacker);
        let mut changed = false;
        for (current, app) in self.applications.iter_mut().enumerate() {
            let active = current == index;
            changed |= app.active != active;
            app.active = active;
        }
        (changed, old_attacker, self.applications.get(index).and_then(|app| app.attacker))
    }
}

/// `dotUtil.GetEffectiveDamage` with the client's resistance multiplication:
/// `int(min(ratio * total, max)) * resistance`; any non-finite or negative
/// input yields 0.
pub(crate) fn effective_tick_damage(max_damage: f64, max_hp_fraction: f64, total_hp: f64, resistance: f64) -> f64 {
    if !(max_damage.is_finite() && max_hp_fraction.is_finite() && total_hp.is_finite() && resistance.is_finite())
        || max_damage < 0.0 || max_hp_fraction < 0.0 || total_hp < 0.0 || resistance < 0.0 {
        return 0.0;
    }
    (max_hp_fraction * total_hp).min(max_damage).trunc() * resistance
}

/// Some when the loaded charge carries dotMissileLaunching and the three
/// fitted values are finite and positive.
pub(super) fn capture_dot(fit: &Fit, module: &super::lifecycle::Module) -> Option<DotPayload> {
    let key = Key::Charge(module.id);
    let charge = fit.source_type(key)?;
    if !fit.sde().types.effects(charge).iter().any(|effect| effect.effect_id == EFFECT_DOT_MISSILE_LAUNCHING) {
        return None;
    }
    let payload = DotPayload {
        duration_ms: fit.value(key, ATTR_DOT_DURATION),
        max_damage_per_tick: fit.value(key, ATTR_DOT_MAX_DAMAGE_PER_TICK)
            * fit.value(Key::Module(module.id), ATTR_DAMAGE_MULTIPLIER).max(0.0),
        max_hp_fraction_per_tick: fit.value(key, ATTR_DOT_MAX_HP_PERCENTAGE_PER_TICK).clamp(0.0, 100.0) / 100.0,
    };
    (payload.duration_ms.is_finite() && payload.duration_ms > 0.0
        && payload.max_damage_per_tick.is_finite() && payload.max_damage_per_tick > 0.0
        && payload.max_hp_fraction_per_tick.is_finite() && payload.max_hp_fraction_per_tick > 0.0)
        .then_some(payload)
}

/// Send `applications`, the surviving set for `target`, as
/// `OnDotVictimUpdated` to the victim pilot and every attacker character in
/// the set, plus `extra` characters such as attackers whose applications just
/// left it. An empty slice encodes the clearing update.
fn broadcast(s: &System, target: i64, applications: &[DotApplication], extra: impl IntoIterator<Item = i64>) {
    let mut recipients: Vec<i64> = applications.iter().filter_map(|app| app.attacker).collect();
    recipients.extend(extra);
    if let Some(victim) = s.combat_owner(target) { recipients.push(victim); }
    recipients.sort_unstable();
    recipients.dedup();
    if recipients.is_empty() { return; }
    let args = wire::victim_update(target, applications);
    for character in recipients { s.notify_pilot(character, "OnDotVictimUpdated", args.clone()); }
}

/// Register one accepted pod contact on `target`; returns false (nothing
/// registered) when the target has no live combat record or the payload is
/// invalid. Arms the target timer on the first application and notifies the
/// victim pilot and the attacker.
pub(crate) fn apply_pod_contact(
    s: &mut System, db: Db, world: Weak<World>, target: i64,
    source: &CombatSource, module: Option<i64>, weapon_type: i32, illegal: bool, payload: DotPayload,
) -> bool {
    if s.combat_membership_generation(target).is_none() { return false; }
    if !(payload.duration_ms.is_finite() && payload.duration_ms > 0.0
        && payload.max_damage_per_tick.is_finite() && payload.max_damage_per_tick > 0.0
        && payload.max_hp_fraction_per_tick.is_finite() && payload.max_hp_fraction_per_tick > 0.0) { return false; }
    let source_ship = source.entity.item_id;
    let Some(source_generation) = s.combat_membership_generation(source_ship) else { return false };
    let Some(token) = s.capture_ball(target) else { return false };
    let expires_ft = filetime_now().saturating_add(filetime_ticks_from_ms(payload.duration_ms));
    let id = s.breacher.next_application;
    s.breacher.next_application = id.saturating_add(1);
    let application = DotApplication {
        id,
        attacker: source.actual_actor_character,
        source_ship,
        source_type: source.entity.type_id,
        source_generation,
        module,
        weapon_type,
        illegal,
        expires_ft,
        max_damage: payload.max_damage_per_tick,
        max_hp_fraction: payload.max_hp_fraction_per_tick,
        generation: crate::space::targeting::next_generation(),
        sequence: 0,
        active: false,
    };
    // The prepared facts carry this actor's own system id for every real
    // (positive-id) system; the tick needs it only for loss settlement.
    let system = source.context.system_id.unwrap_or(0);
    let entry = s.breacher.targets.entry(target).or_insert_with(|| DotTarget {
        applications: Vec::new(), published_fraction: None, armed: None });
    entry.applications.push(application);
    if entry.armed.is_none() {
        let generation = s.breacher.timer_sequence;
        s.breacher.timer_sequence = generation.wrapping_add(1);
        entry.armed = Some((generation, token));
        s.schedule(DOT_TICK_MS, move |s| tick(s, db, world, system, target, token, generation));
    }
    broadcast(s, target, s.breacher.targets.get(&target)
        .map(|entry| entry.applications.as_slice()).unwrap_or(&[]), []);
    true
}

/// Named timer body; fenced by the captured ball token and timer generation.
/// Never awaits and never touches the database except through
/// `apply_flat_damage_in_actor`'s lethal deferral.
fn tick(s: &mut System, db: Db, world: Weak<World>, system: i64, target: i64, token: BallToken, generation: u64) {
    if !s.ball_token_current(token) { return; }
    let Some(mut entry) = s.breacher.targets.remove(&target) else { return; };
    if entry.armed.map(|(armed, _)| armed) != Some(generation) {
        // A newer timer owns this target; keep the entry and let this stale
        // run end without a re-arm.
        s.breacher.targets.insert(target, entry);
        return;
    }
    let now_ft = filetime_now();
    let mut removed = entry.prune(now_ft, |app| {
        app.attacker.and_then(|character| s.pilot_ball(character)).map(|ball| ball.id) == Some(app.source_ship)
            && s.combat_membership_generation(app.source_ship) == Some(app.source_generation)
    });
    for app in &removed { let _ = s.retire_combat_source(app.generation); }
    let Some((max_hp, _)) = s.combat_health_scalars(target) else {
        // The target's combat record is gone: its applications end with it,
        // the slim field clears and the empty set reaches every attacker.
        for app in &entry.applications { let _ = s.retire_combat_source(app.generation); }
        s.publish_slim_field(target, wire::SLIM_DOT_FIELD, None);
        broadcast(s, target, &[], removed.iter().chain(entry.applications.iter())
            .filter_map(|app| app.attacker));
        return;
    };
    if entry.applications.is_empty() {
        // The last application left: clear the slim field, publish the empty
        // set and do not re-arm the target timer.
        s.publish_slim_field(target, wire::SLIM_DOT_FIELD, None);
        broadcast(s, target, &entry.applications, removed.iter().filter_map(|app| app.attacker));
        return;
    }
    let total_hp: f64 = max_hp.iter().sum();
    let resistance = s.with_combat_fit(target, |fit, key, _| fit.value(key, ATTR_BREACHER_POD_DAMAGE_RESISTANCE))
        .unwrap_or(1.0).max(0.0);
    let mut applied: Option<(usize, f64, CombatSource)> = None;
    loop {
        let Some((index, damage)) = entry.select(total_hp, resistance) else { break };
        let app = &entry.applications[index];
        let Some(sequence) = app.sequence.checked_add(1) else {
            let dropped = entry.applications.remove(index);
            let _ = s.retire_combat_source(dropped.generation);
            removed.push(dropped);
            continue;
        };
        let source_ship = app.source_ship;
        let source_generation = app.source_generation;
        let result = s.prepare_combat_source(source_ship, app.module, Some(i64::from(EFFECT_DOT_MISSILE_LAUNCHING)),
            Some(app.weapon_type), FactOrigin::Natural, app.generation, sequence, 1,
            move |s: &System| s.combat_membership_generation(source_ship) == Some(source_generation));
        match result {
            Ok(facts) => {
                entry.applications[index].sequence = sequence;
                applied = Some((index, damage, facts));
                break;
            }
            Err(Refusal::Full | Refusal::Busy) => {
                // Transient capacity: keep the application and skip this tick.
                tracing::debug!(target, "combat fact capacity busy; breacher tick skipped");
                break;
            }
            Err(refusal) => {
                let dropped = entry.applications.remove(index);
                let _ = s.retire_combat_source(dropped.generation);
                removed.push(dropped);
                tracing::warn!(target, ?refusal, "breacher application source rejected; dropped");
            }
        }
    }
    if let Some((index, damage, facts)) = applied {
        let app = entry.applications[index];
        let result = apply_flat_damage_in_actor(s, &db, system, target, damage, DamageSource {
            attacker_id: app.attacker, attacker_ball_id: Some(app.source_ship),
            source_ship_id: Some(app.source_ship), source_ship_type: Some(app.source_type),
            weapon_type_id: Some(app.weapon_type), facts: Some(facts),
            force_origin: None, illegal: app.illegal,
        });
        let (dealt, receipt) = match result {
            Ok(Some((outcome, receipt))) => (outcome.dealt, receipt),
            Ok(None) => (0.0, None),
            Err(error) => {
                tracing::warn!(?error, target, "breacher tick damage failed");
                (0.0, None)
            }
        };
        let (changed, old_attacker, _) = entry.set_active(index);
        if changed || !removed.is_empty() {
            // Any set change republishes the survivors: an expiry or a dropped
            // source reaches the victim, the attackers still on the set and
            // the removed attackers, not only a newly applied application.
            broadcast(s, target, &entry.applications,
                removed.iter().filter_map(|app| app.attacker).chain(old_attacker));
        }
        let fraction = if total_hp > 0.0 { dealt / total_hp } else { 0.0 };
        if entry.published_fraction.is_none_or(|old| (fraction - old).abs() > PUBLISHED_FRACTION_TOLERANCE) {
            s.publish_slim_field(target, wire::SLIM_DOT_FIELD, Some(Value::Float(100.0 * fraction)));
            entry.published_fraction = Some(fraction);
        }
        let victim = s.combat_owner(target);
        super::weapons::defer_hit_report(s, world.clone(), app.id, app.source_ship, target, app.attacker,
            victim, app.weapon_type, dealt, receipt, None);
    } else if !removed.is_empty() && !entry.applications.is_empty() {
        // Removals without an applied application still publish the survivors.
        broadcast(s, target, &entry.applications, removed.iter().filter_map(|app| app.attacker));
    }
    if entry.applications.is_empty() {
        s.publish_slim_field(target, wire::SLIM_DOT_FIELD, None);
        broadcast(s, target, &entry.applications, removed.iter().filter_map(|app| app.attacker));
        return;
    }
    let generation = s.breacher.timer_sequence;
    s.breacher.timer_sequence = generation.wrapping_add(1);
    entry.armed = Some((generation, token));
    s.breacher.targets.insert(target, entry);
    s.schedule(DOT_TICK_MS, move |s| tick(s, db, world, system, target, token, generation));
}

/// Scene-exit hook: drop the ball's own DOT target entry and every application
/// it sourced, retiring their combat generations.
pub(crate) fn remove_ball(s: &mut System, ball: i64) {
    if let Some(entry) = s.breacher.targets.remove(&ball) {
        let mut attackers = Vec::new();
        for app in &entry.applications {
            let _ = s.retire_combat_source(app.generation);
            attackers.extend(app.attacker);
        }
        // The departing ball keeps no slim republish; its pilot and the
        // attackers still get the clearing set (OnSessionChanged also clears).
        broadcast(s, ball, &[], attackers);
    }
    let mut affected: Vec<(i64, Vec<i64>, Vec<u64>)> = Vec::new();
    for (&target, entry) in s.breacher.targets.iter_mut() {
        let mut attackers = Vec::new();
        let mut retired = Vec::new();
        entry.applications.retain(|app| {
            if app.source_ship == ball {
                attackers.extend(app.attacker);
                retired.push(app.generation);
                return false;
            }
            true
        });
        if !retired.is_empty() { affected.push((target, attackers, retired)); }
    }
    for (target, attackers, retired) in affected {
        for generation in retired { let _ = s.retire_combat_source(generation); }
        if s.breacher.targets.get(&target).is_some_and(|entry| entry.applications.is_empty()) {
            s.breacher.targets.remove(&target);
            s.publish_slim_field(target, wire::SLIM_DOT_FIELD, None);
            broadcast(s, target, &[], attackers);
        } else {
            // The surviving set reaches the victim, the attackers still on it
            // and the removed attackers.
            broadcast(s, target, s.breacher.targets.get(&target)
                .map(|entry| entry.applications.as_slice()).unwrap_or(&[]), attackers);
        }
    }
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
