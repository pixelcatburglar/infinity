//! Local and remote shield, armor and hull repair phases.
//! Local phases mirror client dogma; remote phases use the server's matching
//! per-layer policy. Fitted SDE attributes determine amounts and range.
//! Fueled ancillary armor uses its charge multiplier; fueled shield bypasses
//! capacitor discharge. Mutadaptive remote repair remains unsupported.
use crate::services::{combat::Ctx, fit::{Fit, Key}, CallError};
use infinity_core::consts::{dogma::ATTR_SHIELD_BONUS, dogma::ATTR_ARMOR_DAMAGE_AMOUNT, dogma::ATTR_STRUCTURE_DAMAGE_AMOUNT};

pub(super) fn amount_attribute(layer: usize) -> i32 {
    match layer {
        0 => ATTR_SHIELD_BONUS,
        1 => ATTR_ARMOR_DAMAGE_AMOUNT,
        2 => ATTR_STRUCTURE_DAMAGE_AMOUNT,
        _ => unreachable!("invalid repair layer"),
    }
}

pub(super) fn scaled_amount(base: f64, gap: f64, optimal: f64, falloff: f64) -> f64 {
    base * super::range::range_multiplier(gap, optimal, falloff)
}

/// Validate a locked target against one interpolated system snapshot.
pub(super) async fn target(cx: &Ctx, target: Option<i64>, optimal: f64, falloff: f64) -> Result<(i64, Option<i64>, f64), crate::services::CallError> {
    use crate::services::{combat::targeting_charges::targets_of, user_error};
    use infinity_core::consts::{CATEGORY_DRONE, CATEGORY_ENTITY, CATEGORY_SHIP};
    use infinity_core::messages;
    use crate::space::{dist, MODE_WARP};
    let missing = || user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]);
    let id = target.filter(|id| *id != cx.ship_id && targets_of(cx.char_id).contains(id)).ok_or_else(missing)?;
    let source = cx.ship_id;
    let Some((ship, victim, npc, drone)) = cx.world.with(cx.system_id, move |s| {
        Some((s.sampled_ball(source)?, s.sampled_ball(id)?, s.is_npc(id), s.drone(id).is_some()))
    }).await.flatten() else { return Err(missing()); };
    if ship.mode == MODE_WARP || victim.mode == MODE_WARP {
        return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![]));
    }
    let category = cx.world.sde().types.get(victim.type_id).and_then(|t| t.category_id);
    if category != Some(CATEGORY_SHIP) && !(category == Some(CATEGORY_ENTITY) && npc)
        && !(category == Some(CATEGORY_DRONE) && drone) { return Err(missing()); }
    let gap = (dist(ship.pos, victim.pos) - f64::from(ship.radius) - f64::from(victim.radius)).max(0.0);
    let multiplier = super::range::range_multiplier(gap, optimal, falloff);
    if multiplier <= 0.0 || !multiplier.is_finite() { return Err(missing()); }
    Ok((id, victim.char_id, multiplier))
}

pub(crate) fn applies_at_start(layer: usize) -> bool { layer == 0 }

pub(super) fn amount(fit: &Fit, module: i64, amount_attr: i32) -> f64 {
    fit.value(Key::Module(module), amount_attr)
}

fn fueled_amount(base: f64, charged: bool, bonus: Option<f64>) -> f64 {
    if charged { base * bonus.unwrap_or(1.0) } else { base }
}

pub(super) fn fueled(fit: &Fit, module: i64, amount_attr: i32, bonus_attr: Option<i32>, charged: bool) -> f64 {
    fueled_amount(amount(fit, module, amount_attr), charged, bonus_attr.map(|attr| fit.value(Key::Module(module), attr)))
}

pub(super) async fn apply(cx: &Ctx, cycle: &super::lifecycle::Cycle, layer: usize, amount: f64, start: bool) -> Result<(), CallError> {
    if applies_at_start(layer) != start { return Ok(()); }
    let module = cycle.modules[0].id;
    let generation = cycle.stop.clone();
    let ship = cx.ship_id;
    let character = cx.char_id;
    crate::services::combat::damage::repair_target(&cx.world, cx.system_id, ship, layer, amount, crate::space::combat::RepairSource::Local, move |s| {
        crate::services::combat::state().active.get(&module).is_some_and(|active| std::sync::Arc::ptr_eq(&active.stop, &generation))
            && s.pilot_ball(character).is_some_and(|ball| ball.id == ship)
    }).await
}

#[cfg(test)] mod tests;
