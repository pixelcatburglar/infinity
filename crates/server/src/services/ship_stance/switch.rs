//! The `SetShipStance` mechanic behind `shipStanceMgr`.
//! Mirrors `shipmode/shipstance.py`: selecting the current stance is a no-op,
//! a different stance deletes the old hidden mode item, creates the new one
//! and applies it immediately through the shared fitting boundary. The
//! persisted `ready_at` deadline is the cooldown; its duration is the
//! effective dogma attribute `stanceSwitchTime` read from the resident fit in
//! space and the stored fit while docked. One durable transaction per
//! accepted call, and the RPC returns `None`, which the client ignores.
//! There is no timer, cache or Lua side.
use std::sync::Arc;
use anyhow::anyhow;
use infinity_core::consts::{dogma::ATTR_STANCE_SWITCH_TIME, CATEGORY_SHIP, FLAG_HANGAR};
use infinity_core::messages;
use marshal::Value;
use tracing::info;
use crate::client::{filetime_now, filetime_ticks_from_ms, ms_from_filetime_ticks};
use crate::dao::characters::selection;
use crate::dao::items::{fitting::destroy_upgrade, loading::ItemQuery, ship::lock_reload_ship, ship_stance::{replace_timing, Timing}};
use crate::db::{outbox::{Notification, Outbox}, Db};
use crate::sde::{ship_stances, Sde};
use crate::services::{db_err, notify_error, CallError, CallResult};
use crate::services::combat::loaded::{settle_in, LoadedHolds};
use crate::services::effects::online::{capture_online_fences, dispatch_entries, fitting, OnlineChanged};
use crate::services::fit::{Fit, Key};
use crate::services::identity::Identity;
use crate::services::inventory::notifications::notify_owner_item_removed;
use crate::services::items::load_items;
use crate::space::World;
use super::{state, StanceTransition};

/// `shipmode/shipstance.py` admits a switch at the exact cooldown deadline.
fn ready(now: i64, ready_at: i64) -> bool { now >= ready_at }

/// `now + ms` in FILETIME ticks; `None` for any duration that cannot become a
/// real deadline (non-finite, negative or overflowing the tick range).
fn deadline(now: i64, ms: f64) -> Option<i64> {
    if !(ms.is_finite() && ms >= 0.0 && ms <= ms_from_filetime_ticks(i64::MAX) as f64) { return None; }
    now.checked_add(filetime_ticks_from_ms(ms))
}

