//! Capacitor effects for NPC and drone sources.
use super::{capacitor::nos_allowed, range::range_multiplier, registry::{EffectAttribute, EffectFlag},
    space_source::{SourceContext, SourceGuard}, EffectKind};
use infinity_core::consts::{dogma::ATTR_ENERGY_NEUTRALIZER_AMOUNT, dogma::ATTR_NOS_OVERRIDE, dogma::ATTR_POWER_TRANSFER_AMOUNT};
use crate::services::{combat::groups_health::{capacitor_snapshot, drain_exact_guarded, inject_guarded, npc_capacitor_fraction, NpcCapacitorGuard}, CallError};
use crate::space::{dist, drone::DronePhase};

fn scaled_amount(amount: f64, distance: f64, optimal: f64, falloff: f64) -> f64 {
    if !amount.is_finite() { return 0.0; }
    amount.max(0.0) * range_multiplier(distance, optimal, falloff)
}
pub(crate) fn in_range(distance: f64, optimal: f64, falloff: f64) -> bool {
    range_multiplier(distance, optimal, falloff) > 0.0
}

pub(crate) async fn apply_source(cx: &SourceContext<'_>) -> Result<(), CallError> {
    if !matches!(cx.phase, DronePhase::Begin) { return Ok(()); }
    let value = |id| cx.fit.value(cx.key, id);
    let optimal = cx.effect.attribute(EffectAttribute::Range).map(value).unwrap_or(0.0);
    let falloff = cx.effect.attribute(EffectAttribute::Falloff).map(value).unwrap_or(0.0);
    let distance = (dist(cx.source.pos, cx.target.pos) - f64::from(cx.source.radius) - f64::from(cx.target.radius)).max(0.0);
    let guard = NpcCapacitorGuard::Attack {
        source: cx.source.id, target: cx.target.id, source_guard: Some(cx.guard), player: None,
        optimal, falloff, offensive: cx.effect.has(EffectFlag::Offensive),
    };
    let mut admitted_owner = None;
    match cx.effect.kind {
        EffectKind::Neutralizer => {
            let amount = scaled_amount(value(ATTR_ENERGY_NEUTRALIZER_AMOUNT), distance, optimal, falloff);
            admitted_owner = drain_exact_guarded(cx.db, cx.world, cx.system, cx.target.id, amount, guard).await?;
        }
        EffectKind::Nosferatu => {
            let Some(source) = capacitor_snapshot(cx.db, cx.world, cx.system, cx.source.id).await? else { return Ok(()) };
            let Some(target) = capacitor_snapshot(cx.db, cx.world, cx.system, cx.target.id).await? else { return Ok(()) };
            if nos_allowed(source.level, source.capacity, target.level, target.capacity, value(ATTR_NOS_OVERRIDE) > 0.0) {
                let amount = scaled_amount(value(ATTR_POWER_TRANSFER_AMOUNT), distance, optimal, falloff);
                if let Some((drained, owner)) = drain_exact_guarded(cx.db, cx.world, cx.system, cx.target.id, amount, guard.clone()).await? {
                    admitted_owner = Some((drained, owner));
                    inject_guarded(cx.db, cx.world, cx.system, cx.source.id, drained, guard).await?;
                }
            }
        }
        EffectKind::CapTransfer => {
            let (target, distance, guard) = match cx.guard {
                SourceGuard::Npc { source, target: hostile, generation } => {
                    let sde = cx.world.sde().clone();
                    let Some((source_ball, target)) = cx.world.with(cx.system, move |s| {
                        if !s.npc_projection_valid(source, hostile, generation) { return None; }
                        let target = s.assist_target(source, |me, candidate| {
                            let distance = (dist(me.pos, candidate.pos) - f64::from(me.radius) - f64::from(candidate.radius)).max(0.0);
                            if !in_range(distance, optimal, falloff) { return None; }
                            npc_capacitor_fraction(&sde, candidate).filter(|fraction| *fraction < 1.0)
                        })?;
                        Some((s.sampled_ball(source)?, target))
                    }).await.flatten() else { return Ok(()) };
                    let distance = (dist(source_ball.pos, target.pos) - f64::from(source_ball.radius) - f64::from(target.radius)).max(0.0);
                    (target.id, distance, NpcCapacitorGuard::Assistance { source, hostile, target: target.id, generation, optimal, falloff })
                }
                SourceGuard::Drone { source, target, generation } => {
                    let Some(owner) = cx.attacker else { return Ok(()) };
                    (target, distance, NpcCapacitorGuard::DroneAssistance { source, target, generation, optimal, falloff, owner })
                }
            };
            let amount = scaled_amount(value(ATTR_POWER_TRANSFER_AMOUNT), distance, optimal, falloff);
            inject_guarded(cx.db, cx.world, cx.system, target, amount, guard).await?;
        }
        _ => unreachable!("not a capacitor effect"),
    }
    if cx.effect.has(EffectFlag::Offensive) {
        if let (Some(attacker), Some((_, Some(victim)))) = (cx.attacker, admitted_owner) {
            crate::services::crimewatch::aggress(cx.db, cx.world, cx.system, attacker, victim).await?;
        }
    }
    Ok(())
}

#[cfg(test)] mod tests;
