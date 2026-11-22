//! Owned ship boarding in stations and space.
//! Mirrors clientDogmaLocation.MakeShipActive using game items, characters and the SDE.
//! MakeShipActive passes session.shipid as the boarding context both docked and in space.
//! Boarding preserves both hulls and their condition. A foreign in-space hull transfers its active subtree; structure access is unsupported.
//! Admission runs inside the system actor's turn: the shared ship-change
//! deadline must have elapsed, and an outstanding CONCORD response ticket
//! refuses boarding; the hull left behind keeps its sampled momentum and
//! decelerates through ordinary evolution. Refreshing the already-selected
//! hull is not a ship change. Unresolved space boarding settlements
//! relinquish pending ownership for the existing actor retry.
use crate::client::Client;
use infinity_core::consts::{CATEGORY_SHIP, FLAG_HANGAR};
use crate::dao::items::loading::ItemQuery;
use crate::db::outbox::Outbox;
use infinity_core::messages;
use crate::services::dogma::info::ship_state_core;
use crate::services::items::load_items;
use crate::services::{db_err, user_error, CallResult};
use crate::services::effects::online;
use crate::space::system::boarding::transitions::{PreparedOld, PreparedTarget, PreparedTransition, settle_space};
use std::sync::Arc;
use marshal::Value;
use tracing::info;
mod ownership;

/// Client `appConst.maxBoardingDistance`, in metres of surface separation.
const MAX_BOARDING_DISTANCE: f64 = 6550.0;

