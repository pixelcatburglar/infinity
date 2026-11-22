//! In-actor area damage for a sampled victim set and its shared source attempt.
//! Fixed, Vorton and signature-scaled inputs use loaded SDE data and actor state.
//! This module returns loss receipts; callers await them outside the actor.
use super::{complete_damage_outcome, DamageSource};
use crate::{db::Db, services::CallError};

/// The result of one in-actor area batch: `attempt_finished` marks that
/// `apply_area_damage` ran and finished the source attempt on every path it
/// takes; `fatal` is a refusal that aborts the batch; `first_error` keeps the
/// first per-target completion failure while the rest still apply. Loss
/// receipts belong to the caller's worker and must be awaited outside the
/// actor.
pub(crate) struct AreaCompletion {
    pub(crate) attempt_finished: bool,
    pub(crate) fatal: Option<CallError>,
    pub(crate) receipts: Vec<tokio::sync::oneshot::Receiver<Result<(), CallError>>>,
    pub(crate) first_error: Option<CallError>,
}

/// Shared in-actor area apply/finish for one sampled victim batch
/// `(target, pilot, membership, illegal, report)`. `valid_source` admits the
/// batch; a declined batch clears `victims` so no hit report is emitted.
/// `application` selects each accepted victim's raw vector: a fixed typed
/// vector for every victim, the Vorton missile formula sampled per target,
/// or a fitted weapon signature scale.
/// Damageable projectiles in range take the projectile path after source
/// admission and never enter ship plans. The caller keeps ownership of any
/// attempt token: this helper finishes the `facts` attempt only through
/// `admit_area_source`/`apply_area_damage`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_area_damage_in_actor(
    s: &mut crate::space::System, db: &Db, system: i64, weapon: i32,
    victims: &mut Vec<(i64, Option<i64>, Option<u64>, bool, Option<(i64, f64)>)>,
    application: crate::services::effects::aoe::DamageApplication,
    facts: crate::space::combat_facts::CombatSource,
    sde: &crate::sde::Sde, valid_source: bool,
) -> AreaCompletion {
    use crate::services::effects::aoe::DamageApplication;
    let ship = facts.entity.item_id;
    let attacker = facts.actual_actor_character;
    let mut completion = AreaCompletion { attempt_finished: false, fatal: None, receipts: Vec::new(), first_error: None };
    if !valid_source {
        victims.clear();
        return completion;
    }
    if let Err(error) = s.observe_combat_utc() {
        completion.fatal = Some(match error {
            crate::space::combat_facts::Refusal::Full => super::super::capacity::refusal(system),
            other => CallError::Internal(anyhow::anyhow!("area boundary refusal: {other:?}")),
        });
        return completion;
    }
    let at = s.combat_observed_time();
    victims.sort_unstable_by_key(|entry| entry.0);
    let mut targets = Vec::with_capacity(victims.len());
    let mut projectiles = Vec::new();
    for (target, pilot, membership, illegal, report) in victims.iter_mut() {
        let (target, pilot, membership, illegal) = (*target, *pilot, *membership, *illegal);
        if s.projectile_damageable(target) {
            let base = match application {
                DamageApplication::Fixed(raw) => raw,
                DamageApplication::Vorton(inputs) => inputs.raw,
                DamageApplication::Signature { raw, .. } => raw,
            };
            projectiles.push((target, base));
            continue;
        }
        if !s.ball(target).is_some_and(|ball| ball.char_id == pilot)
            || s.combat_membership_generation(target) != membership { continue; }
        if s.is_damage_immune(target) { continue; }
        let raw = match application {
            DamageApplication::Fixed(raw) => {
                *report = Some((crate::services::effects::weapons::hit_quality(1.0), 0.0));
                raw
            }
            DamageApplication::Vorton(inputs) => {
                let Some(victim) = s.sampled_ball(target) else { continue };
                *report = Some((crate::services::effects::weapons::hit_quality(1.0), 0.0));
                crate::services::effects::vorton::vorton_application(inputs.raw,
                    crate::services::effects::weapons::target_signature(sde, &victim),
                    crate::space::dist(victim.vel, [0.0; 3]), inputs.aoe_cloud, inputs.aoe_velocity, inputs.drf)
            }
            DamageApplication::Signature { raw, signature_radius } => {
                let factor = if signature_radius > 0.0 {
                    let Some(victim) = s.sampled_ball(target) else { continue };
                    (crate::services::effects::weapons::target_signature(sde, &victim) / signature_radius).clamp(0.0, 1.0)
                } else { 1.0 };
                if factor <= 0.0 { continue; }
                *report = Some((crate::services::effects::weapons::hit_quality(1.0), 0.0));
                raw.map(|value| value * factor)
            }
        };
        let source = DamageSource { attacker_id: attacker, attacker_ball_id: Some(ship),
            source_ship_id: Some(ship), source_ship_type: Some(facts.entity.type_id),
            weapon_type_id: Some(weapon), facts: Some(facts), force_origin: None, illegal };
        match s.plan_area_damage(target, raw, source, at) {
            Some(plan) => targets.push(plan),
            None if matches!(s.combat_phase(target), Some(crate::space::combat::CombatPhase::Live)) => {
                completion.fatal = Some(CallError::Internal(anyhow::anyhow!("area damage plan failed for live target")));
                return completion;
            }
            None => { *report = None; }
        }
    }
    targets.sort_unstable_by_key(|target| (target.target.item_id, target.target.incarnation));
    let applied = if targets.is_empty() { s.admit_area_source(facts, &mut [], at, |_| valid_source) }
        else { s.apply_area_damage(facts, &mut targets, at, |_| valid_source) };
    completion.attempt_finished = true;
    match applied {
        Ok(false) => return completion,
        Err(crate::space::combat_facts::Refusal::Full) => {
            completion.fatal = Some(super::super::capacity::refusal(system));
            return completion;
        }
        Err(crate::space::combat_facts::Refusal::Replay | crate::space::combat_facts::Refusal::Busy) => {
            victims.clear();
            return completion;
        }
        Err(error) => {
            completion.fatal = Some(CallError::Internal(anyhow::anyhow!("area combat refusal: {error:?}")));
            return completion;
        }
        Ok(true) => {}
    }
    for (target, projectile_raw) in projectiles { s.apply_projectile_damage(target, projectile_raw); }
    for target in &mut targets {
        let Some(outcome) = target.payload.outcome.take() else { continue };
        if let Ok(index) = victims.binary_search_by_key(&target.target.item_id, |entry| entry.0) {
            if let Some((_, dealt)) = victims[index].4.as_mut() { *dealt = outcome.dealt; }
        }
        match complete_damage_outcome(s, db, system, target.target.item_id, outcome) {
            Ok((_, Some(receipt))) => completion.receipts.push(receipt),
            Ok((_, None)) => {},
            Err(error) => if completion.first_error.is_none() { completion.first_error = Some(error); },
        }
    }
    completion
}
