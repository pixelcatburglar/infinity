//! Actor-owned superweapon firing state and shared visual constructors.
//! `arm_super_weapon_firing` installs one accepted activation: per-source
//! lockout deadlines and generation-guarded damage timers that re-arm until
//! the fitted pulse window ends. Cylinder, cone and slash hit sets are
//! evaluated inside the actor on every pulse, so balls entering the shape
//! after the warning are still hit. Timers carry only captured values; no
//! pulse reads the database. The GUID/graphicInfo constructors mirror the
//! client `SuperWeapon`, `DirectionalWeapon` and `SlashWeapon` effect
//! consumers and are shared with GM previews.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use crate::services::keyval;
use crate::space::{add, cross, dist, dot, norm, scale, sub, MODE_WARP, V3};
use marshal::Value;
use super::System;
use crate::services::effects::aoe::DamageApplication;

/// The client-exported racial or race-free superweapon visual family.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuperWeaponFxFamily {
    Single,
    DamageBeam,
    SlashBeam,
    Cone,
}

/// Construct one of the client-exported superweapon effect GUIDs.
pub(crate) fn super_weapon_guid(family: SuperWeaponFxFamily, race: Option<i32>) -> Option<&'static str> {
    match (family, race) {
        (SuperWeaponFxFamily::Cone, _) => Some("effects.SuperWeaponCone"),
        (SuperWeaponFxFamily::Single, Some(4)) => Some("effects.SuperWeaponAmarr"),
        (SuperWeaponFxFamily::Single, Some(1)) => Some("effects.SuperWeaponCaldari"),
        (SuperWeaponFxFamily::Single, Some(8)) => Some("effects.SuperWeaponGallente"),
        (SuperWeaponFxFamily::Single, Some(2)) => Some("effects.SuperWeaponMinmatar"),
        (SuperWeaponFxFamily::DamageBeam, Some(4)) => Some("effects.SWDamageBeamAmarr"),
        (SuperWeaponFxFamily::DamageBeam, Some(1)) => Some("effects.SWDamageBeamCaldari"),
        (SuperWeaponFxFamily::DamageBeam, Some(8)) => Some("effects.SWDamageBeamGallente"),
        (SuperWeaponFxFamily::DamageBeam, Some(2)) => Some("effects.SWDamageBeamMinmatar"),
        (SuperWeaponFxFamily::SlashBeam, Some(4)) => Some("effects.SWSlashBeamAmarr"),
        (SuperWeaponFxFamily::SlashBeam, Some(1)) => Some("effects.SWSlashBeamCaldari"),
        (SuperWeaponFxFamily::SlashBeam, Some(8)) => Some("effects.SWSlashBeamGallente"),
        (SuperWeaponFxFamily::SlashBeam, Some(2)) => Some("effects.SWSlashBeamMinmatar"),
        _ => None,
    }
}

fn vector_value(vector: V3) -> Value {
    Value::tuple(vector.into_iter().map(Value::Float).collect::<Vec<_>>())
}

/// DirectionalWeapon's attribute-style graphicInfo payload.
pub(crate) fn directional_weapon_graphic_info(target_offset: V3, duration_ms: f64) -> Value {
    keyval([
        ("targetOffset", vector_value(target_offset)),
        ("duration", Value::Float(duration_ms)),
    ])
}

/// SlashWeapon's attribute-style graphicInfo payload.
pub(crate) fn slash_weapon_graphic_info(start: V3, end: V3, duration_ms: f64) -> Value {
    keyval([
        ("startTargetOffset", vector_value(start)),
        ("endTargetOffset", vector_value(end)),
        ("duration", Value::Float(duration_ms)),
    ])
}

/// The immutable spec one accepted firing carries into every actor pulse.
/// Only scalar facts, the activation generation and frozen geometry are kept:
/// no fit, database handle, target ball or Lua state survives admission.
#[derive(Clone)]
pub(crate) struct FiringSpec {
    pub(crate) ship: i64,
    pub(crate) character: i64,
    pub(crate) master: i64,
    pub(crate) weapon_type: i32,
    pub(crate) effect: i64,
    pub(crate) generation: Arc<AtomicBool>,
    pub(crate) combat_generation: u64,
    /// Combat-facts sequence of the first pulse; each hit uses the next.
    pub(crate) sequence: u64,
    /// Frozen hit shape built at admission from the aim and source position.
    pub(crate) aim: Aim,
    /// Delay before the first hit, from the actor's millisecond clock at the
    /// arm turn (fitted warning or damage delay).
    pub(crate) hit_delay_ms: i64,
    /// Length of the damage-over-time window for pulsed families.
    pub(crate) window_ms: i64,
    /// Pulse cadence for damage-over-time families.
    pub(crate) cycle_ms: f64,
    /// Fitted damage radius (beam half-width, slash beam thickness).
    pub(crate) radius: f64,
    /// Fitted maximum range (beam/cone reach, slash sweep extent).
    pub(crate) range: f64,
    /// Fitted weapon signature radius; nonpositive disables signature scaling.
    pub(crate) signature_radius: f64,
    pub(crate) damage: [f64; 4],
    /// Fitted source lockout durations armed once at accepted firing.
    pub(crate) immobility_ms: i64,
    pub(crate) no_jump_or_cloak_ms: i64,
    /// One normal-target hit instead of a damage-over-time pulse train.
    pub(crate) single: bool,
    pub(crate) illegal: bool,
}

