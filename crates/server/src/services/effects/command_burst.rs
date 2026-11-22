//! Command-burst pulses: the five module families wired to SDE effects
//! 6732-6736 (command-burst charge groups plus the expedition charge group,
//! module group 1770). Mirrors the client's
//! `dbuff/common/baseBurstEffect.py` pulse, `dbuff/client/incomingDbuffTracker.py`
//! whole-state tracker and `eve/client/script/parklife/dbuffClient.py`
//! boosted count. Static data is read from `Arc<Sde>` dogma and `dbuffs`
//! plus actor state; no tables. Each pulse consumes one charge, lands the
//! module's effective `warfareBuff*` collections on the source ship and on
//! every fleet member's ship within surface range as timed runtime modifier
//! sources, sends each recipient pilot the complete `OnDbuffUpdated` state
//! and the source pilot `OnFleetBoosted`, and emits the transient sphere and
//! link effects. Landed buffs are never revoked when the source leaves; they
//! expire by deadline. Expedition consumers, environment and weather
//! producers, a Crimewatch weapons timer and tether/force-field eligibility
//! are not supported here.

use infinity_core::consts::dogma::{ATTR_BUFF_DURATION, ATTR_WARFARE_BUFF_IDS, ATTR_WARFARE_BUFF_VALUES};
use super::{lifecycle::{Cycle, CycleOutcome}, parameters::{MasterPacket, MemberInputs}, registry::EffectAttribute, weapons::consume_charge, command_burst_fx::emit_command_burst_fx, notify};
use crate::services::{combat::Ctx, CallError, fit::{Fit, Key}, fit::runtime::{COMMAND_BURST_NAMESPACE, RuntimeReceiverKey, RuntimeSourceKey}};
use crate::space::{dbuff::TimedRuntimeInput, dist, FxModule, System};
use marshal::Value;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum CommandBurstFamily { Armor, Shield, Skirmish, Information, Mining }

/// One collection slot of the module's effective buff set (id 0 = unused).
#[derive(Clone, Copy, Debug)]
pub(crate) struct WarfareBuff { pub(crate) id: i32, pub(crate) value: f64 }

/// One pulse's frozen inputs captured from the resident fit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CommandBurstInputs {
    pub(crate) family: CommandBurstFamily,
    pub(crate) module_item: i64,
    pub(crate) module_type: i32,
    pub(crate) module_incarnation: u64,
    pub(crate) duration_ms: i64,
    pub(crate) range_m: f64,
    pub(crate) buffs: [WarfareBuff; 4],
}

/// Source identity of one module incarnation (typed namespace, never a plain
/// u16). `source_ship` is not part of the identity: fitted module items are
/// unique server-wide, so a refit of the same module on the same ship keeps
/// the key.
pub(crate) fn command_burst_source_key(_source_ship: i64, source_module: i64, module_incarnation: u64) -> RuntimeSourceKey {
    RuntimeSourceKey { producer: COMMAND_BURST_NAMESPACE, source_id: source_module, incarnation: module_incarnation }
}

/// Read the module's effective warfare-buff set and pulse shape from the
/// resident fit; the loaded charge's own warfare-charge multipliers already
/// contribute through the fit's `Key::Charge` node and are never reapplied
/// here. An invalid SDE or charge row is an internal fault, not a refusal.
pub(super) fn capture(fit: &Fit, cycle: &Cycle, family: CommandBurstFamily) -> Result<CommandBurstInputs, CallError> {
    let module = &cycle.modules[0];
    let key = Key::Module(module.id);
    let module_incarnation = cycle.source_membership
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("command burst captured before membership binding")))?;
    let duration_ms = fit.value(key, ATTR_BUFF_DURATION);
    if !duration_ms.is_finite() || duration_ms <= 0.0 {
        return Err(CallError::Internal(anyhow::anyhow!("command burst buff duration invalid")));
    }
    let range_m = cycle.effect.attribute(EffectAttribute::Range).map_or(0.0, |attr| fit.value(key, attr));
    if !range_m.is_finite() {
        return Err(CallError::Internal(anyhow::anyhow!("command burst range invalid")));
    }
    let mut buffs = [WarfareBuff { id: 0, value: 0.0 }; 4];
    for (index, slot) in buffs.iter_mut().enumerate() {
        let id = fit.value(key, ATTR_WARFARE_BUFF_IDS[index]) as i32;
        let value = fit.value(key, ATTR_WARFARE_BUFF_VALUES[index]);
        if id <= 0 { continue; }
        if fit.sde().dbuffs.get(id).is_none() || !value.is_finite() {
            return Err(CallError::Internal(anyhow::anyhow!("command burst warfare buff invalid")));
        }
        *slot = WarfareBuff { id, value };
    }
    Ok(CommandBurstInputs {
        family, module_item: module.id, module_type: module.type_id, module_incarnation,
        duration_ms: duration_ms as i64, range_m, buffs,
    })
}

