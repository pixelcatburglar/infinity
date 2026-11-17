//! `/tr` destination parsing and dispatch. Mirrors the reference `tr` command
//! (`chat/client/slash_command.py` forwards the raw line; GM menus send
//! `/tr me <itemID>`): the destination is one positive ID, optionally preceded
//! by a case-insensitive `me`, and a trailing `noblock` token is accepted for
//! parity without bypassing any server check.
//! Stations resolve before live balls, then live balls in this system,
//! SDE solar systems, non-station celestials and finally stored space items
//! whose location is a real SDE system with finite coordinates. SDE data comes
//! from the startup snapshot; live positions come from the system actor.
//! Name lookup, coordinates, offsets, bookmarks, agents, structures and
//! teleporting other pilots are unsupported; from a station only station
//! destinations are accepted.

use super::{db_err, slash_error, CallError, Client};
use infinity_core::consts::GROUP_PLANET;
use infinity_core::messages;
use crate::services::ship::transport::{relocate, Destination};

/// Server design: clearance in metres when teleporting to an item.
const TELEPORT_CLEARANCE: f64 = 5000.0;
/// Server design: clearance in metres when arriving next to a system celestial.
const SYSTEM_CLEARANCE: f64 = 100_000.0;

/// The one positive destination ID: `/tr <id>` or `/tr me <id>`, either with
/// an optional trailing `noblock`. A leading or embedded `noblock` and any
/// other extra argument are usage errors.
fn destination_id(args: &[String]) -> Result<i64, CallError> {
    let words = match args.split_last() {
        Some((last, rest)) if last.eq_ignore_ascii_case("noblock") => rest,
        _ => args,
    };
    let dest = match words {
        [me, dest] if me.eq_ignore_ascii_case("me") => dest.as_str(),
        [dest] if !dest.eq_ignore_ascii_case("me") => dest.as_str(),
        _ => return Err(slash_error(messages::gm::GM_TELEPORT_USAGE.into())),
    };
    let id = dest.parse::<i64>().ok().filter(|id| *id > 0)
        .ok_or_else(|| slash_error(messages::gm::gm_invalid_id(dest)))?;
    Ok(id)
}

/// /tr [me] <destination>: move the caller's active ship to the destination.
pub(super) async fn command(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    let id = destination_id(args)?;
    // An SDE station takes priority over a live station ball in this system.
    if c.sde.map.station(id).is_some() {
        relocate(c, Destination::Station(id)).await?;
        return Ok(messages::gm::gm_teleported(id));
    }
    let here = c.session.int("solarsystemid").unwrap_or(0);
    // Something in this system's ballpark.
    if let Some((ball_pos, radius)) = c.world.with(here, move |s| s.ball(id).map(|b| (b.position_at(s.now_ms()), b.radius))).await.flatten() {
        let pos = [ball_pos[0] + radius as f64 + TELEPORT_CLEARANCE, ball_pos[1], ball_pos[2]];
        relocate(c, Destination::Space { system: here, position: pos }).await?;
        return Ok(messages::gm::gm_teleported(id));
    }
    // A solar system: land at a random celestial, planets first.
    if c.sde.map.system(id).is_some() {
        // SQL's descending boolean order puts NULL group IDs before planets.
        let priority = c.sde.map.celestials(id)
            .map(|p| (p.group_id.is_none(), p.group_id == Some(i64::from(GROUP_PLANET))))
            .max();
        let candidates = || c.sde.map.celestials(id).filter(|p| {
            Some((p.group_id.is_none(), p.group_id == Some(i64::from(GROUP_PLANET)))) == priority
        });
        let count = candidates().count();
        let p = candidates().nth((super::super::rand01() * count as f64) as usize)
            .ok_or_else(|| slash_error(messages::gm::gm_unknown_system(id)))?;
        let p = (p.x.expect("celestial x"), p.y.expect("celestial y"), p.z.expect("celestial z"), f64::from(c.sde.celestial_radius(p)));
        let pos = [p.0 + p.3 + SYSTEM_CLEARANCE, p.1, p.2];
        relocate(c, Destination::Space { system: id, position: pos }).await?;
        return Ok(messages::gm::gm_teleported_to_system(id));
    }
    // A planet, moon, gate... (stations already matched above).
    if let Some(p) = c.sde.map.celestial(id) {
        let pos = [p.x.expect("celestial x") + f64::from(c.sde.celestial_radius(p)) + TELEPORT_CLEARANCE, p.y.expect("celestial y"), p.z.expect("celestial z")];
        relocate(c, Destination::Space { system: p.solar_system_id.expect("celestial system"), position: pos }).await?;
        return Ok(messages::gm::gm_teleported(id));
    }
    // A character in space, or an item floating somewhere in a real system.
    let at = crate::dao::items::social::position(&c.db, id).await.map_err(db_err)?;
    if let Some(r) = at {
        if c.sde.map.system(r.location_id).is_some() && r.x.is_finite() && r.y.is_finite() && r.z.is_finite() {
            let system = r.location_id;
            let pos = [r.x + TELEPORT_CLEARANCE, r.y, r.z];
            let live = c.world.with(system, move |s| s.pilot_ball(id).or_else(|| s.ball(id)).map(|b| b.position_at(s.now_ms()))).await.flatten();
            let pos = live.map(|p| [p[0] + TELEPORT_CLEARANCE, p[1], p[2]]).unwrap_or(pos);
            relocate(c, Destination::Space { system, position: pos }).await?;
            return Ok(messages::gm::gm_teleported_in_system(id, system));
        }
    }
    Err(slash_error(messages::gm::gm_target_not_found(id)))
}

#[cfg(all(test, feature = "tests-slash"))]
mod tests;
