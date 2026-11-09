//! Mining scanner service over the system actor's committed asteroid projection.
//! Scanner range uses ship and rock positions sampled at the same system-actor time.
//! The modern overlay triggers this RPC itself; survey module cycles do not push results.
//! Online fitted survey scanners supply their effective range, otherwise the configured
//! integrated-scanner range is used.
use super::*;
/// SDE dgmAttributeTypes: surveyScanRange.
const ATTR_SURVEY_SCAN_RANGE: i32 = 197;

fn scanner_range(configured: f64, ranges: impl Iterator<Item = f64>) -> f64 {
    ranges.filter(|range| range.is_finite() && *range > 0.0).reduce(f64::max).unwrap_or(configured)
}
pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "perform_scan" => Some(Ok(perform_scan(c).await)),
        _ => None,
    }
}

/// miningScanMgr.perform_scan() -> [(asteroidID, typeID, quantity)] within reach.
async fn perform_scan(c: &Client) -> Value {
    let (Some(system), Some(ship), Some(character)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else { return Value::None };
    let Ok(fit) = Fit::load_in_space(&c.db, &c.world, system, character, ship).await else { return Value::None };
    let Ok(modules) = crate::services::items::load_items(&c.db, &c.sde, crate::dao::items::loading::ItemQuery::Location(ship)).await else { return Value::None };
    let Ok(online) = crate::dao::items::dogma::online(&c.db, ship).await else { return Value::None };
    let range = scanner_range(crate::config::game().mining.scanner_range_m, modules.iter()
        .filter(|item| item.owner_id == character && item.singleton && crate::services::inventory::is_fitting_flag(item.flag_id)
            && online.iter().any(|entry| entry.item_id == item.item_id)
            && c.sde.types.effects(item.type_id).iter().any(|effect| crate::services::effects::registry::kind(&c.sde, effect.effect_id) == Some(crate::services::effects::EffectKind::SurveyScan)))
        .map(|item| fit.value(Key::Module(item.item_id), ATTR_SURVEY_SCAN_RANGE)));
    c.world.with(system, move |s| s.rocks_in_range(ship, range)).await.flatten()
        .map(|rocks| Value::List(rocks.into_iter()
            .map(|(id, type_id, quantity)| Value::tuple([Value::Int(id), Value::Int(type_id as i64), Value::Int(quantity)]))
            .collect()))
        .unwrap_or(Value::None)
}

#[cfg(test)] mod tests;
