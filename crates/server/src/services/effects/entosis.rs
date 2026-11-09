//! Entosis admission and paid-cycle publication for the shared dogma lifecycle.
//! The client identifies effect 6063 in dogma/const.py; corporation and source
//! generations come from the sovereignty DAOs under canonical admission locks.
//! Only neutral Hub capture is supported; owned campaigns are not implemented.
use super::{behaviors::Activation, lifecycle::Cycle, registry::EffectAttribute};
use infinity_core::messages;
use crate::{
    client::filetime_from_unix_ms,
    dao::{characters::ship, corporations::registry, sovereignty::sources},
    db::outbox::Outbox,
    services::{combat::{Ctx, targeting_charges::has_target}, db_err, fit::{Fit, Key}, notify_error, CallError, sovereignty::CaptureContributor},
    space::{sovereignty::links::{self, LinkCycle}, MODE_WARP},
};

fn refused() -> CallError { notify_error(messages::effects::ENTOSIS_UNAVAILABLE) }

fn valid_parameters(duration: f64, range: f64) -> Option<(f64, f64)> {
    (duration.is_finite() && duration > 0.0 && range.is_finite() && range >= 0.0).then_some((duration, range))
}

// CCP's Entosis Link article specifies capital duration; SDE passive ship
// modifiers are already evaluated by Fit, so no additional multiplier is applied.
fn parameters(fit: &Fit, effect: &super::registry::Effect, module: i64) -> Option<(f64, f64)> {
    let key = Key::Module(module);
    valid_parameters(fit.value(key, effect.attribute(EffectAttribute::Duration)?),
        fit.value(key, effect.attribute(EffectAttribute::Range)?))
}
fn next_cycle(proof: &mut LinkCycle, paid_cycles: i64, range: f64) {
    proof.range_m = range;
    proof.scoring = paid_cycles > 0;
    proof.expires_sim = 0;
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, activation: &Activation) -> Result<(), CallError> {
    let target = activation.target.filter(|target| has_target(cx.char_id, *target)).ok_or_else(refused)?;
    let (_, range) = parameters(fit, &activation.effect, activation.module.id).ok_or_else(refused)?;
    let (character, ship) = (cx.char_id, cx.ship_id);
    let valid = cx.world.with(cx.system_id, move |s| {
        if links::admission(s, character, ship, target).is_none() { return false; }
        let Some((source, victim)) = s.sampled_ball(ship).zip(s.sampled_ball(target)) else { return false };
        let gap = crate::space::dist(source.pos, victim.pos) - f64::from(source.radius) - f64::from(victim.radius);
        source.mode != MODE_WARP && victim.mode != MODE_WARP && gap.is_finite() && gap.max(0.0) <= range
    }).await.unwrap_or(false);
    if valid { Ok(()) } else { Err(refused()) }
}

/// Capture one source generation under the same organization guard as invalidations.
pub(super) async fn prepare(cx: &Ctx, fit: &Fit, cycle: &Cycle) -> Result<LinkCycle, CallError> {
    let (_, range) = parameters(fit, &cycle.effect, cycle.modules[0].id).ok_or_else(refused)?;
    let target = cycle.target.filter(|target| has_target(cx.char_id, *target)).ok_or_else(refused)?;
    let (character, ship, module) = (cx.char_id, cx.ship_id, cycle.modules[0].id);
    let activation = cycle.stop.clone();
    let mut proof = if cycle.cycles != 0 {
        let generation = activation.clone();
        cx.world.with(cx.system_id, move |s| links::cycle(s, module, &generation)).await.flatten().ok_or_else(refused)?
    } else {
        let (epoch, membership_generation) = cx.world.with(cx.system_id, move |s| links::admission(s, character, ship, target)).await.flatten().ok_or_else(refused)?;
        let location = ship::current(&cx.db, character).await.map_err(db_err)?.ok_or_else(refused)?;
        let alliance = location.alliance.ok_or_else(refused)?;
        let corporation = location.corporation;
        let system = cx.system_id;
        let contributor = cx.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
            if !registry::lock_admission(tx, &[alliance], &[corporation], &[character]).await.map_err(db_err)? { return Err(refused()); }
            let current = ship::current(tx, character).await.map_err(db_err)?.ok_or_else(refused)?;
            if current.corporation != corporation || current.alliance != Some(alliance) || current.station.is_some()
                || current.system != Some(system) || current.ship != Some(ship) { return Err(refused()); }
            if !crate::dao::alliances::exists(tx, alliance).await.map_err(db_err)? { return Err(refused()); }
            let version = sources::ensure(tx, character).await.map_err(db_err)?;
            Ok(CaptureContributor { character_id: character, ship_id: ship, module_id: module, corporation_id: corporation, alliance_id: alliance, source_revision: version.revision })
        }), |contributor, _| async move { Ok(contributor) }).await?;
        LinkCycle { target_item_id: target, target_epoch: epoch, contributor, membership_generation, activation, range_m: range, scoring: false, expires_sim: 0 }
    };
    next_cycle(&mut proof, cycle.cycles, range);
    let check = proof.clone();
    if !cx.world.with(cx.system_id, move |s| links::valid_paid(s, &check)).await.unwrap_or(false) {
        return Err(refused());
    }
    Ok(proof)
}

pub(super) async fn begin(cx: &Ctx, mut proof: LinkCycle, duration_ms: f64) -> Option<i64> {
    cx.world.with(cx.system_id, move |s| {
        let start = s.now_ms();
        proof.expires_sim = filetime_from_unix_ms(super::lifecycle::cycle_deadline(start, duration_ms));
        links::paid_cycle(s, proof).then_some(start)
    }).await.flatten()
}

/// Publish only while the admitted generation is still valid in the actor.
pub(super) async fn publish_start(cx: &Ctx, cycle: &Cycle) -> bool {
    let cx = cx.clone();
    let world = cx.world.clone();
    let module = cycle.modules[0].clone();
    let generation = cycle.stop.clone();
    let effect = cycle.effect.clone();
    let (target, other, started, duration, repeat, charge) =
        (cycle.target, cycle.other.clone(), cycle.started, cycle.duration_ms, cycle.repeat, cycle.charge_type);
    world.with(cx.system_id, move |s| {
        let Some(link) = links::cycle(s, module.id, &generation) else { return false };
        if link.expires_sim <= filetime_from_unix_ms(s.now_ms()) || !links::valid_paid(s, &link) {
            return false;
        }
        let current = crate::services::combat::state().active.get(&module.id)
            .is_some_and(|active| std::sync::Arc::ptr_eq(&active.stop, &generation) && !active.forced_inactive);
        if !current { return false; }
        super::notify::ship_effect(&cx, module.id, effect.id, target, other, true, started, duration, repeat);
        if let Some(guid) = effect.guid.as_deref().and_then(|guid| guid.split(',').next()) {
            let event = super::notify::fire_fx(s.current_time(), cx.ship_id, Some((module.id, module.type_id)),
                guid, target, charge, effect.has(super::registry::EffectFlag::Offensive), true, duration, repeat);
            s.broadcast(cx.ship_id, vec![event]);
        }
        true
    }).await.unwrap_or(false)
}

pub(super) fn stop(s: &mut crate::space::System, cycle: &Cycle) {
    links::stop(s, cycle.modules[0].id, &cycle.stop);
}

#[cfg(test)] mod tests;
