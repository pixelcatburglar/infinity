//! Grouped player turret banks: one master roll, summed typed raw damage, one
//! application and one `isBanked` report per observer. Members consume and
//! admit individually with the real member as replay owner; the first
//! admitted attempt carries the volley under master attribution. A candidate
//! whose consumed charge, direct-damage kind or roll inputs differ from the
//! captured master selects the per-member fallback instead.
//! For a normal banked volley any capacity refusal cancels the whole round:
//! every armed token closes once, no damage or report, and the canonical
//! refusal returns through the normal cycle error path. Whole source or
//! target invalidation cancels the same way without a replacement ghost. The
//! mismatch fallback keeps per-member semantics instead: a later member's
//! refusal still stops the rest and returns the canonical error, but damage
//! and reports earlier members already committed stand.
use super::{consume_charge, fitted_source_current, fitted_target_current, hit_quality, report_hit, target_signature, turret_chance, turret_geometry, turret_roll};
use crate::db::Db;
use crate::services::CallError;
use crate::services::combat::Ctx;
use crate::services::combat::damage::{apply_damage_in_actor, DamageSource};
use crate::services::combat::targeting_charges::targets_of;
use crate::services::effects::fx::weapon_fx;
use crate::services::effects::lifecycle::{Cycle, CycleOutcome};
use crate::services::effects::lifecycle::combat_source::prepare_turret_member;
use crate::services::effects::parameters::{MemberInputs, MemberPacket, MemberShot, TurretRound};
use crate::sde::Sde;
use crate::space::{System, system::presentation::FxRetention};
use crate::space::combat::CombatPhase;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tracing::{debug, warn};

const ROLL_FIELDS: [&str; 4] = ["optimal", "falloff", "tracking", "signature"];

/// Frozen source identity for one volley; no fit or member collection.
#[derive(Clone)]
struct BankContext {
    ship: i64,
    pilot: i64,
    system: i64,
    master: i64,
    weapon: i32,
    illegal: bool,
    /// Membership admitted at the cycle's first capture, or absent.
    source_membership: Option<u64>,
    /// Target's combat record generation frozen before consumption.
    target_generation: u64,
    stop: Arc<AtomicBool>,
}

/// How one volley round ended inside the actor turn.
enum BankResolution {
    /// Armed members resolved; their stored shots report outside the actor.
    Resolved,
    /// The whole source lost validity; canceled without damage or report.
    SourceLost,
    /// The target left, died, lost its lock or was replaced; canceled the
    /// same way and never fires a ghost shot at a reused identity.
    TargetLost,
}
/// The whole-bank source check inside the actor; the predicate itself is
/// shared with the singleton shot path in `weapons`.
fn source_current(s: &System, context: &BankContext) -> bool {
    fitted_source_current(s, context.ship, context.master, context.pilot, &context.stop, context.source_membership)
}

/// The whole-round target check through the shared predicate.
fn target_current(s: &System, context: &BankContext, target: i64) -> bool {
    fitted_target_current(s, context.pilot, target, context.target_generation)
}

/// Whether the shared actor still owns a valid source: an admission failure
/// is a whole-round cancel when false, a per-member exclusion when true.
async fn shared_source_valid(cx: &Ctx, context: &BankContext) -> bool {
    let context = context.clone();
    cx.world.with(context.system, move |s| source_current(s, &context)).await.unwrap_or(false)
}

/// Drop every round slot; armed attempts post their finish through `Drop`.
fn clear_round(members: &mut [MemberInputs]) {
    for member in members {
        member.turret_round = None;
    }
}

/// Execute one grouped turret volley for `cycle`'s whole bank.
pub(in crate::services::effects) async fn start(cx: &Ctx, cycle: &mut Cycle, target: i64)
    -> Result<CycleOutcome, CallError>
{
    start_with_roll(cx, cycle, target, turret_roll).await
}

/// The production volley core; the roll step is an injected plain call so
/// tests can count or steer every real member roll: one for a normal bank,
/// one per fired member for a fallback, without any global hook or counter.
async fn start_with_roll<R>(cx: &Ctx, cycle: &mut Cycle, target: i64, roll: R)
    -> Result<CycleOutcome, CallError>
