//! Owned ship boarding in stations and space.
//! Mirrors clientDogmaLocation.MakeShipActive using game items, characters and the SDE.
//! MakeShipActive passes session.shipid as the boarding context both docked and in space.
//! Boarding preserves both hulls and their condition; foreign custody and structure access are unsupported.
//! Unresolved space boarding settlements relinquish pending ownership for the existing actor retry.
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
    let market = crate::services::market::lock().await;
    let selected = selection::selected(&c.db, char_id, c.login.user_id).await.map_err(db_err)?.ok_or_else(refusal)?;
    if selected.station_id != station || (station.is_none() && Some(selected.solar_system_id) != system)
        || selection::ship_id(&c.db, char_id).await.map_err(db_err)? != Some(old) {
        return Err(refusal());
    }
    let ship = load_items(&c.db, &c.sde, ItemQuery::Id(ship_id)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| user_error(messages::MSG_SHIP_NOT_FOUND, vec![]))?;
    if ship.category_id != CATEGORY_SHIP as i64 || ship.owner_id != char_id || ship.location_id != location
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
    let old_ship = load_items(&c.db, &c.sde, ItemQuery::Id(old)).await.map_err(db_err)?.into_iter().next().ok_or_else(refusal)?;
    let old_type = old_ship.type_id;
    let target_type = ship.type_id;
    let fences = online::capture_online_fences(&c.world, &[char_id]).await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    if station.is_none() {
        let system = system.ok_or_else(refusal)?;
        let gate = c.world.lock_condition_snapshot(system).await.map_err(db_err)?;
        if c.world.online_session(char_id).as_ref() != Some(&expected) { return Err(refusal()); }
        let expected_actor = expected.clone();
        let (snapshot, transition) = c.world.with(system, move |s| {
            if !s.combat_session_current(char_id, Some(&expected_actor)) {
                return Err(crate::space::system::boarding::BoardRefusal::StalePilot);
            }
            let snapshot = s.authorize_board(char_id, old, ship_id, MAX_BOARDING_DISTANCE)?;
            let transition = s.begin_condition_transitions(&[old, ship_id]).ok_or(crate::space::system::boarding::BoardRefusal::Unsafe)?;
            if transition.entries[0].1.generation != snapshot.old_generation
                || transition.entries[1].1.generation != snapshot.target_generation {
                return Err(crate::space::system::boarding::BoardRefusal::Unsafe);
            }
            Ok((snapshot, transition))
        }).await.ok_or_else(refusal)?.map_err(|_| refusal())?;
        let security = crate::services::crimewatch::security_status(&db, char_id).await?;
        let client = c.login.client_id;
        let selected = settle_space(c.world.clone(), db, c.sde.clone(), system, char_id, client,
            expected.clone(), transition, market, gate, boarding::RecoveryPlan::Board {
                character: char_id, user: c.login.user_id, system, old, old_type, target: ship_id, target_type,
            },
            move |tx, outbox, states, facts| {
                let snapshot = snapshot.clone();
                let sde = sde.clone();
                let fences = fences.clone();
                Box::pin(async move {
                    if !boarding::board_in_space(tx, char_id, old, ship_id, system, old_type, target_type,
                        snapshot.old.pos, snapshot.target.pos).await.map_err(db_err)? { return Err(refusal()); }
                    let written = crate::space::system::boarding::transitions::checkpoint(tx, outbox, system, states, facts).await?;
                    let fit = Fit::load(tx, sde.clone(), char_id, ship_id).await?;
                    fitting::required_skills(&fit, &sde, Key::Ship, target_type, false)?;
                    online::reconcile_core(tx, &sde, outbox, &fences, char_id, ship_id).await?;
                    let row = |ball: &crate::space::Ball| FreeShip {
                        item_id: ball.id, type_id: ball.type_id, owner_id: ball.owner_id,
                        corporation_id: ball.corp_id, alliance_id: ball.alliance_id, pos: ball.pos,
                    };
                    let old_prepared = crate::services::ship::balls::unpiloted(tx, sde.clone(), &row(&snapshot.old)).await?;
                    let target_prepared = crate::services::ship::balls::piloted(tx, sde, &row(&snapshot.target), char_id, security).await?;
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
            async move { online::dispatch_changed((), outbox, c) }
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

#[cfg(test)] mod tests;
