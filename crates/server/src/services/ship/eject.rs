//! Voluntary eject from a piloted hull in space through the `ship` service.
//! `menuFunctions.py` confirms, then calls `ship.Eject()` with no arguments
//! through `sessionMgr.PerformSessionChange`; the reply is ignored. A capsule
//! is created at the hull's sampled position and becomes the pilot's ship; the
//! hull stays in space, unpiloted, still owned by the pilot, keeping the
//! velocity sampled with its position and decelerating through ordinary
//! evolution, and survives logout and restart through the unpiloted-hull
//! query. Eject shares the pilot registry's ship-change deadline with the
//! other transitions.
//! Game items and characters hold custody and positions; the system actor
//! holds sampled balls, combat state and the transition fencing.
//! Boarding another pilot's unpiloted hull, implant-driven capsule variants,
//! the criminal-flag prohibition and in-structure `LeaveShip` are unsupported.
use super::{Call, CallError, Client};
use infinity_core::consts::{CATEGORY_SHIP, IX_LOCATION_ID, LOCATION_JUNKYARD, TYPE_CAPSULE};
use infinity_core::messages;
use crate::dao::{characters::selection, items::{boarding, creation, loading::ItemQuery, space_ships::FreeShip}};
use crate::services::{db_err, items::load_items, notify_error};
use crate::space::system::boarding::transitions::{self, PreparedOld, PreparedTarget, PreparedTransition};
use marshal::Value;
use tracing::info;

fn unavailable() -> CallError { notify_error(messages::ship::EJECT_UNAVAILABLE) }