where
    R: FnMut(f64) -> f64 + Send + 'static,
{
    let probe = BankContext {
        ship: cx.ship_id, pilot: cx.char_id, system: cx.system_id,
        master: cycle.modules[0].id, weapon: cycle.modules[0].type_id,
        illegal: cycle.illegal, source_membership: cycle.source_membership,
        target_generation: 0, stop: cycle.stop.clone(),
    };
    // Nothing is consumed until the whole source and target prove current.
    let checked = cx.world.with(cx.system_id, {
        let probe = probe.clone();
        move |s| {
            if !source_current(s, &probe) { return (false, None); }
            let admitted = s.combat_phase(target) == Some(CombatPhase::Live)
                && targets_of(probe.pilot).contains(&target);
            (true, admitted.then(|| s.combat_membership_generation(target)).flatten())
        }
    }).await;
    let generation = match checked {
        Some((true, Some(generation))) => generation,
        Some((true, None)) => return Ok(crate::services::effects::lifecycle::target_loss(cycle).await),
        _ => return Ok(CycleOutcome::Stop("activation lost")),
    };
    let context = BankContext { target_generation: generation, ..probe };

    // Consume once per member in module order; failures exclude the member.
    let mut candidates = 0usize;
    for index in 0..cycle.modules.len() {
        let module_id = cycle.modules[index].id;
        let Some(member) = cycle.inputs.members.get_mut(index) else {
            return Err(CallError::Internal(anyhow::anyhow!("turret bank cycle captured without member inputs")));
        };
        let MemberPacket::Turret(inputs) = &member.packet else {
            return Err(CallError::Internal(anyhow::anyhow!("turret bank member captured without turret inputs")));
        };
        let direct = inputs.direct_damage;
        let charge = if direct { None } else {
            let (permit, token) = member.take_charge();
            match consume_charge(cx, permit, token).await {
                Some(charge) => Some(charge),
                None => continue,
            }
        };
        if !direct && charge != member.charge_type {
            // The resident charge changed under the member; its captured raw
            // belongs to the old charge, so the member is excluded rather than
            // fired with a stale packet.
            debug!(module = module_id, consumed = ?charge, captured = ?member.charge_type,
                "turret bank member charge changed since capture; member excluded");
            continue;
        }
        if charge.is_some() { cycle.charge_type = charge; }
        member.turret_round = Some(TurretRound { charge, attempt: None, shot: None, spool_bonus: None });
        candidates += 1;
    }
    if candidates == 0 { return Ok(CycleOutcome::Stop("out of charges")); }

    // The master's captured packet is the roll reference even when the master
    // itself failed consumption; the charge baseline is the first consumed
    // candidate's charge, not an empty master's None.
    let MemberPacket::Turret(master_inputs) = &cycle.inputs.members[0].packet else {
        return Err(CallError::Internal(anyhow::anyhow!("turret bank master captured without turret inputs")));
    };
    let (master_direct, master_parameters) = (master_inputs.direct_damage, master_inputs.parameters);
    let master_roll = [master_parameters.0, master_parameters.1, master_parameters.2, master_parameters.3];
    let charge_baseline = cycle.inputs.members.iter()
        .find_map(|member| member.turret_round.as_ref().and_then(|round| round.charge));
    let mut fallback = false;
    for member in &cycle.inputs.members {
        let Some(round) = &member.turret_round else { continue };
        let MemberPacket::Turret(inputs) = &member.packet else { continue };
        if inputs.direct_damage != master_direct {
            debug!(member = member.module, master = context.master, field = "direct_damage",
                member_value = inputs.direct_damage, master_value = master_direct,
                "turret bank candidate differs from master; per-member fallback");
            fallback = true;
        }
        let member_roll = [inputs.parameters.0, inputs.parameters.1, inputs.parameters.2, inputs.parameters.3];
        for (index, field) in ROLL_FIELDS.iter().enumerate() {
            if member_roll[index] != master_roll[index] {
                debug!(member = member.module, master = context.master, field = *field,
                    member_value = member_roll[index], master_value = master_roll[index],
                    "turret bank candidate differs from master; per-member fallback");
                fallback = true;
            }
        }
        if round.charge != charge_baseline {
            debug!(member = member.module, master = context.master, field = "charge",
                member_value = ?round.charge, master_value = ?charge_baseline,
                "turret bank candidate differs from master; per-member fallback");
            fallback = true;
        }
    }

    // Admit each candidate once: the real member owns generation/sequence
    // while the facts attribute the master for a banked volley and the member
    // itself for a fallback.
    let mut prepared = 0usize;
    for index in 0..cycle.modules.len() {
        if cycle.inputs.members[index].turret_round.is_none() { continue; }
        let member_id = cycle.modules[index].id;
        match prepare_turret_member(cycle, member_id, !fallback).await {
            Ok(Some(attempt)) => {
                if let Some(round) = cycle.inputs.members[index].turret_round.as_mut() {
                    round.attempt = Some(attempt);
                }
                prepared += 1;
            }
            // The shared source failed its own guard: the whole round cancels,
            // not just this member.
            Ok(None) => { clear_round(&mut cycle.inputs.members); return Ok(CycleOutcome::Stop("activation lost")); }
            // Any capacity refusal cancels the whole round; the canonical
            // error reports and stops through the normal cycle path.
            Err(error) if crate::services::combat::capacity::is_refusal(&error) => {
                clear_round(&mut cycle.inputs.members);
                return Err(error);
            }
            // A shared actor or source failure cancels the round the same way
            // the source guard does; only a genuine per-member failure (for
            // example sequence exhaustion) excludes the member.
            Err(error) => {
                if !shared_source_valid(cx, &context).await {
                    clear_round(&mut cycle.inputs.members);
                    return Ok(CycleOutcome::Stop("activation lost"));
                }
                match error {
                    CallError::Internal(error) => warn!("bank member {member_id} admission failed: {error:#}"),
                    error => { clear_round(&mut cycle.inputs.members); return Err(error); }
                }
            }
        }
    }
    if prepared == 0 {
        clear_round(&mut cycle.inputs.members);
        return Ok(CycleOutcome::Stop("activation lost"));
    }

    match bank_resolve(cx, cycle, target, context, !fallback, roll).await? {
        BankResolution::Resolved => {}
        BankResolution::SourceLost => return Ok(CycleOutcome::Stop("activation lost")),
        BankResolution::TargetLost => return Ok(crate::services::effects::lifecycle::target_loss(cycle).await),
    }
    if cycle.cycles == 0 {
        // The shared graphicInfo dict rides the beam's one real start;
        // a repeating volley must not resend it and zero the client's ramp.
        let graphic_info = cycle.inputs.members.iter().find_map(|member| match &member.packet {
            MemberPacket::Turret(inputs) => inputs.spool.map(crate::services::effects::spool::graphic_info),
            _ => None,
        }).unwrap_or(marshal::Value::None);
        weapon_fx(cx, &cycle.modules, &cycle.effect, Some(target), cycle.charge_type, true,
            cycle.duration_ms, cycle.repeat, cycle.modules[0].id, &cycle.stop, FxRetention::Transient, graphic_info);
    }
    Ok(CycleOutcome::Continue)
}

