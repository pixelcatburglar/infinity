//! Cargo, ship and survey scanner cycles.
//! shipUI.OnCargoScanComplete and OnShipScanCompleted consume type/quantity pairs.
//! Inventory comes from game.items; capacitor comes from combat's live snapshot.
//! Survey results use the separately client-triggered miningScanMgr.perform_scan RPC;
//! the modern overlay has no server-pushed survey result callback.
use super::{EffectKind, lifecycle::{Cycle, CycleOutcome, Module, subject_valid}, registry::{Effect, EffectAttribute, EffectFlag}, notify::{fire_fx, cargo_scan_complete, ship_scan_completed}};
use crate::consts::{dogma::ATTR_MAX_RANGE, CATEGORY_SHIP, FLAG_CARGO};
use crate::messages;
use crate::services::{combat::{Ctx, state, targeting_charges::targets_of, groups_health::capacitor_snapshot}, fit::{Fit, Key}, items::load_items, user_error, db_err, CallError};
use crate::dao::items::loading::ItemQuery;
use crate::space::{dist, System, MODE_WARP};
use std::sync::Arc;
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

fn scan_range(fit: &Fit, module: &Module, effect: &Effect) -> f64 {
    fit.value(Key::Module(module.id), effect.attribute(EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE))
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    if effect.kind == EffectKind::SurveyScan { return Ok(()); }
    let target = target.filter(|t| targets_of(cx.char_id).contains(t)).ok_or_else(|| user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]))?;
    let (source, range, sde) = (cx.ship_id, scan_range(fit, module, effect), cx.world.sde().clone());
    if !cx.world.with(cx.system_id, move |s| in_range(s, source, target, range, &sde)).await.unwrap_or(false) {
        return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
    }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if !subject_valid(cx, &cycle.modules[0], &cycle.effect).await? { return Ok(CycleOutcome::Stop("subject lost")); }
    if validate(cx, fit, &cycle.modules[0], &cycle.effect, cycle.target).await.is_err() { return Ok(super::lifecycle::target_loss(cycle).await); }
    if cycle.cycles == 0 {
        if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) {
            let (modules, ship, target, duration, repeat, offensive) = (cycle.modules.clone(), cx.ship_id, cycle.target, cycle.duration_ms, cycle.repeat, cycle.effect.has(EffectFlag::Offensive));
            cx.world.broadcast(cx.system_id, ship, move |stamp| modules.iter().map(|m| fire_fx(stamp, ship, Some((m.id, m.type_id)), &guid, target, None, offensive, true, duration, repeat)).collect());
        }
    }
    Ok(CycleOutcome::Continue)
}

pub(super) async fn end(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if cycle.effect.kind == EffectKind::SurveyScan { return Ok(CycleOutcome::Ended { reason: "scan complete", outcome: ModuleCycleOutcome::Completed }); }
    let Some(target) = cycle.target else { return Ok(CycleOutcome::Stop("target lost")); };
    if !subject_valid(cx, &cycle.modules[0], &cycle.effect).await? { return Ok(CycleOutcome::Stop("subject lost")); }
    if load_items(&cx.db, cx.world.sde(), ItemQuery::Id(target)).await.map_err(db_err)?.into_iter().next().is_none_or(|item| item.category_id != i64::from(CATEGORY_SHIP)) { return Ok(super::lifecycle::target_loss(cycle).await); }
    let items = load_items(&cx.db, cx.world.sde(), ItemQuery::Location(target)).await.map_err(db_err)?;
    let cargo = cycle.effect.kind == EffectKind::CargoScan;
    let rows: Vec<(i32, i64)> = items.into_iter().filter(|item| visible_flag(cargo, item.flag_id)).map(|item| (item.type_id, i64::from(item.quantity))).collect();
    let cap = if cargo { None } else { capacitor_snapshot(&cx.db, &cx.world, cx.system_id, target).await? };
    if !cargo && cap.is_none() { return Ok(super::lifecycle::target_loss(cycle).await); }
    let (source, character, master, generation, range, sde, notify) = (cx.ship_id, cx.char_id, cycle.modules[0].id, cycle.stop.clone(), scan_range(fit, &cycle.modules[0], &cycle.effect), cx.world.sde().clone(), cx.clone());
    let applied = cx.world.with(cx.system_id, move |s| {
        let active = state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation));
        if !active || !targets_of(character).contains(&target) || !in_range(s, source, target, range, &sde) { return false; }
        if let Some(cap) = cap { ship_scan_completed(&notify, target, cap.level, cap.capacity, rows); }
        else { cargo_scan_complete(&notify, target, rows); }
        true
    }).await.unwrap_or(false);
    Ok(if applied { CycleOutcome::Ended { reason: "scan complete", outcome: ModuleCycleOutcome::Completed } } else { CycleOutcome::Stop("scan complete") })
}

pub(super) fn stop(s: &mut System, cx: &Ctx, cycle: &Cycle) {
    if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()) {
        let events = cycle.modules.iter().map(|m| fire_fx(s.current_time(), cx.ship_id, Some((m.id, m.type_id)), guid, cycle.target, None, cycle.effect.has(EffectFlag::Offensive), false, cycle.duration_ms, cycle.repeat)).collect();
        s.broadcast(cx.ship_id, events);
    }
}

#[cfg(test)] mod tests;