/// One authorized `SetShipStance`: a manual durable item operation sequenced
/// like `set_online`: fences, loaded-holds quiesce, the market lifecycle lock
/// and snapshot gates, one transaction, then post-commit dispatch.
pub(in crate::services) async fn set_stance(
    identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, login_user: i64,
    ship: i64, stance: i64,
    notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> CallResult {
    if identity.char_id == 0 || identity.ship_id != Some(ship) {
        return Err(notify_error(messages::ship_stance::INVALID_REQUEST));
    }
    let now = filetime_now();
    let char_id = identity.char_id;
    let station = identity.station_id;
    // An in-space caller owns the admission of its captured system before any
    // fencing; a docked caller keeps the membership-or-station admission.
    let admission = match (identity.system_id, station) {
        (Some(system), None) => Some((system, world.admission(system).map_err(db_err)?)),
        _ => None,
    };
    let session = world.online_session(char_id);
    let fences = capture_online_fences(world, &[char_id]).await;
    let membership = fences.membership(ship);
    let admitted = match &admission {
        Some((system, _)) => {
            membership.is_some_and(|(m, _)| m == *system)
                && session.as_ref().is_some_and(|s| s.character_id == char_id && s.ship_id == Some(ship)
                    && s.system_id == Some(*system) && s.station_id.is_none())
        }
        None => station.is_some(),
    };
    if !admitted {
        return Err(notify_error(messages::ship_stance::INVALID_REQUEST));
    }
    let armed = LoadedHolds::arm(world, &membership.map(|(system, _)| (system, ship)).into_iter().collect::<Vec<_>>(), notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let _lock = crate::services::market::lock().await;
    let mut holds = LoadedHolds::lock(world, armed, notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    // The captured actor must not have failed while arming, and the caller's
    // session generation must still be the live one.
    if let Some((_, admission)) = &admission {
        admission.check().map_err(db_err)?;
        if world.online_session(char_id) != session {
            return Err(notify_error(messages::ship_stance::INVALID_REQUEST));
        }
    }
    // In space the effective cooldown duration is the resident fit's scalar
    // read inside the same locks and the same actor read that proves the
    // caller's pilot ball is still this hull; a stale or absent record
    // refuses rather than falling back to a cold fit.
    let space_ms = match membership {
        Some((system, generation)) => {
            match world.with(system, move |s| (
                s.pilot_ball(char_id).is_some_and(|b| b.id == ship && b.owner_id == char_id && b.char_id == Some(char_id)),
                s.with_combat_fit(ship, |fit, _, stamp| (fit.value(Key::Ship, ATTR_STANCE_SWITCH_TIME), stamp.membership)),
            )).await {
                Some((true, Some((ms, observed)))) if observed == generation => Some(ms),
                _ => return Err(notify_error(messages::ship_stance::INVALID_REQUEST)),
            }
        }
        None => None,
    };
    let settle = holds.settle_rows();
    let sde = sde.clone();
    holds.may_execute();
    let result = db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            let settle = settle.clone();
            Box::pin(async move {
                selection::lock_character(tx, char_id).await.map_err(db_err)?;
                if selection::ship_id(tx, char_id).await.map_err(db_err)? != Some(ship) {
                    return Err(notify_error(messages::ship_stance::INVALID_REQUEST));
                }
                if !lock_reload_ship(tx, ship).await.map_err(db_err)? {
                    return Err(notify_error(messages::ship_stance::INVALID_REQUEST));
                }
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next()
                    .ok_or_else(|| notify_error(messages::ship_stance::INVALID_REQUEST))?;
                let located = match membership {
                    Some((system, _)) => hull.location_id == system,
                    None => Some(hull.location_id) == station && hull.flag_id == FLAG_HANGAR,
                };
                if hull.owner_id != char_id || hull.category_id != i64::from(CATEGORY_SHIP) || !hull.singleton || !located {
                    return Err(notify_error(messages::ship_stance::INVALID_REQUEST));
                }
                settle_in(tx, &settle).await?;
                let Some(snapshot) = state::ensure_initialized_tx(tx, &sde, outbox, &hull).await? else {
                    return Err(notify_error(messages::ship_stance::UNSUPPORTED_HULL));
                };
                let Some(new_type) = ship_stances::mode_type(hull.type_id, stance) else {
                    return Err(notify_error(messages::ship_stance::INVALID_REQUEST));
                };
                // Same stance returns without deleting the item, sending any
                // notification or resetting the deadline, even mid-cooldown.
                if stance == snapshot.current { return Ok(false); }
                if !ready(now, snapshot.ready_at) {
                    return Err(notify_error(messages::ship_stance::SWITCH_NOT_READY));
                }
                let before = Fit::load(tx, sde.clone(), char_id, ship).await?;
                let ms = space_ms.unwrap_or_else(|| before.value(Key::Ship, ATTR_STANCE_SWITCH_TIME));
                let ready_at = deadline(now, ms)
                    .ok_or_else(|| CallError::Internal(anyhow!("stance switch time {ms} ms cannot become a deadline")))?;
                let mut boundary = fitting::capture_from_fit(tx, &sde, &fences, char_id, ship, before).await?;
                if !destroy_upgrade(tx, &snapshot.item).await.map_err(db_err)? {
                    return Err(notify_error(messages::ship_stance::INVALID_REQUEST));
                }
                notify_owner_item_removed(outbox, snapshot.item, char_id);
                let new_item = state::create_mode_item(tx, &sde, outbox, &hull, new_type).await?;
                let expected = Timing { previous_stance_id: snapshot.previous_stance_id, switched_at: snapshot.switched_at, ready_at: snapshot.ready_at };
                let next = Timing { previous_stance_id: Some(snapshot.current), switched_at: now, ready_at };
                if !replace_timing(tx, ship, &expected, &next).await.map_err(db_err)? {
                    return Err(notify_error(messages::ship_stance::INVALID_REQUEST));
                }
                boundary.set_ship_stance(StanceTransition { old: snapshot.current, new: stance, switched_at: now, new_item: new_item.item_id });
                let final_fit = Fit::load(tx, sde.clone(), char_id, ship).await?;
                fitting::finish(tx, boundary, final_fit, outbox).await?;
                Ok(true)
            })
        },
        |changed, outbox| async move {
            dispatch_entries((), outbox, world, notify)?;
            if changed { info!(user = login_user, ship, stance, "ship stance switched"); }
            Ok(Value::None)
        },
    ).await;
    holds.finish(db, world, result, &[]).await
}

#[cfg(all(test, feature = "tests-services"))]
mod tests;
