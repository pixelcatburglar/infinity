//! GM ship damage control; condition is held by combat's in-memory cache.
//! Client dogma receives attribute changes and destiny health updates.
//! This command operates on the caller's active ship only.

use super::{slash_error, CallError, Client};
use infinity_core::messages;
use crate::services::fit::PERCENT_SCALE;

pub(super) async fn command(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    debug_assert_eq!(command, "dmg");
    let (layers, fraction) = parse(args).ok_or_else(|| slash_error(messages::gm::GM_DAMAGE_USAGE.into()))?;
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Err(slash_error(messages::GM_UNDOCK_FIRST.into()));
    };
    super::super::combat::groups_health::set_damage(c, system, ship, char_id, layers, fraction).await?;
    Ok(messages::gm::GM_DAMAGE_SET.into())
}

fn parse(args: &[String]) -> Option<([bool; 3], f64)> {
    let [layer, _] = args else { return None };
    let layers = match layer.as_str() {
        "shield" => [true, false, false], "armor" => [false, true, false],
        "hull" => [false, false, true], "all" => [true; 3], _ => return None,
    };
    let percent = super::args::Args::new(args).f64(1).ok()?;
    if !(0.0..=PERCENT_SCALE).contains(&percent) { return None; }
    Some((layers, percent / PERCENT_SCALE))
}

#[cfg(test)] mod tests;
