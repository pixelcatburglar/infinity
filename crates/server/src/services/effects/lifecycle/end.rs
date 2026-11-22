use super::*;

pub(in crate::services::effects) fn cycle_end(s: &mut System, cycle: Cycle, end: i64) {
    if !cycle_valid(s, &cycle) {
        if cycle_target_lost(s, &cycle) { publish_cycle(&cycle, ModuleCycleOutcome::TargetLost); }
        cycle_stop(s, cycle);
        return;
    }
    let final_cycle = cycle_is_final(cycle.stop.load(Ordering::Relaxed), cycle.effect.has(EffectFlag::DisallowAutoRepeat), cycle.repeat, cycle.cycles);
    let Some(receiver) = super::super::heat::finish_cycle(s, &cycle, end, final_cycle) else { cycle_stop(s, cycle); return };
    // The adaptive hardener's paid window closes synchronously here: the
    // tally is taken and the profile adapts before the deferred family/heat
    // work, so gap hits can never enter this cycle's window. An ordinary
    // requested stop keeps this paid cycle; a forced-inactive, wrong-owner
    // or replaced source does not.
    if let Some(membership) = cycle.source_membership {
        if let Some(parameters::CycleFrame { packet: parameters::MasterPacket::ReactiveArmor(inputs), .. }) =
            cycle.inputs.frame.as_ref()
        {
            if source_subject_current(s, cycle.cx.ship_id, cycle.cx.char_id, membership,
                &cycle.modules[0], cycle.presented_effect, &cycle.stop)
            {
                s.finish_reactive_armor_cycle(cycle.cx.ship_id, membership, cycle.modules[0].id, &cycle.stop, inputs.shift);
            }
        }
    }
    let membership = s.combat_membership_generation(cycle.cx.ship_id);
    s.defer(async move { cycle_end_work(cycle, end, final_cycle, membership, receiver).await; });
}

/// Family end, then heat damage, then one actor closure that completes heat
/// immediately before the repeat or the retirement.
async fn cycle_end_work(mut cycle: Cycle, end: i64, final_cycle: bool, membership: Option<u64>, receiver: tokio::sync::oneshot::Receiver<Option<Vec<super::super::heat::SourceSnapshot>>>) {
    let cx = cycle.cx.clone();
    let captured = match receiver.await {
        Ok(Some(sources)) => Some(sources),
        Ok(None) | Err(_) => None,
    };
    let mut public = None;
    let (sources, finished) = if let Some(sources) = captured {
        let mut finished = match on_cycle_end(&cx, &mut cycle).await {
            Ok(CycleOutcome::Continue) => { public = Some(ModuleCycleOutcome::Completed); final_cycle }
            Ok(CycleOutcome::Ended { outcome, .. }) => { public = Some(outcome); true }
            Ok(CycleOutcome::Stop(_)) => true,
            Err(e) => { report_cycle_error(&cx, cycle.modules[0].id, "end", e); true }
        };
        if let Err(e) = super::super::heat::damage::apply(&cx, membership, sources).await {
            warn!("module {} heat damage failed: {}", cycle.modules[0].id, match e { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            finished = true;
        }
        (true, finished)
    } else {
        (false, true)
    };
    cx.world.post(cx.system_id, move |s| {
        let continuing = boundary_continues(sources, finished, &cycle.stop);
        super::super::heat::complete_cycle(s, &cycle, !continuing);
        if let Some(outcome) = public { publish_cycle(&cycle, outcome); }
        if continuing {
            cycle_start(s, cycle, end);
        } else {
            cycle_stop(s, cycle);
        }
    });
}

pub(in crate::services::effects) fn cycle_stop(s: &mut System, mut cycle: Cycle) {
    let master = cycle.modules[0].id;
    let cx = cycle.cx.clone();
    debug!("module {master} stopped after {} cycles", cycle.cycles);
    super::super::heat::stop_cycle(s, &cycle);
    if let Some(mut pending) = cycle.owner.pending_combat.take() {
        s.finish_combat_attempt(&pending.facts());
        pending.disarm();
    }
    // Residual turret-bank round tokens close in place before their native
    // generations retire; a cycle never stops inside a resolved volley. Any
    // unconsumed loaded use returns its admission to the slot.
    for member in &mut cycle.inputs.members {
        if let Some(permit) = member.permit.take() { s.release_loaded_use(permit); }
        if let Some(round) = member.turret_round.as_mut() {
            if let Some(attempt) = round.attempt.as_mut() { attempt.finish_in_actor(s); }
        }
        member.turret_round = None;
    }
    retire_combat_generations(s, std::mem::take(&mut cycle.owner.combat_generations));
    let tracked = super::super::active_cycle::tracked(cycle.effect.id);
    let (retired, terminal) = if tracked {
        let mut st = state();
        let terminal = super::super::active_cycle::terminate(&mut st, master, &cycle.stop);
        drop(st);
        cycle.owner.armed = false;
        (false, terminal)
    } else {
        let mut st = state();
        let retired = retire_activation(&mut st.active, master, &cycle.stop, || {
            if cycle.effect.kind != EffectKind::Entosis || cycle.cycles > 0 {
                ship_effect(&cx, master, cycle.presented_effect, cycle.target, cycle.other.clone(), false, cycle.started, cycle.duration_ms, cycle.repeat);
            }
        });
        if retired && cycle.effect.kind != EffectKind::Propulsion && st.activation_generation.get(&master).is_some_and(|current| Arc::ptr_eq(current, &cycle.stop)) {
            st.activation_generation.remove(&master);
        }
        drop(st);
        if retired { cycle.owner.armed = false; }
        (retired, None)
    };
    if let Some(terminal) = terminal { super::super::active_cycle::apply_terminal(s, terminal); }
    if cycle.effect.kind == EffectKind::Entosis { super::super::entosis::stop(s, &cycle); }
    if retired {
        for module in &cycle.modules { s.fitted_module_active(cx.ship_id, module.id, false); }
    }
    // A retired sphere's assistance block lifts with the published inactive
    // bit: the source's incoming projections are re-evaluated once, but only
    // while the ship that admitted this activation still owns its combat
    // membership; a replacement never receives a stale generation's refresh.
    if retired && cycle.presented_effect == i64::from(infinity_core::consts::dogma::EFFECT_WARP_DISRUPT_SPHERE)
        && s.combat_membership_generation(cx.ship_id) == cycle.source_membership {
        s.refresh_projection_target(cx.ship_id);
    }
    if retired && matches!(cycle.effect.kind, EffectKind::Cloak | EffectKind::MicroJumpDrive | EffectKind::MicroJumpField) {
        s.refresh_projection_target(cx.ship_id);
    }
    if retired { on_stop(s, &cx, &cycle); }
    // A retired activation reports its ramp attributes back to zero and drops
    // its readouts; a superseded generation is untouched by its fencing.
    if retired { s.spool_readout_clear(cx.ship_id, &cycle.stop); }
    if !tracked { s.retire_fx_activation(master, &cycle.stop); }
    if retired && matches!(charge_kind(cycle.effect.id, cycle.effect.kind), EffectKind::FueledLocalRepair { .. }) {
        s.defer(async move {
            if let Err(error) = crate::services::combat::targeting_charges::auto_reload(&cx, master).await {
                warn!("module {master} automatic reload failed: {}", match error { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            }
        });
    }
}
