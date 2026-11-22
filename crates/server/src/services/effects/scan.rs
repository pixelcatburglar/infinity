//! Cargo, ship and survey scanner cycles.
//! shipUI.OnCargoScanComplete and OnShipScanCompleted consume type/quantity pairs.
//! Inventory comes from game.items; capacitor comes from combat's live snapshot.
//! Survey results use the separately client-triggered miningScanMgr.perform_scan RPC;
//! the modern overlay has no server-pushed survey result callback.
//! Effect animations are published through the system's ordered scene channel.
use super::{EffectKind, lifecycle::{cycle_subject_valid, source_subject_current, Cycle, CycleOutcome, Module}, parameters::MasterPacket, registry::{Effect, EffectFlag}, notify::{cargo_scan_complete, ship_scan_completed}};
use infinity_core::consts::{CATEGORY_SHIP, FLAG_CARGO};
use infinity_core::messages;
use crate::services::{combat::{Ctx, targeting_charges::targets_of, groups_health::capacitor_snapshot}, fit::Fit, items::load_items, user_error, db_err, CallError};
use crate::dao::items::loading::ItemQuery;
use crate::space::{dist, FxModule, System, MODE_WARP, system::presentation::{FxRetention, ModuleFx}};
use marshal::Value;
use crate::lua::events::ModuleCycleOutcome;


fn in_range(s: &System, source: i64, target: i64, range: f64, sde: &crate::sde::Sde) -> bool {
    if !range.is_finite() || range < 0.0 { return false; }
    let Some((a, b)) = s.sampled_ball(source).zip(s.sampled_ball(target)) else { return false; };
    a.mode != MODE_WARP && b.mode != MODE_WARP && source != target
        && sde.types.get(b.type_id).and_then(|t| t.category_id) == Some(CATEGORY_SHIP)
        && (dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0) <= range
}

fn visible_flag(cargo: bool, flag: i32) -> bool {
    if cargo { flag == FLAG_CARGO } else { crate::services::inventory::is_fitting_flag(flag) }
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    if effect.kind == EffectKind::SurveyScan { return Ok(()); }
    let target = target.filter(|t| targets_of(cx.char_id).contains(t)).ok_or_else(|| user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]))?;
    let (source, range, sde) = (cx.ship_id, super::tractor::range(fit, module, effect), cx.world.sde().clone());
    if !cx.world.with(cx.system_id, move |s| in_range(s, source, target, range, &sde)).await.unwrap_or(false) {
        return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
    }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if !cycle_subject_valid(cycle).await { return Ok(CycleOutcome::Stop("subject lost")); }
    if cycle.effect.kind != EffectKind::SurveyScan {
        let Some(MasterPacket::Scan(inputs)) = cycle.inputs.frame.as_ref().map(|frame| &frame.packet) else {
            return Err(CallError::Internal(anyhow::anyhow!("scan cycle captured without scan inputs")));
        };
        let Some(target) = cycle.target.filter(|t| targets_of(cx.char_id).contains(t)) else {
            return Ok(super::lifecycle::target_loss(cycle).await);
        };
        let (source, sde, range) = (cx.ship_id, cx.world.sde().clone(), inputs.range);
        if !cx.world.with(cx.system_id, move |s| in_range(s, source, target, range, &sde)).await.unwrap_or(false) {
            return Ok(super::lifecycle::target_loss(cycle).await);
        }
    }
    if cycle.cycles == 0 {
        if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) {
            let (modules, master, generation, duration, repeat, offensive) = (cycle.modules.clone(), cycle.modules[0].id, cycle.stop.clone(), cycle.duration_ms, cycle.repeat, cycle.effect.has(EffectFlag::Offensive));
            cx.world.post(cx.system_id, move |s| {
                for m in &modules {
                    s.publish_module_fx(master, &generation, ModuleFx {
                        module: FxModule { item: Some(m.id), type_id: m.type_id },
                        guid: guid.clone(), charge: None, offensive,
                        duration_ms: duration, repeat, graphic_info: Value::None,
                    }, true, FxRetention::Current);
                }
            });
        }
    }
    Ok(CycleOutcome::Continue)
}

pub(super) async fn end(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if cycle.effect.kind == EffectKind::SurveyScan { return Ok(CycleOutcome::Ended { reason: "scan complete", outcome: ModuleCycleOutcome::Completed }); }
    let Some(target) = cycle.target else { return Ok(CycleOutcome::Stop("target lost")); };
    if load_items(&cx.db, cx.world.sde(), ItemQuery::Id(target)).await.map_err(db_err)?.into_iter().next().is_none_or(|item| item.category_id != i64::from(CATEGORY_SHIP)) { return Ok(super::lifecycle::target_loss(cycle).await); }
    let items = load_items(&cx.db, cx.world.sde(), ItemQuery::Location(target)).await.map_err(db_err)?;
    let cargo = cycle.effect.kind == EffectKind::CargoScan;
    let rows: Vec<(i32, i64)> = items.into_iter().filter(|item| visible_flag(cargo, item.flag_id)).map(|item| (item.type_id, i64::from(item.quantity))).collect();
    let cap = if cargo { None } else { capacitor_snapshot(&cx.db, &cx.world, cx.system_id, target).await? };
    if !cargo && cap.is_none() { return Ok(super::lifecycle::target_loss(cycle).await); }
    let Some(MasterPacket::Scan(inputs)) = cycle.inputs.frame.as_ref().map(|frame| &frame.packet) else {
        return Err(CallError::Internal(anyhow::anyhow!("scan cycle captured without scan inputs")));
    };
    let Some(membership) = cycle.source_membership else { return Ok(CycleOutcome::Stop("subject lost")); };
    let (source, character, module, effect_id, generation, range, sde, notify) =
        (cx.ship_id, cx.char_id, cycle.modules[0].clone(), cycle.presented_effect, cycle.stop.clone(), inputs.range, cx.world.sde().clone(), cx.clone());
    let applied = cx.world.with(cx.system_id, move |s| {
        if !source_subject_current(s, source, character, membership, &module, effect_id, &generation)
            || !targets_of(character).contains(&target) || !in_range(s, source, target, range, &sde) { return false; }
        if let Some(cap) = cap { ship_scan_completed(&notify, target, cap.level, cap.capacity, rows); }
        else { cargo_scan_complete(&notify, target, rows); }
        true
    }).await.unwrap_or(false);
    Ok(if applied { CycleOutcome::Ended { reason: "scan complete", outcome: ModuleCycleOutcome::Completed } } else { CycleOutcome::Stop("scan complete") })
}

pub(super) fn stop(s: &mut System, _cx: &Ctx, cycle: &Cycle) {
    s.retire_fx_activation(cycle.modules[0].id, &cycle.stop);
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