/// The frozen hit shape of one firing; positions and directions are sampled
/// at admission inside the actor and never refreshed.
#[derive(Clone, Copy)]
pub(crate) enum Aim {
    /// One captured normal target guarded by its membership generation.
    Target { id: i64, ball_generation: u64 },
    /// Beam DOT: a segment of `range` length from `origin` along `direction`.
    Cylinder { origin: V3, direction: V3 },
    /// Cone DOT: `direction`'s apex at `origin`, reach `range`, half-angle
    /// `cos_half` (precomputed cosine of the approved proxy half-angle).
    Cone { origin: V3, direction: V3, cos_half: f64 },
    /// Slash sweep: the beam sweeps the great arc from `d0` to `d1` around
    /// unit `axis` through `angle` radians, extending `range` from `origin`.
    Slash { origin: V3, d0: V3, d1: V3, axis: V3, angle: f64 },
}

/// One armed firing: its immutable spec plus pulse bookkeeping. The window
/// bounds are actor millisecond times resolved once at the arm turn.
struct Firing {
    spec: FiringSpec,
    window_start: i64,
    window_end: i64,
    /// Last pulse index already applied; -1 before the first.
    last_pulse: i64,
    /// Next combat-facts sequence for the next pulse.
    next_sequence: u64,
}

/// The firing ship's superweapon deadlines, armed once at accepted firing.
struct Lockout {
    immobile_until: i64,
    jump_cloak_until: i64,
}

/// Actor-owned superweapon state: firing-source lockouts and live pulse jobs.
#[derive(Default)]
pub(crate) struct SuperWeapons {
    lockouts: HashMap<i64, Lockout>,
    firings: HashMap<i64, Firing>,
}

/// The same source/activation validity every delayed pulse rechecks: the
/// activation generation still live, the owner still piloting the same ball,
/// and the source not in warp.
fn firing_valid(s: &System, spec: &FiringSpec) -> bool {
    !spec.generation.load(Ordering::Relaxed)
        && crate::services::effects::lifecycle::activation_current(spec.master, spec.character, &spec.generation)
        && s.ball(spec.ship).is_some_and(|ball| ball.char_id == Some(spec.character) && ball.mode != MODE_WARP)
}

fn distance_to_segment(a: V3, b: V3, p: V3) -> f64 {
    let ab = sub(b, a);
    let len2 = dot(ab, ab);
    let t = if len2 > 0.0 { (dot(sub(p, a), ab) / len2).clamp(0.0, 1.0) } else { 0.0 };
    dist(p, add(a, scale(ab, t)))
}

impl System {
    /// Arm one accepted firing's source lockouts and damage timers. Runs
    /// inside the actor turn immediately after the admission fuel commit;
    /// returns false (and arms nothing) when the generation is already stale.
    pub(crate) fn arm_super_weapon_firing(&mut self, spec: FiringSpec) -> bool {
        if !firing_valid(self, &spec) { return false; }
        let now = self.ms;
        // A lockout with both deadlines elapsed is dead weight; dropping it
        // here keeps the map bounded by live firings.
        self.super_weapons.lockouts.retain(|_, entry|
            entry.immobile_until > now || entry.jump_cloak_until > now);
        let ship = spec.ship;
        let immobile_until = now.saturating_add(spec.immobility_ms);
        let jump_cloak_until = now.saturating_add(spec.no_jump_or_cloak_ms);
        self.super_weapons.lockouts.insert(ship, Lockout { immobile_until, jump_cloak_until });
        let window_start = now.saturating_add(spec.hit_delay_ms);
        let window_end = window_start.saturating_add(spec.window_ms);
        let master = spec.master;
        self.super_weapons.firings.insert(master, Firing {
            window_start, window_end, next_sequence: spec.sequence, last_pulse: -1, spec,
        });
        self.schedule_at(window_start, move |s| s.super_weapon_pulse(master));
        // The lockout's own expiry sweep: deadline equality proves the entry
        // is still this arm, so a re-armed lockout is never removed.
        self.schedule_at(immobile_until.max(jump_cloak_until),
            move |s| s.expire_super_weapon_lockout(ship, immobile_until, jump_cloak_until));
        true
    }

