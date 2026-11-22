//! Micro jump field generator (effect 6208 `microJumpPortalDrive`): at the end
//! of one spool cycle the source ship and every selected passenger move by the
//! same displacement. Mirrors `eve/common/script/dogma/pythonEffects/microJumpPortalDrive.py`
//! and the client hull sets of `inventorycommon/const.py`; the generic
//! high-security refusal of `packages/dogma/moduleRestrictions.py` lives in the
//! lifecycle's activation admission. Range, radius and cap come from the SDE
//! and the resident fit; selection and command queuing complete in one actor
//! turn with no database work. The relocation rides `mjd::queue_jump`, the
//! single SetBallPosition guard.
//! Not supported: the capital field generator (effect 12126), drones,
//! fighters, probes and bombs carried by the field, Crimewatch aggression on
//! carried ships, a server-side reactivation delay, the `disallowTethering`
//! modifier and the per-passenger jump flash.
use super::{lifecycle::{Cycle, CycleOutcome}, mjd};
use infinity_core::consts::{CATEGORY_SHIP, GROUP_CARRIER, GROUP_COMMAND_CARRIER, GROUP_FORCE_AUX,
    GROUP_DREADNOUGHT, GROUP_LANCER_DREADNOUGHT, GROUP_CAPITAL_INDUSTRIAL_SHIP, GROUP_TITAN,
    GROUP_SUPERCARRIER, GROUP_FREIGHTER, GROUP_JUMP_FREIGHTER, GROUP_INDUSTRIAL_COMMAND_SHIP};
use crate::services::{combat::Ctx, fit::Key, rand01, CallError};
use crate::sde::Sde;
use crate::space::{add, dist, scale, System, V3, MODE_WARP};
use crate::lua::events::ModuleCycleOutcome;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

/// Client dogma/const.py: attributeMicroJumpPortalRadius (mjfgRadius), metres.
const ATTR_MJFG_RADIUS: i32 = 2067;
/// Client dogma/const.py: attributeMicroJumpPortalMaxShipCandidates
/// (mjdShipJumpCap): passengers per activation; the source is not charged.
const ATTR_MJD_SHIP_JUMP_CAP: i32 = 2832;

/// Client inventorycommon/const.py `capitalShipsGroups` and
/// `superCapitalShipsGroups`, plus the freighter, jump freighter and
/// industrial command ship groups the field generator's type description
/// names as excluded.
const EXCLUDED_HULL_GROUPS: [i32; 11] = [
    GROUP_CARRIER, GROUP_COMMAND_CARRIER, GROUP_FORCE_AUX, GROUP_DREADNOUGHT,
    GROUP_LANCER_DREADNOUGHT, GROUP_CAPITAL_INDUSTRIAL_SHIP, GROUP_TITAN,
    GROUP_SUPERCARRIER, GROUP_FREIGHTER, GROUP_JUMP_FREIGHTER,
    GROUP_INDUSTRIAL_COMMAND_SHIP,
];

/// Only ship-category hulls outside the excluded groups ride the field;
/// capsules are ordinary ship-category hulls and are carried.
pub(super) fn hull_eligible(category: Option<i32>, group: Option<i32>) -> bool {
    category == Some(CATEGORY_SHIP) && group.is_some_and(|group| !EXCLUDED_HULL_GROUPS.contains(&group))
}

/// Pick up to `cap` passengers without replacement: a partial Fisher-Yates
/// over an injected uniform source keeps the choice deterministic under test.
/// A cap that covers every candidate keeps the input order and draws nothing.
pub(super) fn select_passengers<T>(mut candidates: Vec<T>, cap: usize, mut next: impl FnMut() -> f64) -> Vec<T> {
    let len = candidates.len();
    if cap >= len { return candidates; }
    for i in 0..cap {
        let j = i + (next().clamp(0.0, 1.0 - f64::EPSILON) * (len - i) as f64) as usize;
        candidates.swap(i, j);
    }
    candidates.truncate(cap);
    candidates
}