/// Move the member backing into one actor turn, resolve the volley there and
/// report every stored shot after the backing returns: none for a canceled
/// banked round, earlier members for a canceled fallback. `Err` propagates
/// only after the backing is restored and every produced shot reported.
async fn bank_resolve<R>(cx: &Ctx, cycle: &mut Cycle, target: i64, context: BankContext, banked: bool, mut roll: R)
    -> Result<BankResolution, CallError>
where
    R: FnMut(f64) -> f64 + Send + 'static,
{
    // Each armed member's bonus-scaled raw is summed inside the final actor
    // turn, so the volley's ramp state cannot be read or spent ahead of it.
    let actor_context = context.clone();
    let db = cx.db.clone();
    let sde = cx.world.sde().clone();
    let mut members = std::mem::take(&mut cycle.inputs.members);
    let Some((returned, result)) = cx.world.with(actor_context.system, move |s| {
        let result = resolve_in_actor(s, &db, &sde, &actor_context, target, banked, &mut members, &mut roll);
        (members, result)
    }).await else { return Ok(BankResolution::SourceLost) };
    let (ship, weapon) = (cx.ship_id, context.weapon);
    cycle.inputs.members = returned;
    let (resolution, error) = match result {
        Err(error @ CallError::User { .. }) => (BankResolution::Resolved, Some(error)),
        Err(CallError::Internal(error)) => { warn!("bank volley failed: {error:?}"); (BankResolution::Resolved, None) }
        Ok(resolution) => (resolution, None),
    };
    // Report every stored shot: one banked report for a normal volley, or a
    // fallback's own reports, including the members that already resolved
    // before a later member's refusal canceled the round.
    for index in 0..cycle.modules.len() {
        let Some(round) = cycle.inputs.members[index].turret_round.take() else { continue };
        let Some(mut shot) = round.shot else { continue };
        if let Some(receipt) = shot.receipt.take() {
            receipt.await.map_err(|_| CallError::Internal(anyhow::anyhow!("combat loss worker stopped")))??;
        }
        let weapon = if banked { weapon } else { cycle.modules[index].type_id };
        report_hit(&cx.world, ship, target, shot.shooter, shot.victim, weapon, shot.dealt,
            hit_quality(shot.multiplier), banked);
    }
    match error { Some(error) => Err(error), None => Ok(resolution) }
}

