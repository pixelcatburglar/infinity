//! GM turret hit-chance diagnostics using native GetAccuracy inputs.
//! The slash client forwards this command; fitted attributes come from game
//! data and the SDE, while the system actor supplies both sampled balls.
//! Missing balls are refused; this command does not fire or modify weapons.
use super::{db_err, slash_error, CallError, Client};
use infinity_core::consts::{dogma::ATTR_FALLOFF, dogma::ATTR_MAX_RANGE, dogma::ATTR_OPTIMAL_SIG_RADIUS, dogma::ATTR_TRACKING_SPEED};
use infinity_core::messages;
use crate::services::effects::{registry, weapons::{target_signature, turret_chance, turret_geometry}, EffectKind};
use crate::services::fit::{Fit, Key};

fn parse(args: &[String]) -> Option<i64> {
    let [target] = args else { return None };
    target.parse::<i64>().ok().filter(|id| *id > 0)
}


pub(super) async fn command(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    let target = parse(args).ok_or_else(|| slash_error(messages::gm::GM_HITCHANCE_USAGE.into()))?;
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Err(slash_error(messages::gm::GM_HITCHANCE_UNDOCK.into()));
    };
    let modules = crate::dao::items::fit::modules(&c.db, ship).await.map_err(db_err)?;
    let fit = Fit::load_in_space(&c.db, &c.world, system, char_id, ship).await?;
    let Some((src, dst)) = c.world.with(system, move |s| Some((s.sampled_ball(ship)?, s.sampled_ball(target)?))).await.flatten() else {
        return Err(slash_error(messages::gm::GM_HITCHANCE_MISSING_BALL.into()));
    };
    let (center, surface, speed, angular_squared) = turret_geometry(&src, &dst);
    let target_sig = target_signature(c.world.sde(), &dst);
    let mut rows = Vec::new();
    for module in modules {
        let key = Key::Module(module.item_id);
        if fit.source_type(key) != Some(module.type_id) || !c.sde.types.effects(module.type_id).iter()
            .any(|effect| registry::kind(&c.sde, effect.effect_id) == Some(EffectKind::Turret)) { continue; }
        let optimal = fit.value(key, ATTR_MAX_RANGE);
        let falloff = fit.value(key, ATTR_FALLOFF);
        let tracking = fit.value(key, ATTR_TRACKING_SPEED);
        let opt_sig = fit.value(key, ATTR_OPTIMAL_SIG_RADIUS);
        let chance = turret_chance(&src, &dst, optimal, falloff, tracking, opt_sig, target_sig);
        rows.push((module.item_id, messages::gm::gm_hitchance_row(module.item_id, module.type_id, center, surface, speed, angular_squared.sqrt(), angular_squared, tracking, opt_sig, target_sig, optimal, falloff, chance)));
    }
    rows.sort_unstable_by_key(|row| row.0);
    if rows.is_empty() { Ok(messages::gm::GM_HITCHANCE_EMPTY.into()) }
    else { Ok(rows.into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("\n")) }
}

#[cfg(test)] mod tests;