/// Eject() -> shipState: the session switches to a new capsule created at the
/// hull's sampled position; the hull is retained unpiloted at that position.
pub(super) async fn eject(c: &mut Client, call: &Call<'_>) -> super::CallResult {
    if !call.args.is_empty() { return Err(unavailable()); }
    let (station, system) = super::configuration::current_binding(c, call).ok_or_else(unavailable)?;
    if station.is_some() { return Err(unavailable()); }
    let character = c.session.int("charid").filter(|id| *id > 0).ok_or_else(unavailable)?;
    let old = c.session.int("shipid").filter(|id| *id > 0).ok_or_else(unavailable)?;
    let expected = c.world.online_session(character).ok_or_else(unavailable)?;
    if expected.character_id != character || expected.user_id != Some(c.login.user_id)
        || expected.session_id != c.session.sid || expected.ship_id != Some(old)
        || expected.system_id != Some(system) || expected.station_id.is_some() {
        return Err(unavailable());
    }
    super::check_weapons_timer(character, messages::ship::WEAPONS_TIMER_EJECT)?;
    // An eject boundary owns the outgoing hull's loaded slots: refuse new use
    // and wait out admitted finite work before the market guard.
    let held_loaded = crate::space::system::boarding::quiesce_ship_loaded(&c.world, system, &[old])
        .await.map_err(|refusal| refusal.into_call_error(unavailable()))?;
    let admission = c.world.admission(system).map_err(db_err)?;
    let market = crate::services::market::lock().await;
    admission.check().map_err(db_err)?;
    let gate = c.world.lock_condition_snapshot(system).await.map_err(db_err)?;
    let selected = selection::selected(&c.db, character, c.login.user_id).await.map_err(db_err)?.ok_or_else(unavailable)?;
    if selected.station_id.is_some() || selected.solar_system_id != system
        || selection::ship_id(&c.db, character).await.map_err(db_err)? != Some(old) {
        return Err(unavailable());
    }
    let old_item = load_items(&c.db, &c.sde, ItemQuery::Id(old)).await.map_err(db_err)?.pop().ok_or_else(unavailable)?;
    if old_item.owner_id != character || old_item.location_id != system || old_item.flag_id != 0
        || old_item.category_id != i64::from(CATEGORY_SHIP) || !old_item.singleton || old_item.quantity != 1
        || super::loss::medical::is_capsule(&c.sde, old_item.type_id) {
        return Err(unavailable());
    }
    let expected_actor = expected.clone();
    let change_at = crate::client::filetime_now();
    let (old_ball, transition) = c.world.with(system, move |s| {
        if !s.combat_session_current(character, Some(&expected_actor)) { return Err(unavailable()); }
        if !s.ship_change_ready(character, expected_actor.session_id, change_at) {
            return Err(notify_error(messages::ship::SHIP_CHANGE_NOT_READY));
        }
        let (old_ball, generation) = s.authorize_own_hull_exit(character, old).map_err(|_| unavailable())?;
        if crate::space::sovereignty::links::blocks(s, old) {
            return Err(notify_error(messages::effects::ENTOSIS_ACTION_BLOCKED));
        }
        let mut transition = s.begin_condition_transitions(&[old]).ok_or_else(unavailable)?;
        transition.held_loaded = Some(held_loaded);
        if transition.entries[0].1.generation != generation { return Err(unavailable()); }
        Ok((old_ball, transition))
    }).await.ok_or_else(unavailable)??;
    let security = crate::services::crimewatch::security_status(&c.db, character).await?;
    let user = c.login.user_id;
    let client = c.login.client_id;
    let corporation = selected.corporation_id;
    let alliance = selected.alliance_id.unwrap_or(-1);
    let old_type = old_ball.type_id;
    let plan = boarding::RecoveryPlan::Eject { character, user, system, old, old_type };
    let sde = c.sde.clone();
    let selected = transitions::settle_space(c.world.clone(), c.db.clone(), sde.clone(), system,
        character, client, expected.clone(), crate::space::session_change::ship_change_deadline(change_at), transition, market, gate, admission, plan,
        move |tx, outbox, states, facts| {
            let sde = sde.clone();
            let old_ball = old_ball.clone();
            Box::pin(async move {
                selection::lock_character(tx, character).await.map_err(db_err)?;
                if !boarding::guard_current(tx, character, user, old, system, old_type).await.map_err(db_err)? {
                    return Err(unavailable());
                }
                let written = transitions::checkpoint(tx, outbox, system, states, facts).await?;
                let capsule = creation::positioned(tx, TYPE_CAPSULE, character, system, old_ball.pos).await.map_err(db_err)?;
                crate::services::inventory::notifications::notify_item_change(tx, &sde, outbox, capsule,
                    vec![(IX_LOCATION_ID, Value::Int(LOCATION_JUNKYARD))]).await?;
                if !boarding::position_moved(tx, old, character, system, old_ball.pos).await.map_err(db_err)?
                    || !boarding::select_moved(tx, character, user, old, capsule, system).await.map_err(db_err)? {
                    return Err(unavailable());
                }
                let row = |id, kind| FreeShip { item_id: id, type_id: kind, owner_id: character,
                    corporation_id: corporation, alliance_id: alliance, pos: old_ball.pos };
                let old_prepared = super::balls::unpiloted(tx, sde.clone(), &row(old, old_type)).await?;
                let prepared = super::balls::piloted(tx, sde, &row(capsule, TYPE_CAPSULE), character, security).await?;
                if prepared.ball.id != capsule || prepared.ball.char_id != Some(character) || !prepared.ball.interactive {
                    return Err(unavailable());
                }
                let old_pos = old_ball.pos;
                Ok((PreparedTransition {
                    old: Some(PreparedOld::Retained { snapshot_ball: old_ball, prepared: old_prepared }),
                    target: PreparedTarget { id: capsule, sampled_position: old_pos, prepared,
                        pilot: Some((client, character)), existing: false },
                }, written))
            }) as crate::db::transaction::TxFuture<'_, (PreparedTransition, Vec<i64>), CallError>
        }).await?;
    if let Some(capsule) = selected {
        info!(user = c.login.user_id, "character {character} ejected from ship {old} into capsule {capsule}");
    }
    super::boarding::finish(c, selected, Some(system), Some(&expected)).await
}

#[cfg(all(test, feature = "tests-ships"))] mod tests;