/// Board(shipID, currentShipID) -> shipState.
pub(super) async fn board(c: &mut Client, ship_id: i64, context: Option<i64>) -> CallResult {
    use crate::services::{combat, effects::fitting, fit::{Fit, Key}};
    use crate::dao::{characters::selection, items::{boarding, space_ships::FreeShip}};
    let refusal = || crate::services::notify_error(messages::fitting::BOARD_UNAVAILABLE);
    let char_id = c.session.int("charid").ok_or_else(refusal)?;
    let old = c.session.int("shipid").ok_or_else(refusal)?;
    let station = c.session.int("stationid");
    let system = c.session.int("solarsystemid");
    let location = station.or(system).ok_or_else(refusal)?;
    let expected = c.world.online_session(char_id).ok_or_else(refusal)?;
    if infinity_core::session::OnlineSession::from_session(&c.session).as_ref() != Some(&expected)
        || expected.user_id != Some(c.login.user_id) || expected.ship_id != Some(old)
        || expected.station_id != station || (station.is_none() && expected.system_id != system) {
        return Err(refusal());
    }
    let space_admission = system.filter(|_| station.is_none()).map(|system| c.world.admission(system)).transpose().map_err(db_err)?;
    // A space boarding boundary owns both hulls' loaded slots: refuse new use
    // and wait out admitted finite work before the market guard.
    let held_loaded = if let Some(system) = system.filter(|_| station.is_none()) {
        Some(crate::space::system::boarding::quiesce_ship_loaded(&c.world, system, &[old, ship_id]).await.map_err(|held| held.into_call_error(refusal()))?)
    } else { None };
    let market = crate::services::market::lock().await;
    if let Some(admission) = &space_admission { admission.check().map_err(db_err)?; }
    let selected = selection::selected(&c.db, char_id, c.login.user_id).await.map_err(db_err)?.ok_or_else(refusal)?;
    if selected.station_id != station || (station.is_none() && Some(selected.solar_system_id) != system)
        || selection::ship_id(&c.db, char_id).await.map_err(db_err)? != Some(old) {
        return Err(refusal());
    }
    let ship = load_items(&c.db, &c.sde, ItemQuery::Id(ship_id)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| user_error(messages::MSG_SHIP_NOT_FOUND, vec![]))?;
    if ship.category_id != CATEGORY_SHIP as i64 || ship.location_id != location
        || (station.is_some() && ship.owner_id != char_id)
        || ship.flag_id != if station.is_some() { FLAG_HANGAR } else { 0 } {
        return Err(if station.is_some() { user_error(messages::MSG_CANT_BOARD_SHIP_NOT_IN_HANGAR, vec![]) } else { refusal() });
    }
    if !ship.singleton || ship.quantity != 1 {
        return Err(user_error(messages::MSG_CANT_BOARD_UNASSEMBLED_SHIP, vec![]));
    }
    if ship_id == old {
        if context != Some(old) { return Err(refusal()); }
        if let Some(system) = system.filter(|_| station.is_none()) {
            if !c.world.with(system, move |s| s.pilot_ball(char_id).is_some_and(|b| b.id == old && b.owner_id == char_id && b.char_id == Some(char_id))).await.unwrap_or(false) {
                return Err(refusal());
            }
        }
        return ship_state_core(c, ship_id).await;
    }
    if context != Some(old) { return Err(refusal()); }
    if station.is_none() && !crate::dao::items::boarding::unselected(&c.db, ship_id).await.map_err(db_err)? {
        return Err(refusal());
    }
    let old_ship = load_items(&c.db, &c.sde, ItemQuery::Id(old)).await.map_err(db_err)?.into_iter().next().ok_or_else(refusal)?;
    let old_type = old_ship.type_id;
    let target_type = ship.type_id;
    let foreign = if station.is_none() && ship.owner_id != char_id {
        let identity = crate::services::identity::Identity::of_client(c);
        Some(ownership::ForeignTake::capture(&c.db, &c.sde, &identity, &c.world, system.unwrap_or(0), ship_id).await?)
    } else { None };
    let fences = online::capture_online_fences(&c.world, &[char_id]).await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    if station.is_none() {
        let system = system.ok_or_else(refusal)?;
        let gate = c.world.lock_condition_snapshot(system).await.map_err(db_err)?;
        if c.world.online_session(char_id).as_ref() != Some(&expected) { return Err(refusal()); }
        let expected_actor = expected.clone();
        let change_at = crate::client::filetime_now();
        let (snapshot, transition) = c.world.with(system, move |s| {
            if !s.combat_session_current(char_id, Some(&expected_actor)) { return Err(refusal()); }
            if !s.ship_change_ready(char_id, expected_actor.session_id, change_at) {
                return Err(crate::services::notify_error(messages::ship::SHIP_CHANGE_NOT_READY));
            }
            let snapshot = s.authorize_board(char_id, old, ship_id, MAX_BOARDING_DISTANCE).map_err(|_| refusal())?;
            if snapshot.target.owner_id != char_id && crate::services::combat::targeting_charges::board_target_locked_by_other(char_id, ship_id) {
                return Err(crate::services::notify_error(messages::ship::SHIP_LOCKED_BY_OTHER));
            }
            if crate::services::crimewatch::concord_pursuit(char_id) {
                return Err(crate::services::notify_error(messages::ship::SHIP_CHANGE_PURSUIT));
            }
            let mut transition = s.begin_condition_transitions(&[old, ship_id]).ok_or_else(refusal)?;
            transition.held_loaded = held_loaded;
            if transition.entries[0].1.generation != snapshot.old_generation
                || transition.entries[1].1.generation != snapshot.target_generation {
                return Err(refusal());
            }
            Ok((snapshot, transition))
        }).await.ok_or_else(refusal)??;
        let security = crate::services::crimewatch::security_status(&db, char_id).await?;
        let client = c.login.client_id;
        let ready_at = crate::space::session_change::ship_change_deadline(change_at);
        let selected = settle_space(c.world.clone(), db, c.sde.clone(), system, char_id, client,
            expected.clone(), ready_at, transition, market, gate, space_admission.expect("space boarding admission"), boarding::RecoveryPlan::Board {
                character: char_id, user: c.login.user_id, system, old, old_type, target: ship_id, target_type,
                ownership: foreign.as_ref().map(|take| take.sealed.clone()),
            },
            move |tx, outbox, states, facts| {
                let snapshot = snapshot.clone();
                let sde = sde.clone();
                let fences = fences.clone();
                let mut attempt = foreign.as_ref().map(|take| take.fresh());
                Box::pin(async move {
                    let foreign_written = if let Some(take) = attempt.as_mut() {
                        Some(take.transfer(tx, &sde, outbox, char_id, system, old, states, facts).await?)
                    } else { None };
                    if attempt.is_some() && !crate::dao::items::boarding::position_moved(tx, ship_id, char_id, system, snapshot.target.pos).await.map_err(db_err)? {
                        return Err(refusal());
                    }
                    if !boarding::board_in_space(tx, char_id, old, ship_id, system, old_type, target_type,
                        snapshot.old.pos, snapshot.target.pos).await.map_err(db_err)? { return Err(refusal()); }
                    let written = match foreign_written {
                        Some(written) => written,
                        None => crate::space::system::boarding::transitions::checkpoint(tx, outbox, system, states, facts).await?,
                    };
                    let fit = Fit::load(tx, sde.clone(), char_id, ship_id).await?;
                    fitting::required_skills(&fit, &sde, Key::Ship, target_type, false)?;
                    online::reconcile_core(tx, &sde, outbox, &fences, char_id, ship_id).await?;
                    let affiliation = crate::dao::characters::ship::current(tx, char_id).await.map_err(db_err)?.ok_or_else(refusal)?;
                    let target_row = FreeShip { item_id: snapshot.target.id, type_id: snapshot.target.type_id, owner_id: char_id,
                        corporation_id: affiliation.corporation, alliance_id: affiliation.alliance.unwrap_or(-1), pos: snapshot.target.pos };
                    let old_row = FreeShip { item_id: snapshot.old.id, type_id: snapshot.old.type_id, owner_id: snapshot.old.owner_id,
                        corporation_id: snapshot.old.corp_id, alliance_id: snapshot.old.alliance_id, pos: snapshot.old.pos };
                    let old_prepared = crate::services::ship::balls::unpiloted(tx, sde.clone(), &old_row).await?;
                    let target_prepared = crate::services::ship::balls::piloted(tx, sde, &target_row, char_id, security).await?;
                    Ok((PreparedTransition {
                        old: Some(PreparedOld::Retained { snapshot_ball: snapshot.old, prepared: old_prepared }),
                        target: PreparedTarget { id: ship_id, sampled_position: snapshot.target.pos,
                            prepared: target_prepared, pilot: Some((client, char_id)), existing: true },
                    }, written))
                }) as crate::db::transaction::TxFuture<'_, (PreparedTransition, Vec<i64>), crate::services::CallError>
            }).await?;
        return finish(c, selected, Some(system), Some(&expected)).await;
    }
    if c.world.online_session(char_id).as_ref() != Some(&expected) { return Err(refusal()); }
    let change_at = crate::client::filetime_now();
    if !c.world.ship_change_ready(char_id, expected.session_id, change_at) {
        return Err(crate::services::notify_error(messages::ship::SHIP_CHANGE_NOT_READY));
    }
    // The deadline of an admitted station change freezes here and is recorded
    // in post-commit dispatch; a refused or uncommitted change sets nothing.
    let ready_at = crate::space::session_change::ship_change_deadline(change_at);
    let session_id = expected.session_id;
    db.transaction(
        move |tx, outbox: &mut Outbox<online::OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            Box::pin(async move {
                board_in_station_core(tx, &sde, outbox, &fences, char_id, old, ship_id, location, old_type, target_type).await
            })
        },
        |(), outbox| {
            let c = &mut *c;
            async move {
                c.world.commit_ship_change(char_id, session_id, ready_at);
                online::dispatch_changed((), outbox, c)
            }
        },
    ).await?;
    combat::load_docked_condition(&db, &c.world, char_id, ship_id).await?;
    finish(c, Some(ship_id), None, Some(&expected)).await
}

