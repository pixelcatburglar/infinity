//! Automatic and passive acquisition through the shared combat targeting path.
//! Passive targeting mirrors dogma/pythonEffects/migratedeffects.py AddTargetEx.
//! Candidates are current incoming locks from combat's shared targeter query; no unrelated nearby ships are targeted.
use super::{lifecycle::{Cycle, CycleOutcome, Module}, registry::Effect, EffectKind};
use crate::messages;
use crate::services::{combat::{self, state, Ctx}, combat::targeting_charges::add_target_for_cycle, fit::Fit, user_error, CallError};
use std::sync::{Arc, atomic::Ordering};

pub(super) async fn validate(cx: &Ctx, _fit: &Fit, _module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    if effect.kind == EffectKind::PassiveTarget {
        let target = target.ok_or_else(|| user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]))?;
        let ship = cx.ship_id;
        if !cx.world.with(cx.system_id, move |s| s.contains(ship) && s.contains(target)).await.unwrap_or(false) {
            return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
        }
    }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, _fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let master = cycle.modules[0].id;
    if cycle.stop.load(Ordering::Relaxed) || !state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &cycle.stop)) {
        return Ok(CycleOutcome::Stop("activation lost"));
    }
    if cycle.effect.kind == EffectKind::PassiveTarget {
        let target = cycle.target.ok_or_else(|| user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]))?;
        add_target_for_cycle(cx, target, true, master, cycle.stop.clone()).await?;
    } else {
        let candidates = combat::targeters(&cx.world, cx.system_id, cx.ship_id).await;
        for target in candidates {
            if cycle.stop.load(Ordering::Relaxed) || !state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &cycle.stop)) { break; }
            let _ = add_target_for_cycle(cx, target, false, master, cycle.stop.clone()).await;
        }
    }
    Ok(CycleOutcome::Continue)
}
