//! Deferred module cycle admission, resource consumption and start.
//! Uses the same dogma and actor-owned activation state as the lifecycle;
//! cycles are transient and are not resumed after restart.
use super::*;
use crate::services::effects::{capacitor, ecm, entosis, fuel, heat};
use crate::services::effects::behaviors::on_cycle_start;
use crate::services::combat::groups_health::drain_capacitor;
use infinity_core::consts::dogma::ATTR_CAPACITOR_NEED;
use crate::services::effects::propulsion::apply_propulsion;
pub(super) async fn cycle_begin_work(mut cycle: Cycle) {
    let cx = cycle.cx.clone();
    let master = cycle.modules[0].id;
    let mut public = None;
    let mut check_target = false;
    let result = async {
        let fit = match Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, cx.ship_id).await {
            Ok(fit) => fit,
            Err(_) => return Err("fit unavailable"),
        };
        let resource_kind = charge_kind(cycle.effect.id, cycle.effect.kind);
        if charge_sensitive(resource_kind) && cycle.modules.iter().any(|m| state().reloading.get(&m.id).is_some_and(|until| *until > filetime_now())) {
            return Err("module reloading");
        }
        if charge_sensitive(resource_kind)
            && !crate::dao::items::pending_reloads::for_flags(&cx.db, cx.ship_id, &[cycle.modules[0].flag]).await.map_err(|_| "reload unavailable")?.is_empty() {
            return Err("module reloading");
        }
        let charge = if charge_sensitive(resource_kind) {
            loaded(&cx.db, cx.ship_id, cycle.modules[0].flag).await.map_err(|_| "charge unavailable")?.filter(|(_, quantity)| *quantity > 0)
        } else { None };
        if charge_sensitive(resource_kind) && charge.map(|(kind, _)| kind) != fit.source_type(Key::Charge(master)) {
            return Err("charge changed since fit snapshot");
        }
        if needs_charge(cycle.effect.kind) && charge.is_none() { return Err("out of charges"); }
        let charged = charge.is_some();
        if charge_sensitive(resource_kind) { cycle.charge_type = charge.map(|(kind, _)| kind); }
        if charge_sensitive(resource_kind) {
            cycle.other = charge.map(|(kind, _)| charge_key(cx.ship_id, cycle.modules[0].flag, kind));
        }
        cycle.duration_ms = cycle.effect.attribute(EffectAttribute::Duration).map(|a| fit.value(Key::Module(master), a)).unwrap_or(cycle.duration_ms);
        if cycle.effect.kind != EffectKind::Entosis { cycle.duration_ms = cycle.duration_ms.max(MIN_MODULE_DURATION_MS); }
        let entosis_proof = if cycle.effect.kind == EffectKind::Entosis {
            Some(entosis::prepare(&cx, &fit, &cycle).await.map_err(|_| "Entosis admission refused")?)
        } else { None };
        if matches!(cycle.effect.kind, EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu)
            && capacitor::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, cycle.target).await.is_err() {
            check_target = true;
            return Err("target lost");
        }
        if matches!(cycle.effect.kind, EffectKind::Ecm | EffectKind::BurstJammer) && ecm::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, cycle.target).await.is_err() { check_target = true; return Err("target lost"); }
        let discharge = cycle.effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED);
        let need = cycle_discharge(resource_kind, charged, cycle.modules.iter().map(|m| fit.value(Key::Module(m.id), discharge)).sum());
        if !drain_capacitor(&cx, &fit, need) {
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
        if let Err(error) = fuel::consume_cycle(&cx, &fit, &cycle.modules).await {
            if let Err(refund) = refund_refused_charge(&cx, need).await {
                warn!("module {master} capacitor refund failed: {}", match refund { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            }
            match error {
                CallError::User { msg, dict } => cx.notify("OnEffectMessage", vec![Value::str(msg), Value::Dict(dict.into_iter().map(|(key, value)| (Value::str(key), value)).collect())]),
                CallError::Internal(error) => warn!("module {master} fuel consumption failed: {error:#}"),
            }
            return Err("fuel unavailable");
        }
        if cycle.effect.kind == EffectKind::Propulsion {
            apply_propulsion(&cx, master, cycle.stop.clone(), true).await;
        }
        let start_result = if let Some(proof) = entosis_proof {
            if let Some(start) = entosis::begin(&cx, proof, cycle.duration_ms).await {
                cycle.nominal_start = start;
                if cycle.cycles == 0 {
                    crate::services::crimewatch::weapons_activate(&cx.world, cx.char_id);
                }
                Ok(CycleOutcome::Continue)
            } else { Ok(CycleOutcome::Stop("Entosis link refused")) }
        } else { on_cycle_start(&cx, &fit, &mut cycle, charged).await };
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
        if let Err(e) = heat::connect_cycle(&cx, &fit, &cycle).await {
            warn!("module {master} heat connection failed: {}", match e { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            return Err("heat connection failed");
        }
        if cycle.effect.kind == EffectKind::Entosis && cycle.cycles == 0
            && !entosis::publish_start(&cx, &cycle).await {
            return Err("Entosis link invalidated");
        }
        if cycle.effect.kind == EffectKind::Smartbomb || cycle.cycles == 0 && matches!(cycle.effect.kind, EffectKind::Bomb | EffectKind::FueledLocalRepair { .. } | EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster | EffectKind::Ecm | EffectKind::BurstJammer) {
            weapon_fx(&cx, &cycle.modules, &cycle.effect, cycle.target, cycle.charge_type, true, cycle.duration_ms, cycle.repeat);
        }
        Ok(fit)
    }.await;
    match result {
        Ok(fit) => {
            cycle.cycles += 1;
            let (start, duration) = (cycle.nominal_start, cycle.duration_ms);
            cx.world.post(cx.system_id, move |s| {
                schedule_cycle_end(s, start, duration, move |s, end| cycle_end(s, cycle, fit, end));
            });
        }
        Err(reason) => {
            debug!("module {master} stopped: {reason}");
            cx.world.post(cx.system_id, move |s| {
                if cycle_source_current(s, &cycle) {
                    if let Some(outcome) = public { publish_cycle(&cycle, outcome); }
                    else if check_target && cycle_target_lost(s, &cycle) { publish_cycle(&cycle, ModuleCycleOutcome::TargetLost); }
                }
                cycle_stop(s, cycle);
            });
        }
    }
}