/// The in-actor half of `bank_resolve`: one shared validity check, one
/// geometry sample, then each armed member's shot. A banked round resolves
/// once through the first admitted attempt (the carrier) against the summed
/// raw and master roll inputs; a fallback resolves every armed member with
/// its own captured inputs and self-attributed facts. Every armed token
/// closes exactly once before this returns, whatever the outcome was.
#[allow(clippy::too_many_arguments)]
fn resolve_in_actor(s: &mut System, db: &Db, sde: &Sde, context: &BankContext, target: i64,
    banked: bool, members: &mut [MemberInputs], roll: &mut impl FnMut(f64) -> f64)
    -> Result<BankResolution, CallError>
{
    let mut resolution = if !source_current(s, context) {
        BankResolution::SourceLost
    } else if !target_current(s, context, target) {
        BankResolution::TargetLost
    } else {
        BankResolution::Resolved
    };
    let geometry = if matches!(resolution, BankResolution::Resolved) {
        match (s.sampled_ball(context.ship), s.sampled_ball(target)) {
            (Some(source), Some(victim)) => Some((source, victim)),
            (None, _) => { resolution = BankResolution::SourceLost; None }
            (_, None) => { resolution = BankResolution::TargetLost; None }
        }
    } else { None };
    let signature = geometry.as_ref().map(|(_, victim)| target_signature(sde, victim));
    // Pin each armed member's ramp bonus to this round and sum the volley's
    // bonus-scaled raw here, inside the actor turn that resolves the shots;
    // a bound member whose stored identity no longer matches cancels the
    // volley the same way a source or target loss does.
    let mut parameters = (0.0, 0.0, 0.0, 0.0);
    let mut raw = [0.0; 4];
    if matches!(resolution, BankResolution::Resolved) {
        if banked {
            // The master's captured packet is the roll reference even when the
            // master itself failed consumption.
            let MemberPacket::Turret(master) = &members[0].packet else {
                return Err(CallError::Internal(anyhow::anyhow!("turret bank master captured without turret inputs")));
            };
            parameters = master.parameters;
        }
        for member in members.iter_mut() {
            let Some(round) = member.turret_round.as_mut() else { continue };
            if round.attempt.is_none() { continue; }
            let MemberPacket::Turret(inputs) = &member.packet else {
                return Err(CallError::Internal(anyhow::anyhow!("turret bank member captured without turret inputs")));
            };
            let bonus = member.spool.zip(inputs.spool).map(|(state, parameters)| state.current(parameters));
            round.spool_bonus = bonus;
            if let Some(binding) = member.spool.and_then(|state| state.binding()) {
                if Some(binding.source_membership) != context.source_membership {
                    resolution = BankResolution::SourceLost;
                } else if binding.target != target || binding.target_membership != context.target_generation {
                    resolution = BankResolution::TargetLost;
                }
            }
            // Only the actually admitted members add their typed raw; the
            // master's packet still supplies the roll inputs when the master
            // itself failed.
            if banked {
                for (sum, value) in raw.iter_mut().zip(inputs.raw) { *sum += value * (1.0 + bonus.unwrap_or(0.0)); }
            }
        }
    }
    let mut carrier = false;
    let mut volley_fired = false;
    let mut error: Option<CallError> = None;
    for member in members.iter_mut() {
        let Some(round) = member.turret_round.as_mut() else { continue };
        let Some(mut attempt) = round.attempt.take() else { continue };
        let mut applied = false;
        let mut fired = false;
        'fire: {
            if !matches!(resolution, BankResolution::Resolved) || error.is_some() { break 'fire; }
            if banked && carrier { break 'fire; }
            // A kill or loss mid-round stops the rest: each fired member
            // rechecks the frozen target before rolling.
            if !target_current(s, context, target) {
                resolution = BankResolution::TargetLost;
                break 'fire;
            }
            let (Some((source, victim)), Some(signature)) = (geometry.as_ref(), signature) else { break 'fire; };
            let (parameters, raw) = if banked {
                (parameters, raw)
            } else {
                match &member.packet {
                    MemberPacket::Turret(inputs) =>
                        (inputs.parameters, inputs.raw.map(|damage| damage * (1.0 + round.spool_bonus.unwrap_or(0.0)))),
                    _ => break 'fire,
                }
            };
            // A spooling beam breaks instead of resolving beyond its optimal:
            // the whole volley cancels the same way a source loss does.
            if round.spool_bonus.is_some() && turret_geometry(source, victim).1 > parameters.0 {
                resolution = BankResolution::SourceLost;
                break 'fire;
            }
            carrier |= banked;
            s.drone_attack(context.ship, target);
            let chance = turret_chance(source, victim, parameters.0, parameters.1, parameters.2, parameters.3, signature);
            let multiplier = roll(chance);
            let facts = attempt.facts();
            let mut shot = MemberShot { multiplier, dealt: 0.0, receipt: None,
                shooter: facts.actual_actor_character, victim: s.combat_owner(target) };
            if multiplier <= 0.0 {
                // A genuine miss still reports zero damage and zero quality.
                round.shot = Some(shot);
                fired = true;
                break 'fire;
            }
            // Once invoked, the helper finishes this member's token on every
            // return; it is disarmed below, never finished twice.
            applied = true;
            match apply_damage_in_actor(s, db, context.system, target,
                raw.map(|damage| damage * multiplier), DamageSource {
                    attacker_id: shot.shooter, attacker_ball_id: Some(facts.entity.item_id),
                    source_ship_id: Some(facts.entity.item_id), source_ship_type: Some(facts.entity.type_id),
                    weapon_type_id: facts.weapon_type_id, facts: Some(facts), force_origin: None,
                    illegal: context.illegal,
                }, false)
            {
                Ok(Some((outcome, receipt))) => { shot.dealt = outcome.dealt; shot.receipt = receipt; round.shot = Some(shot); fired = true; }
                // A refused or dead target is no shot, never a fabricated hit.
                Ok(None) => {}
                Err(err) if crate::services::combat::capacity::is_refusal(&err) => { error = Some(err); }
                Err(err) => warn!(member = member.module, "bank member volley failed: {err:?}"),
            }
        }
        if fired {
            if banked {
                volley_fired = true;
            } else if let Some(bonus) = round.spool_bonus {
                if let (Some(state), MemberPacket::Turret(inputs)) = (member.spool.as_mut(), &member.packet) {
                    if let Some(parameters) = inputs.spool {
                        // A fallback member advances only its own really fired
                        // shot; a later member's refusal never rolls it back.
                        state.advance(parameters);
                        member.spool_applied = Some(bonus);
                    }
                }
            }
        }
        // A guard failure, a miss or a member error finishes the token here;
        // an applied volley already closed it inside the damage helper.
        if applied { attempt.disarm(); } else { attempt.finish_in_actor(s); }
    }
    // An accepted banked carrier — hit or genuine miss — advances every
    // member that truly joined the volley once, not only the carrier.
    if banked && volley_fired {
        for member in members.iter_mut() {
            let Some(bonus) = member.turret_round.as_ref().and_then(|round| round.spool_bonus) else { continue };
            let Some(state) = member.spool.as_mut() else { continue };
            let MemberPacket::Turret(inputs) = &member.packet else { continue };
            let Some(parameters) = inputs.spool else { continue };
            state.advance(parameters);
            member.spool_applied = Some(bonus);
        }
    }
    match error { Some(error) => Err(error), None => Ok(resolution) }
}
#[cfg(all(test, feature = "tests-effects"))] mod tests;