/// Pulse one cycle: pay the charge, snapshot the fleet once outside the
/// actor, then land the buffs inside the actor on the source and every
/// eligible member ship.
pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let Some(MasterPacket::CommandBurst(inputs)) = cycle.inputs.frame.as_ref().map(|frame| &frame.packet) else {
        return Err(CallError::Internal(anyhow::anyhow!("command burst cycle captured without burst inputs")));
    };
    let inputs = *inputs;
    let (permit, token) = cycle.inputs.members.first_mut().map_or((None, None), MemberInputs::take_charge);
    if consume_charge(cx, permit, token).await.is_none() {
        return Ok(CycleOutcome::Stop("out of charges"));
    }
    let members = crate::services::fleets::command_burst_members(cx.char_id);
    let (source_ship, source_char, cycle_duration) = (cx.ship_id, cx.char_id, cycle.duration_ms);
    let boosted = cx.world.with(cx.system_id, move |s|
        pulse(s, source_ship, source_char, inputs, &members, cycle_duration)).await.flatten();
    match boosted {
        None => Ok(CycleOutcome::Stop("source lost or in warp")),
        Some(count) => {
            cx.notify("OnFleetBoosted", notify::command_burst_fleet_boosted(inputs.module_type, count));
            Ok(CycleOutcome::Continue)
        }
    }
}

/// Land one captured pulse inside the actor: the source ship always, plus
/// the pilot balls of the fleet snapshot members present in this system,
/// uncloaked and within surface range of the source.
fn pulse(s: &mut System, source_ship: i64, source_char: i64, inputs: CommandBurstInputs, members: &[i64], cycle_duration: f64) -> Option<i64> {
    let source = s.sampled_ball(source_ship)?;
    let expires_at_ms = s.now_ms().saturating_add(inputs.duration_ms);
    let key = command_burst_source_key(source_ship, inputs.module_item, inputs.module_incarnation);
    let module = Some(FxModule { item: Some(inputs.module_item), type_id: inputs.module_type });
    let recipients: Vec<i64> = std::iter::once(source_ship)
        .chain(members.iter().copied()
            .filter(|member| *member != source_char)
            .filter_map(|member| s.pilot_ball(member).map(|ball| ball.id)))
        .filter(|&id| s.sampled_ball(id).is_some_and(|ball| ball.cloak == 0
            && (id == source_ship
                || dist(source.pos, ball.pos) - f64::from(source.radius) - f64::from(ball.radius) <= inputs.range_m)))
        .collect();
    emit_command_burst_fx(s, source_ship, module, None, inputs.family, Value::Float(cycle_duration));
    for ship in &recipients {
        let timed: Vec<TimedRuntimeInput> = inputs.buffs.iter()
            .filter(|buff| buff.id > 0)
            .map(|buff| TimedRuntimeInput {
                receiver: RuntimeReceiverKey::Ship(*ship),
                collection_id: buff.id,
                source: key,
                value: buff.value,
                expires_at_ms,
            }).collect();
        s.land_runtime_inputs(&timed);
        emit_command_burst_fx(s, *ship, module, Some(source_ship), inputs.family, Value::Float(cycle_duration));
    }
    Some(recipients.len() as i64)
}
