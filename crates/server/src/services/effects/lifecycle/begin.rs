//! Deferred module cycle admission, resource consumption and start.
//! Uses the same dogma and actor-owned activation state as the lifecycle;
//! cycles are transient and are not resumed after restart.
use super::*;
use crate::services::effects::fx::weapon_fx;
use crate::services::effects::{capacitor, ecm, entosis, fuel, heat};
use crate::services::effects::behaviors::on_cycle_start;
use crate::services::combat::groups_health::drain_capacitor;
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
                if (charge_sensitive(resource_kind) && matches!(reason, "out of charges" | "source lost or in warp")
                    || matches!(cycle.effect.kind, EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu) && reason == "target lost") && need > 0.0 {
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
    let (result, cycle) = if result.is_ok() {
        // The heat connection runs inside one actor turn on the captured
        // packet, then the cycle returns and the original order continues.
        // Members whose shot actually fired publish their ramp readout in the
        // same turn — only `spool_applied` carriers reach it.
        match cx.world.with(cx.system_id, move |s| {
            for module in &cycle.modules {
                s.fitted_module_active(cycle.cx.ship_id, module.id, true);
            }
            let nominal = crate::client::filetime_from_unix_ms(cycle.nominal_start);
            for member in &cycle.inputs.members {
                let Some(applied) = member.spool_applied else { continue };
                let Some(state) = member.spool else { continue };
                if state.binding().is_none() { continue; }
                let parameters::MemberPacket::Turret(inputs) = &member.packet else { continue };
                let Some(parameters) = inputs.spool else { continue };
                let max_at = parameters.maximum_at(nominal, state.current(parameters), cycle.duration_ms);
                s.spool_readout_publish(cycle.cx.ship_id, member.module, &cycle.stop, applied, max_at, cycle.cx.char_id);
            }
            (heat::connect_cycle(s, &cycle), cycle)
        }).await {
            Some((Ok(()), returned)) => {
                let mut result = Ok(());
                if returned.effect.kind == EffectKind::Entosis && returned.cycles == 0
                    && !entosis::publish_start(&cx, &returned).await {
                    result = Err("Entosis link invalidated");
                }
                if result.is_ok() && (returned.effect.kind == EffectKind::Smartbomb || returned.cycles == 0 && matches!(returned.effect.kind, EffectKind::Bomb | EffectKind::FueledLocalRepair { .. } | EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster | EffectKind::Ecm | EffectKind::BurstJammer)) {
                    let retention = if matches!(returned.effect.kind, EffectKind::FueledLocalRepair { .. } | EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster | EffectKind::Ecm) { FxRetention::Current } else { FxRetention::Transient };
                    weapon_fx(&cx, &returned.modules, &returned.effect, returned.target, returned.charge_type, true, returned.duration_ms, returned.repeat, master, &returned.stop, retention, Value::None);
                }
                (result, Some(returned))
            }
            Some((Err(error), returned)) => {
                warn!("module {master} heat connection failed: {}", match error { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
                (Err("heat connection failed"), Some(returned))
            }
            None => (Err("system unavailable"), None),
        }
    } else { (result, Some(cycle)) };
    match (result, cycle) {
        (Ok(()), Some(mut cycle)) => {
            cycle.cycles += 1;
            let (start, duration) = (cycle.nominal_start, cycle.duration_ms);
            cx.world.post(cx.system_id, move |s| {
                schedule_cycle_end(s, start, duration, move |s, end| cycle_end(s, cycle, end));
            });
        }
        (Err(reason), Some(cycle)) => {
            debug!("module {master} stopped: {reason}");
            cx.world.post(cx.system_id, move |s| {
                if cycle_source_current(s, &cycle) {
                    if let Some(outcome) = public { publish_cycle(&cycle, outcome); }
                    else if check_target && cycle_target_lost(s, &cycle) { publish_cycle(&cycle, ModuleCycleOutcome::TargetLost); }
                }
                cycle_stop(s, cycle);
            });
        }
        (Err(reason), None) => {
            // The dropped cycle's owner teardown already ran; the actor is gone.
            debug!("module {master} stopped: {reason}");
        }
        (Ok(()), None) => unreachable!("the actor turn returns the cycle"),
    }
}
