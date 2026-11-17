//! Native combat-source admission for one member of an activation cycle.
//! The system actor's combat facts own generation/sequence reservation and
//! replay refusal; the shared activation table guards source currency. The
//! returned token owns cancellation cleanup; charge, target and reporting
//! stay with the effect families. Turret banks admit each member separately:
//! the replay owner is always the real fitted member while the facts'
//! attributed module is the master for a banked volley and the member itself
//! for a mismatch fallback.
use super::*;
use crate::services::effects::space_source::PreparedCombatAttempt;

/// Reserve at native admission; the returned token owns cancellation cleanup.
/// `member_id` owns the replay generation/sequence and `attribution` is the
/// module the facts blame for the outcome; they differ only in a turret bank.
async fn prepare(cycle: &mut Cycle, member_id: i64, attribution: i64, weapon_type_id: Option<i32>)
    -> Result<Option<PreparedCombatAttempt>, CallError>
{
    let Some(member) = cycle.owner.combat_generations.iter_mut().find(|member| member.module_id == member_id) else {
        return Err(CallError::Internal(anyhow::anyhow!("combat activation member missing")));
    };
    let Some(sequence) = member.sequence.checked_add(1) else {
        return Err(CallError::Internal(anyhow::anyhow!("combat activation sequence exhausted")));
    };
    member.sequence = sequence;
    let generation = member.generation;
    let (ship, character, system, effect, master) = (cycle.cx.ship_id, cycle.cx.char_id, cycle.cx.system_id, cycle.effect.id, cycle.owner.master);
    let stop = cycle.stop.clone();
    let weak = Arc::downgrade(&cycle.cx.world);
    cycle.cx.world.with(system, move |s| {
        let valid = |s: &System| {
            !stop.load(Ordering::Relaxed)
                && state().active.get(&master).is_some_and(|active|
                    active.char_id == character && !active.forced_inactive && Arc::ptr_eq(&active.stop, &stop))
                && s.pilot_ball(character).is_some_and(|ball| ball.id == ship)
        };
        if !valid(s) { return Ok(None); }
        let source = s.prepare_combat_source(ship, Some(attribution), Some(effect), weapon_type_id,
            crate::lua::events::resources::FactOrigin::Natural, generation, sequence, 1, valid)
            .map_err(|refusal| match refusal {
                crate::space::combat_facts::Refusal::Full => crate::services::combat::capacity::refusal(system),
                other => CallError::Internal(anyhow::anyhow!("combat source admission refused: {other:?}")),
            })?;
        Ok(Some(PreparedCombatAttempt::new(source, weak, system).with_activation(master, stop)))
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat actor unavailable")))?
}

/// Reserve at native admission; the returned token owns cancellation cleanup.
pub(in crate::services::effects) async fn prepare_combat_source(cycle: &mut Cycle, module_id: i64, weapon_type_id: Option<i32>)
    -> Result<Option<PreparedCombatAttempt>, CallError>
{
    prepare(cycle, module_id, module_id, weapon_type_id).await
}

/// Admit one member of a grouped turret cycle. `banked` selects the master as
/// the facts' attributed module for a normal banked volley; `false` attributes
/// the member itself for the mismatch fallback. Either way the replay owner is
/// the real member's own generation and sequence.
pub(in crate::services::effects) async fn prepare_turret_member(cycle: &mut Cycle, member_id: i64, banked: bool)
    -> Result<Option<PreparedCombatAttempt>, CallError>
{
    if cycle.effect.kind != EffectKind::Turret || cycle.modules.len() < 2 {
        return Err(CallError::Internal(anyhow::anyhow!("turret member prepare outside a grouped turret cycle")));
    }
    let Some(member) = cycle.modules.iter().find(|module| module.id == member_id) else {
        return Err(CallError::Internal(anyhow::anyhow!("turret member is not a cycle member")));
    };
    let member_type = member.type_id;
    let (attribution, weapon_type_id) = if banked {
        (cycle.modules[0].id, cycle.modules[0].type_id)
    } else {
        (member_id, member_type)
    };
    prepare(cycle, member_id, attribution, Some(weapon_type_id)).await
}
