//! Deferred module cycle admission, resource consumption and start.
//! Uses the same dogma and actor-owned activation state as the lifecycle;
//! cycles are transient and are not resumed after restart.
use super::*;
use crate::services::effects::fx::weapon_fx;
use crate::services::effects::{capacitor, ecm, entosis, fuel, heat};
use crate::services::effects::behaviors::on_cycle_start;
use crate::client::filetime_now;
use crate::services::combat::groups_health::drain_capacitor;
use crate::services::combat::targeting_charges::charge_key;
use crate::space::system::presentation::FxRetention;
pub(super) async fn cycle_begin_work(mut cycle: Cycle) {
    let cx = cycle.cx.clone();
    let master = cycle.modules[0].id;
    let mut public = None;
    let mut check_target = false;
    let result = async {
        let resource_kind = charge_kind(cycle.effect.id, cycle.effect.kind);
        if charge_sensitive(resource_kind) && cycle.modules.iter().any(|m| state().reloading.get(&m.id).is_some_and(|until| *until > filetime_now())) {
            return Err("module reloading");
        }
        let charge = if charge_sensitive(resource_kind) {
            let (ship, module) = (cx.ship_id, cycle.modules[0].id);
            let (effective, empty) = cx.world.with(cx.system_id, move |s| {
                (s.loaded_effective(ship, module), s.loaded_empty_proof(ship, module))
            }).await.unwrap_or((None, false));
            // A pending reload, or a slot an open business transaction still
            // owns, is the race the durable pending query used to refuse.
            if effective.map_or(!empty, |view| view.3) { return Err("module reloading"); }
            effective.and_then(|view| (view.2 > 0).then_some((view.1, view.2)))
        } else { None };
        let Some(frame) = cycle.inputs.frame.as_ref() else { return Err("cycle inputs uncaptured") };
        let (master_charge_type, duration_ms, discharge, capacitor) =
            (frame.master_charge_type, frame.duration_ms, frame.discharge, frame.capacitor);
        if charge_sensitive(resource_kind) && charge.map(|(kind, _)| kind) != master_charge_type {
            return Err("charge changed since fit snapshot");
        }
        if needs_charge(cycle.effect.kind) && charge.is_none() { return Err("out of charges"); }
        let charged = charge.is_some();
        if charge_sensitive(resource_kind) { cycle.charge_type = charge.map(|(kind, _)| kind); }
        if charge_sensitive(resource_kind) {
            cycle.other = charge.map(|(kind, _)| charge_key(cx.ship_id, cycle.modules[0].flag, kind));
        }
        cycle.duration_ms = duration_ms;
        if cycle.effect.kind != EffectKind::Entosis { cycle.duration_ms = cycle.duration_ms.max(MIN_MODULE_DURATION_MS); }
        let entosis_proof = if cycle.effect.kind == EffectKind::Entosis {
            let Some(parameters::MasterPacket::Entosis(inputs)) = cycle.inputs.frame.as_ref().map(|frame| &frame.packet) else {
                return Err("cycle inputs uncaptured");
            };
            Some(entosis::prepare(&cx, inputs, &cycle).await.map_err(|_| "Entosis admission refused")?)
        } else { None };
        if matches!(cycle.effect.kind, EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu)
            && capacitor::target_distance(&cx, cycle.target).await.is_err() {
            check_target = true;
            return Err("target lost");
        }
        if matches!(cycle.effect.kind, EffectKind::Ecm | EffectKind::BurstJammer) && ecm::validate_cycle(&cx, &cycle).await.is_err() { check_target = true; return Err("target lost"); }
        // A mode revalidates the shared source authority before any debit,
        // including later cycles: a stale generation, owner, membership,
        // online bit or effect never spends capacitor or fuel.
        if super::super::active_cycle::ship_mode(cycle.effect.id) {
            let admitted = cycle.source_membership;
            let (ship, character, module, effect, generation) = (cx.ship_id, cx.char_id, cycle.modules[0].clone(), cycle.presented_effect, cycle.stop.clone());
            let current = cx.world.with(cx.system_id, move |s| admitted.is_some_and(|membership|
                super::super::active_cycle::mode_source_current(s, ship, character, membership, &module, effect, &generation))).await.unwrap_or(false);
            if !current { return Err("source lost"); }
        }
        let need = cycle_discharge(resource_kind, charged, discharge);
        if !drain_capacitor(&cx, capacitor.0, capacitor.1, need) {
            cx.notify("OnEffectMessage", vec![Value::str("NotEnoughCharge"), Value::Dict(vec![])]);
            public = Some(ModuleCycleOutcome::OutOfCapacitor);
            return Err("out of capacitor");
        }
        let (ship, character, target, generation) = (cx.ship_id, cx.char_id, cycle.target, cycle.stop.clone());
        let valid = cx.world.with(cx.system_id, move |s| {
            !generation.load(Ordering::Relaxed)
                && s.ball(ship).is_some_and(|ball| ball.char_id == Some(character))
                && target.is_none_or(|id| s.contains(id))
                && state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation))
        }).await.unwrap_or(false);
        if !valid {
            check_target = true;
            if let Err(error) = refund_refused_charge(&cx, need).await {
                warn!("module {master} capacitor refund failed: {}", match error { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            }
            return Err("source or target lost");
        }
        let fuel_result = match std::mem::replace(&mut cycle.inputs.fuel, Ok(Vec::new())) {
            Ok(requirements) => fuel::consume_cycle(&cx, &requirements).await,
            Err(error) => Err(error),
        };
        if let Err(error) = fuel_result {
            if let Err(refund) = refund_refused_charge(&cx, need).await {
                warn!("module {master} capacitor refund failed: {}", match refund { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            }
            match error {
                CallError::User { msg, dict } => cx.notify("OnEffectMessage", vec![Value::str(msg), Value::Dict(dict.into_iter().map(|(key, value)| (Value::str(key), value)).collect())]),
                CallError::Internal(error) => warn!("module {master} fuel consumption failed: {error:#}"),
            }
            return Err("fuel unavailable");
        }
        if cycle.effect.kind == EffectKind::SuperWeapon {
            cycle.target = None;
        }
        // The energizer's one activation write burns the module to its captured
        // effective hit points through the shared heat-damage authority, after
        // the capacitor precharge and before the Running record is accepted. A
        // refused write refunds the precharge and emits no start or cooldown.
        if cycle.cycles == 0 && cycle.effect.id == i64::from(infinity_core::consts::dogma::EFFECT_EMERGENCY_HULL_ENERGIZER) {
            if let Err(error) = super::super::active_cycle::burn(&cx, &cycle).await {
                report_cycle_error(&cx, master, "start", error);
                if let Err(refund) = refund_refused_charge(&cx, need).await {
                    warn!("module {master} capacitor refund failed: {}", match refund { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
                }
                return Err("energizer burn refused");
            }
        }
        let start_result = if let Some(proof) = entosis_proof {
            if let Some(start) = entosis::begin(&cx, proof, cycle.duration_ms).await {
                cycle.nominal_start = start;
                if cycle.cycles == 0 {
                    crate::services::crimewatch::weapons_activate(&cx.world, cx.char_id);
                }
                Ok(CycleOutcome::Continue)
            } else { Ok(CycleOutcome::Stop("Entosis link refused")) }
        } else { on_cycle_start(&cx, &mut cycle, charged).await };
        match start_result {
            Ok(CycleOutcome::Continue) => {}
            Ok(result) => {
                let reason = match result {
                    CycleOutcome::Stop(reason) => reason,
                    CycleOutcome::Ended { reason, outcome } => { public = Some(outcome); reason }
                    CycleOutcome::Continue => unreachable!(),
                };
                if ((charge_sensitive(resource_kind) && matches!(reason, "out of charges" | "source lost or in warp")
                    || matches!(cycle.effect.kind, EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu) && reason == "target lost")
                    || super::super::active_cycle::ship_mode(cycle.effect.id) && reason == "source lost") && need > 0.0 {
                    if let Err(error) = refund_refused_charge(&cx, need).await {
                        warn!("module {master} capacitor refund failed: {}", match error { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
                    }
                }
                return Err(reason);
            }
            Err(e) => { report_cycle_error(&cx, master, "start", e); return Err("phase failed"); }
        }
        Ok(())
    }.await;
    let (result, cycle, deadline) = if result.is_ok() {
        // The heat connection runs inside one actor turn on the captured
        // packet, then the cycle returns and the original order continues. A
        // tracked activation connects and validates heat before any visible
        // acceptance, so a failure consumes its record silently in this same
        // turn; a stale admission returns for the ordinary stop. Members
        // whose shot actually fired publish their ramp readout in the same
        // turn. Only `spool_applied` carriers reach it.
        match cx.world.with(cx.system_id, move |s| {
            let (mode, bounded) = (super::super::active_cycle::ship_mode(cycle.effect.id),
                super::super::active_cycle::duration_bounded(cycle.effect.id));
            let deadline = if bounded || mode {
                if let Err(error) = heat::connect_cycle(s, &cycle) {
                    let mut st = state();
                    let terminal = super::super::active_cycle::terminate(&mut st, master, &cycle.stop);
                    drop(st);
                    if let Some(terminal) = terminal { super::super::active_cycle::apply_terminal(s, terminal); }
                    return (Some((Err(error), None)), cycle);
                }
                if mode {
                    let current = cycle.source_membership.is_some_and(|membership|
                        super::super::active_cycle::mode_source_current(s, cycle.cx.ship_id, cycle.cx.char_id, membership,
                            &cycle.modules[0], cycle.presented_effect, &cycle.stop));
                    if !current { return (None, cycle); }
                }
                if bounded || cycle.cycles == 0 {
                    match super::super::active_cycle::accept(s, &mut cycle) {
                        Some(deadline) if bounded => Some(deadline),
                        Some(_) => None,
                        None => return (None, cycle),
                    }
                } else if !super::super::active_cycle::running_generation(master, &cycle.stop) {
                    return (None, cycle);
                } else { None }
            } else { None };
            for module in &cycle.modules {
                s.fitted_module_active(cycle.cx.ship_id, module.id, true);
            }
            // An active sphere carries a positive `disallowAssistance` value:
            // its first published active bit turns the restriction on, so the
            // source's incoming projections are re-evaluated once inside this
            // actor turn. The refresh needs the still-current activation, the
            // admitted membership and the actually published bit; scripted
            // presentations share the presented id, and later cycles never
            // republish, so they never re-arm it.
            if cycle.cycles == 0
                && cycle.presented_effect == i64::from(infinity_core::consts::dogma::EFFECT_WARP_DISRUPT_SPHERE)
                && activation_current(master, cycle.cx.char_id, &cycle.stop)
                && s.combat_membership_generation(cycle.cx.ship_id) == cycle.source_membership
                && s.with_combat_fit(cycle.cx.ship_id, |fit, _, _|
                    fit.module_state(master).is_some_and(|(_, active)| active)).unwrap_or(false)
            {
                s.refresh_projection_target(cycle.cx.ship_id);
            }
            let nominal = crate::client::filetime_from_unix_ms(cycle.nominal_start);
            for member in &cycle.inputs.members {
                let Some(applied) = member.spool_applied else { continue };
                let Some(state) = member.spool else { continue };
                if state.binding().is_none() { continue };
                let parameters::MemberPacket::Turret(inputs) = &member.packet else { continue };
                let Some(parameters) = inputs.spool else { continue };
                let max_at = parameters.maximum_at(nominal, state.current(parameters), cycle.duration_ms);
                s.spool_readout_publish(cycle.cx.ship_id, member.module, &cycle.stop, applied, max_at, cycle.cx.char_id);
            }
            // The adaptive hardener installs or reopens its profile in the
            // same actor turn once the authority check and the heat connection
            // have both succeeded; a refused admission stops the activation
            // rather than cycling silently without the mechanic.
            let connection = match if bounded || mode { Ok(()) } else { heat::connect_cycle(s, &cycle) } {
                Err(error) => Err(error),
                Ok(()) if cycle.effect.id == i64::from(EFFECT_ADAPTIVE_ARMOR_HARDENER) => {
                    let seed = match cycle.inputs.frame.as_ref().map(|frame| &frame.packet) {
                        Some(parameters::MasterPacket::ReactiveArmor(inputs)) => inputs.seed,
                        _ => None,
                    };
                    let admitted = cycle.source_membership.is_some_and(|membership|
                        source_subject_current(s, cycle.cx.ship_id, cycle.cx.char_id, membership,
                            &cycle.modules[0], cycle.presented_effect, &cycle.stop)
                        && s.begin_reactive_armor(cycle.cx.ship_id, membership, cycle.modules[0].id,
                            cycle.modules[0].type_id, &cycle.stop, seed));
                    if admitted { Ok(()) } else {
                        Err(CallError::Internal(anyhow::anyhow!("adaptive armor activation refused")))
                    }
                }
                Ok(()) => Ok(()),
            };
            (Some((connection, deadline)), cycle)
        }).await {
            Some((Some((Ok(()), deadline)), returned)) => {
                let mut result = Ok(());
                if returned.effect.kind == EffectKind::Entosis && returned.cycles == 0
                    && !entosis::publish_start(&cx, &returned).await {
                    result = Err("Entosis link invalidated");
                }
                if result.is_ok() && (returned.effect.kind == EffectKind::Smartbomb || returned.cycles == 0 && matches!(returned.effect.kind, EffectKind::Bomb | EffectKind::FueledLocalRepair { .. } | EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster | EffectKind::Ecm | EffectKind::BurstJammer | EffectKind::MicroJumpField)) {
                    let retention = if matches!(returned.effect.kind, EffectKind::FueledLocalRepair { .. } | EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster | EffectKind::Ecm) { FxRetention::Current } else { FxRetention::Transient };
                    weapon_fx(&cx, &returned.modules, &returned.effect, returned.target, returned.charge_type, true, returned.duration_ms, returned.repeat, master, &returned.stop, retention, Value::None);
                }
                (result, Some(returned), deadline)
            }
            Some((Some((Err(error), _)), returned)) => {
                warn!("module {master} heat connection failed: {}", match error { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
                let amount = returned.inputs.frame.as_ref().map(|frame| cycle_discharge(charge_kind(returned.effect.id, returned.effect.kind), returned.charge_type.is_some(), frame.discharge)).unwrap_or(0.0);
                if amount > 0.0 {
                    if let Err(refund) = refund_refused_charge(&cx, amount).await {
                        warn!("module {master} capacitor refund failed: {}", match refund { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
                    }
                }
                (Err("heat connection failed"), Some(returned), None)
            }
            Some((None, returned)) => {
                let amount = returned.inputs.frame.as_ref().map(|frame| cycle_discharge(charge_kind(returned.effect.id, returned.effect.kind), returned.charge_type.is_some(), frame.discharge)).unwrap_or(0.0);
                if super::super::active_cycle::ship_mode(returned.effect.id) && amount > 0.0 {
                    if let Err(refund) = refund_refused_charge(&cx, amount).await {
                        warn!("module {master} capacitor refund failed: {}", match refund { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
                    }
                }
                (Err("activation stale"), Some(returned), None)
            }
            None => (Err("system unavailable"), None, None),
        }
    } else { (result, Some(cycle), None) };
    match (result, cycle) {
        (Ok(()), Some(mut cycle)) => {
            cycle.cycles += 1;
            if let Some(deadline) = deadline {
                // The consumer's nominal end is the one terminal owner; it
                // never re-enters the repeating cycle boundary.
                cx.world.post(cx.system_id, move |s| {
                    s.schedule_at(deadline, move |s| super::super::active_cycle::finish_current(s, cycle));
                });
            } else {
                let (start, duration) = (cycle.nominal_start, cycle.duration_ms);
                cx.world.post(cx.system_id, move |s| {
                    schedule_cycle_end(s, start, duration, move |s, end| super::end::cycle_end(s, cycle, end));
                });
            }
        }
        (Err(reason), Some(cycle)) => {
            debug!("module {master} stopped: {reason}");
            cx.world.post(cx.system_id, move |s| {
                if cycle_source_current(s, &cycle) {
                    if let Some(outcome) = public { publish_cycle(&cycle, outcome); }
                    else if check_target && cycle_target_lost(s, &cycle) { publish_cycle(&cycle, ModuleCycleOutcome::TargetLost); }
                }
                super::end::cycle_stop(s, cycle);
            });
        }
        (Err(reason), None) => {
            // The dropped cycle's owner teardown already ran; the actor is gone.
            debug!("module {master} stopped: {reason}");
        }
        (Ok(()), None) => unreachable!("the actor turn returns the cycle"),
    }
}
