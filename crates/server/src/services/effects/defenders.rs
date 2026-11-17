//! Player Defender admission and deterministic bomb-target policy.
//!
//! The fitted launcher remains the SDE `useMissiles` effect (101); charge effect
//! `defenderMissileLaunching` (103) is content evidence only. The client
//! `pointDefense.py` supplies visuals, while the modern reference Defender
//! behaviour supplies the target rule plus friendship and ordering: a Defender
//! selects only the nearest hostile bomb whose remaining blast path crosses its
//! own source ship inside the Defender's flight reach. Ordinary missiles are
//! never targets. No player hit chance is evidenced, so a valid flight ends the
//! bomb at the actor-owned five-metre surface terminal without detonating it.
use super::lifecycle::Module;
use super::parameters::MissileInputs;
use super::space_source::PreparedCombatAttempt;
use crate::services::combat::Ctx;
use crate::services::effects::weapons;
use crate::services::CallError;
use crate::space::combat::loaded::LoadedPermit;
use crate::space::system::observers::BallToken;

use std::collections::HashSet;

use infinity_core::consts::dogma::{EFFECT_DEFENDER_MISSILE_LAUNCHING, EFFECT_USE_MISSILES};

/// Release an admitted loaded-use permit without consuming its charge.
pub(crate) fn release_permit(cx: &Ctx, permit: Option<LoadedPermit>) {
    if let Some(permit) = permit {
        cx.world.post(cx.system_id, move |s| s.release_loaded_use(permit));
    }
}

/// Reserve one bomb target in the source actor before its charge is consumed.
pub(crate) async fn reserve_target(cx: &Ctx, reach_m: f64) -> Option<(i64, BallToken)> {
    let source_id = cx.ship_id;
    cx.world.with(cx.system_id, move |s| {
        s.reserve_defender_target(source_id, reach_m)
    }).await.flatten()
}
/// Release a target reservation after a launch refusal.
pub(crate) async fn release_target(cx: &Ctx, target_id: i64) {
    let _ = cx.world.with(cx.system_id, move |s| {
        s.release_defender_target(target_id);
    }).await;
}


/// Client inventorycommon/const.py `groupMissileLauncherDefender`.
pub(crate) const GROUP_MISSILE_LAUNCHER_DEFENDER: i32 = 512;
/// Client inventorycommon/const.py `groupDefenderMissile`.
pub(crate) const GROUP_DEFENDER_MISSILE: i32 = 88;
/// Surface gap from the predecessor modern Defender behavior reference.
pub(crate) const DEFENDER_INTERCEPT_SURFACE_METRES: f64 = 5.0;

/// The launch-time identity of a bomb or a Defender source, captured once
/// from the owner's online session. Missing alliance, faction or fleet
/// membership means the pilot has none, not that it is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProjectileIdentity {
    pub owner: i64,
    pub corporation: i64,
    pub alliance: Option<i64>,
    pub war_faction: Option<i64>,
    pub fleet: Option<i64>,
}

impl ProjectileIdentity {
    /// Friendship is same pilot, corporation, or one shared membership:
    /// alliance, war faction or fleet. A membership both sides lack is no bond.
    pub(crate) fn friends(self, other: Self) -> bool {
        self.owner == other.owner
            || self.corporation == other.corporation
            || [self.alliance, self.war_faction, self.fleet].into_iter()
                .zip([other.alliance, other.war_faction, other.fleet])
                .any(|(left, right)| left.is_some() && left == right)
    }
}

/// A candidate's actor-local selection facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Candidate {
    pub id: i64,
    pub surface_distance: f64,
    pub expiry_at_ms: i64,
    pub identity: ProjectileIdentity,
}


/// Admission requires the modern launcher group, Defender charge group and
/// charge effect proof. Missing static data is a refusal, never an inference.
pub(crate) fn is_defender_fit(module_group: Option<i32>, module_effect: i32, charge_group: Option<i32>,
    charge_effects: impl IntoIterator<Item = i32>) -> bool {
    module_group == Some(GROUP_MISSILE_LAUNCHER_DEFENDER)
        && module_effect == EFFECT_USE_MISSILES
        && charge_group == Some(GROUP_DEFENDER_MISSILE)
        && charge_effects.into_iter().any(|effect| effect == EFFECT_DEFENDER_MISSILE_LAUNCHING)
}

pub(super) async fn start(cx: &Ctx, inputs: &MissileInputs, module: &Module, charge: i32, target_id: i64, target_token: BallToken,
    attempt: PreparedCombatAttempt) -> Result<(), CallError> {
    weapons::launch_defender(cx, inputs, module, charge, target_id, target_token, attempt).await
}

/// Filter friendly, reserved and non-finite or negative-distance candidates
/// and apply the required total ordering: surface distance, expiry, then
/// numeric ball ID.
pub(crate) fn choose_candidate(source: ProjectileIdentity, candidates: impl IntoIterator<Item = Candidate>, reserved: &HashSet<i64>) -> Option<Candidate> {
    let mut eligible: Vec<_> = candidates.into_iter().filter(|candidate| !source.friends(candidate.identity)
        && !reserved.contains(&candidate.id)
        && candidate.surface_distance.is_finite() && candidate.surface_distance >= 0.0)
        .collect();
    eligible.sort_by(|left, right| left.surface_distance.total_cmp(&right.surface_distance)
        .then_with(|| left.expiry_at_ms.cmp(&right.expiry_at_ms))
        .then_with(|| left.id.cmp(&right.id)));
    eligible.into_iter().next()
}

/// Whether the straight remaining blast path `[bomb, endpoint]` passes close
/// enough to `source` that its blast reaches the ship: the point-to-segment
/// distance must fit the blast range plus both surface radii. Non-finite
/// input is no threat.
pub(crate) fn bomb_threatens(source: crate::space::V3, source_radius: f64,
    bomb: crate::space::V3, bomb_radius: f64, endpoint: crate::space::V3, blast_range: f64) -> bool {
    use crate::space::{add, dist, dot, scale, sub};
    if !(source_radius.is_finite() && bomb_radius.is_finite() && blast_range.is_finite() && blast_range >= 0.0)
        || ![source, bomb, endpoint].into_iter().flatten().all(|v| v.is_finite()) { return false; }
    let segment = sub(endpoint, bomb);
    let length_sq = dot(segment, segment);
    let fraction = if length_sq <= 0.0 { 0.0 } else {
        (dot(sub(source, bomb), segment) / length_sq).clamp(0.0, 1.0)
    };
    let reach = blast_range + source_radius.max(0.0) + bomb_radius.max(0.0);
    dist(source, add(bomb, scale(segment, fraction))) <= reach + 1e-6
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