pub(in crate::services) async fn board_in_station_core(
    ex: &(impl crate::db::executor::Executor + ?Sized), sde: &Arc<crate::sde::Sde>,
    outbox: &mut Outbox<online::OnlineChanged>, fences: &online::OnlineFences,
    character: i64, old: i64, target: i64, station: i64, old_type: i32, target_type: i32,
) -> Result<(), crate::services::CallError> {
    use crate::services::{effects::fitting, fit::{Fit, Key}};
    if !crate::dao::items::boarding::board_in_station(ex, character, old, target, station, old_type, target_type).await.map_err(db_err)? {
        return Err(crate::services::notify_error(messages::fitting::BOARD_UNAVAILABLE));
    }
    let fit = Fit::load(ex, sde.clone(), character, target).await?;
    fitting::required_skills(&fit, sde, Key::Ship, target_type, false)?;
    online::reconcile_core(ex, sde, outbox, fences, character, target).await.map(|_| ())
}

/// Apply the caller-side session and state after a successful owned settlement.
pub(in crate::services) async fn finish(c: &mut Client, selected: Option<i64>, system: Option<i64>,
    expected: Option<&infinity_core::session::OnlineSession>) -> CallResult {
    let Some(ship_id) = selected else { return Ok(Value::None) };
    let char_id = c.session.int("charid").expect("selected character");
    if expected.is_some_and(|session| c.world.online_session(char_id).as_ref() != Some(session)) {
        return Err(crate::services::notify_error(messages::fitting::BOARD_UNAVAILABLE));
    }
    crate::services::combat::reset(c);
    c.change_session(vec![("shipid", Value::Int(ship_id))]);
    info!(user = c.login.user_id, "character {char_id} boarded ship {ship_id}");
    if system.is_some() { crate::services::ship::send_state(c).await?; }
    ship_state_core(c, ship_id).await
}

#[cfg(all(test, feature = "tests-ships"))] mod tests;
#[cfg(all(test, any(feature = "tests-ships", feature = "tests-space")))]
#[path = "boarding/tests/foreign_proof.rs"]
mod foreign_proof;