    /// The firing ship's movement/warp refusal deadline, if a firing armed
    /// one; navigation checks compare it against the actor's `now_ms`. The
    /// lockout outlives its activation: a stopped or offlined module never
    /// clears an armed deadline.
    pub(crate) fn super_weapon_immobile_until(&self, ship: i64) -> Option<i64> {
        self.super_weapons.lockouts.get(&ship).map(|entry| entry.immobile_until)
    }

    /// The firing ship's jump/cloak refusal deadline, same arming rule.
    pub(crate) fn super_weapon_jump_cloak_until(&self, ship: i64) -> Option<i64> {
        self.super_weapons.lockouts.get(&ship).map(|entry| entry.jump_cloak_until)
    }

    /// Retire one firing's pulse job when the activation stops; a stale
    /// generation never clears a newer firing of the same module. The ship's
    /// lockout is unaffected: an armed deadline outlives its activation.
    pub(crate) fn end_super_weapon_firing(&mut self, master: i64, generation: &Arc<AtomicBool>) {
        if self.super_weapons.firings.get(&master).is_some_and(|firing| Arc::ptr_eq(&firing.spec.generation, generation)) {
            self.super_weapons.firings.remove(&master);
        }
    }

    /// Drop the lockout armed with these deadlines once both have elapsed.
    /// Deadline equality proves the entry is still this arm: a lockout
    /// re-armed on the same ship is never removed by an older timer.
    fn expire_super_weapon_lockout(&mut self, ship: i64, immobile_until: i64, jump_cloak_until: i64) {
        let now = self.ms;
        let dead = self.super_weapons.lockouts.get(&ship).is_some_and(|entry|
            entry.immobile_until == immobile_until && entry.jump_cloak_until == jump_cloak_until
                && entry.immobile_until <= now && entry.jump_cloak_until <= now);
        if dead { self.super_weapons.lockouts.remove(&ship); }
    }

    /// Ball-departure cleanup for one ship's lockout. Live pulse jobs retire
    /// themselves at their next tick through `firing_valid` (source gone).
    pub(crate) fn super_weapon_ship_left(&mut self, ship: i64) {
        self.super_weapons.lockouts.remove(&ship);
    }

    /// Every ball the frozen shape currently hits; the source is never a
    /// victim. Victim identity and membership generation are captured in the
    /// same turn and rechecked by the damage pipeline before each plan.
    pub(super) fn super_weapon_victims(&self, spec: &FiringSpec) -> Vec<(i64, Option<i64>, Option<u64>, bool, Option<(i64, f64)>)> {
        let mut ids = Vec::new();
        match spec.aim {
            Aim::Target { id, ball_generation } => {
                if self.combat_membership_generation(id) == Some(ball_generation) {
                    if let Some(ball) = self.ball(id) {
                        return vec![(id, ball.char_id, Some(ball_generation), spec.illegal, None)];
                    }
                }
                return Vec::new();
            }
            Aim::Cylinder { origin, direction } => {
                let end = add(origin, scale(direction, spec.range));
                for ball in self.balls() {
                    if ball.id == spec.ship { continue; }
                    let Some(pos) = self.sample_position(ball.id) else { continue; };
                    if distance_to_segment(origin, end, pos) <= spec.radius { ids.push(ball.id); }
                }
            }
            Aim::Cone { origin, direction, cos_half } => {
                for ball in self.balls() {
                    if ball.id == spec.ship { continue; }
                    let Some(pos) = self.sample_position(ball.id) else { continue; };
                    let v = sub(pos, origin);
                    let d2 = dot(v, v);
                    if d2 > spec.range * spec.range { continue; }
                    if d2 > 0.0 && dot(v, direction) / d2.sqrt() < cos_half { continue; }
                    ids.push(ball.id);
                }
            }
            Aim::Slash { origin, d0, d1, axis, angle } => {
                for ball in self.balls() {
                    if ball.id == spec.ship { continue; }
                    let Some(pos) = self.sample_position(ball.id) else { continue; };
                    let v = sub(pos, origin);
                    let radial = dot(v, v).sqrt();
                    if radial > spec.range { continue; }
                    let off_plane = dot(v, axis);
                    // Signed sweep-plane angle of the victim's in-plane
                    // direction, from `d0` rotating toward `d1` around `axis`.
                    let in_plane = sub(v, scale(axis, off_plane));
                    let sweep = if dot(in_plane, in_plane) <= 0.0 { 0.0 } else {
                        let u = norm(in_plane);
                        let angle = dot(d0, u).clamp(-1.0, 1.0).acos();
                        if dot(cross(d0, u), axis) < 0.0 { -angle } else { angle }
                    };
                    // Inside the swept fan: within beam thickness of the
                    // sweep plane and within the arc's angular span.
                    let in_sweep = off_plane.abs() <= spec.radius && (0.0..=angle).contains(&sweep);
                    // Near an arc edge the beam's end cylinder still covers.
                    let in_cap = in_sweep || [d0, d1].iter().any(|&d| {
                        distance_to_segment(origin, add(origin, scale(d, spec.range)), pos) <= spec.radius
                    });
                    if in_sweep || in_cap { ids.push(ball.id); }
                }
            }
        }
        ids.iter().map(|&id| (id, self.ball(id).and_then(|ball| ball.char_id), self.combat_membership_generation(id), spec.illegal, None)).collect()
    }