/// Eligible passengers inside the field radius: piloted ships that are not
/// the source, not warping or cloaked, on an eligible hull, inside the radius
/// centre to centre, and not micro-jump blocked in their resident fit. Each
/// entry carries the ball's sampled position and persistent membership.
fn candidates(s: &mut System, sde: &Sde, source: i64, center: V3, radius: f64) -> Vec<(i64, V3, u64)> {
    if !(radius.is_finite() && radius > 0.0) { return Vec::new(); }
    let pilots: Vec<(i64, i32, u8, u8)> = s.pilots().map(|ball| (ball.id, ball.type_id, ball.mode, ball.cloak)).collect();
    let mut found = Vec::new();
    for (id, type_id, mode, cloak) in pilots {
        if id == source || mode == MODE_WARP || cloak != 0 { continue; }
        let Some(data) = sde.types.get(type_id) else { continue; };
        if !hull_eligible(data.category_id, data.group_id) { continue; }
        let Some(pos) = s.sample_position(id) else { continue; };
        if dist(pos, center) > radius { continue; }
        if s.with_combat_fit(id, |fit, _, _| mjd::ship_blocked(fit)) != Some(false) { continue; }
        let Some(membership) = s.persistent_generation(id) else { continue; };
        found.push((id, pos, membership));
    }
    found.sort_unstable_by_key(|entry| entry.0);
    found
}

/// The field completes its single spool cycle: the source's jump queues first
/// and every selected passenger queues the same displacement from its own
/// position; a passenger moves only when the source's guard actually applied.
pub(super) async fn end(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = &cycle.modules[0];
    let ship = cx.ship_id;
    let has_row = mjd::has_range_row(&cx.world.sde(), module.type_id);
    let module = module.id;
    let admitted = cycle.source_membership;
    let generation = cycle.stop.clone();
    let sde = cx.world.sde().clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    cx.world.with(cx.system_id, move |s| {
        if !mjd::activation_live(module, &generation) { return; }
        let Some((range, radius, cap)) = s.with_combat_fit(ship, move |fit, _key, stamp| {
            if Some(stamp.membership) != admitted || mjd::blocked(fit, module) { return None; }
            Some((mjd::jump_range(fit, has_row, module)?,
                fit.value(Key::Module(module), ATTR_MJFG_RADIUS),
                fit.value(Key::Module(module), ATTR_MJD_SHIP_JUMP_CAP)))
        }).flatten() else { return };
        let Some(ball) = s.sampled_ball(ship).filter(|ball| ball.mode != MODE_WARP) else { return };
        let Some(membership) = s.persistent_generation(ship) else { return };
        let Some(heading) = s.heading(ship) else { return };
        let displacement = scale(heading, range);
        let cap = cap.max(0.0) as usize;
        let passengers = select_passengers(candidates(s, &sde, ship, ball.pos, radius), cap, rand01);
        let applied = Arc::new(AtomicBool::new(false));
        // Commands run in FIFO order at the next destiny boundary: the
        // source's guard settles before every passenger admit reads it.
        let source = applied.clone();
        mjd::queue_jump(s, ship, add(ball.pos, displacement), generation.clone(), membership, |_, _| true, move |ok| {
            source.store(ok, Ordering::Relaxed);
            let _ = tx.send(ok);
        });
        for (id, pos, membership) in passengers {
            let admit = applied.clone();
            mjd::queue_jump(s, id, add(pos, displacement), generation.clone(), membership,
                move |_, _| admit.load(Ordering::Relaxed), |_| {});
        }
    }).await;
    Ok(if rx.await.unwrap_or(false) {
        CycleOutcome::Ended { reason: "micro jump field completed", outcome: ModuleCycleOutcome::Completed }
    } else { CycleOutcome::Stop("micro jump field completed") })
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