    /// One damage pulse: prepare a facts attempt, evaluate the hit set and
    /// run the shared actor batch. Loss receipts and hit reports are handed
    /// to the system worker; nothing here touches the database.
    fn super_weapon_hit(&mut self, spec: &FiringSpec) {
        let mut victims = self.super_weapon_victims(spec);
        let Some(world) = self.combat_world() else { return };
        let Some(sde) = self.projection_sde.clone() else { return };
        let db = world.db().clone();
        let system = self.id;
        let Some(sequence) = self.super_weapons.firings.get_mut(&spec.master).map(|firing| {
            let sequence = firing.next_sequence;
            firing.next_sequence = sequence.saturating_add(1);
            sequence
        }) else { return };
        let check = spec.clone();
        let facts = match self.prepare_combat_source(spec.ship, Some(spec.master), Some(spec.effect), Some(spec.weapon_type),
            crate::lua::events::resources::FactOrigin::Natural, spec.combat_generation, sequence, 1,
            move |s| firing_valid(s, &check)) {
            Ok(facts) => facts,
            Err(_) => return,
        };
        if victims.is_empty() {
            self.finish_combat_attempt(&facts);
            return;
        }
        let completion = crate::services::combat::damage::apply_area_damage_in_actor(
            self, &db, system, spec.weapon_type, &mut victims,
            DamageApplication::Signature { raw: spec.damage, signature_radius: spec.signature_radius },
            facts, &sde, true);
        if !completion.attempt_finished { self.finish_combat_attempt(&facts); }
        if let Some(fatal) = completion.fatal {
            tracing::warn!(system, "superweapon pulse refused: {fatal:?}");
            return;
        }
        let (ship, character, weapon) = (spec.ship, spec.character, spec.weapon_type);
        let (receipts, first_error) = (completion.receipts, completion.first_error);
        let reports: Vec<_> = victims.into_iter().filter(|entry| entry.4.is_some()).collect();
        if !receipts.is_empty() || !reports.is_empty() || first_error.is_some() {
            self.defer(async move {
                for receipt in receipts { let _ = receipt.await; }
                for (target, pilot, _, _, report) in reports {
                    if let Some((quality, dealt)) = report {
                        crate::services::effects::weapons::report_hit(&world, ship, target, Some(character), pilot, weapon, dealt, quality, false);
                    }
                }
                if let Some(error) = first_error {
                    tracing::warn!(system, "superweapon pulse completion failed: {error:?}");
                }
            });
        }
    }

    /// The self-rearming pulse timer: applies every pulse due since the last
    /// run (a late tick applies all missed indices), then reschedules the
    /// next cadence boundary or retires the firing at the window's end.
    fn super_weapon_pulse(&mut self, master: i64) {
        let Some(firing) = self.super_weapons.firings.get(&master) else { return };
        let spec = firing.spec.clone();
        if !firing_valid(self, &spec) {
            self.end_super_weapon_firing(master, &spec.generation);
            return;
        }
        let now = self.ms;
        let window_start = firing.window_start;
        let window_end = firing.window_end;
        let latest = if spec.single { 0 }
            else { (((now.min(window_end) - window_start).max(0) as f64) / spec.cycle_ms.max(1.0)).floor() as i64 };
        let mut applied_to = firing.last_pulse;
        for index in applied_to + 1..=latest.max(-1) {
            self.super_weapon_hit(&spec);
            applied_to = index;
        }
        let Some(firing) = self.super_weapons.firings.get_mut(&master) else { return };
        if !std::sync::Arc::ptr_eq(&firing.spec.generation, &spec.generation) { return; }
        firing.last_pulse = applied_to;
        let next_due = window_start.saturating_add(((applied_to + 1) as f64 * spec.cycle_ms.max(1.0)) as i64);
        if spec.single || next_due > window_end {
            self.end_super_weapon_firing(master, &spec.generation);
        } else {
            self.schedule_at(next_due.max(now + 1), move |s| s.super_weapon_pulse(master));
        }
    }
}
