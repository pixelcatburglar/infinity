//! Physical-hull dirt RPCs used by the client `cleanShipButton.py`.
//! Timestamps persist in `game.ship_dirt`, independent of bay settings and ownership.
//! Visibility comes from the shared configuration hull check and the space actor.
//! Cleaning has no wallet charge or invented client notification.
use super::{Call, CallResult, Client, Value, db_err};
use infinity_core::consts::{CATEGORY_SHIP, FLAG_HANGAR};
use crate::dao::{characters::{selection, ship as character_ship}, items::{ship, ship_configuration, ship_dirt}};
use crate::services::notify_error;
use infinity_core::messages::ship as text;

fn ship_arg(call: &Call<'_>) -> Option<i64> {
    match call.args { [Value::Int(ship)] if *ship > 0 => Some(*ship), _ => None }
}

pub(super) async fn get(c: &Client, call: &Call<'_>) -> CallResult {
    let invalid = || notify_error(text::CLEANING_INACCESSIBLE);
    let ship = ship_arg(call).ok_or_else(invalid)?;
    let character = c.session.int("charid").filter(|id| *id > 0).ok_or_else(invalid)?;
    super::configuration::current_binding(c, call).ok_or_else(invalid)?;
    if !selection::owns(&c.db, character, c.login.user_id).await.map_err(db_err)? { return Err(invalid()); }
    let item = ship_dirt::ObservedHull::from(&super::configuration::visible_hull(c, ship, character).await?.ok_or_else(invalid)?);
    let now = crate::client::filetime_now();
    c.db.transaction(|tx, _: &mut crate::db::outbox::Outbox| {
        let item = item;
        Box::pin(async move {
            let stamp = ship_dirt::get_or_init(tx, &item, now).await.map_err(db_err)?.ok_or_else(invalid)?;
            Ok::<_, super::CallError>(Value::Int(stamp))
        })
    }, |value, _| async move { Ok(value) }).await
}

pub(super) async fn reset(c: &Client, call: &Call<'_>) -> CallResult {
    let invalid = || notify_error(text::CLEANING_INACCESSIBLE);
    let ship = ship_arg(call).ok_or_else(invalid)?;
    let character = c.session.int("charid").filter(|id| *id > 0).ok_or_else(invalid)?;
    let (station, system) = super::configuration::current_binding(c, call).ok_or_else(invalid)?;
    if let Some(station) = station {
        if !c.sde.map.station(station).is_some_and(|s| s.solar_system_id == Some(system)) { return Err(invalid()); }
    } else {
        if c.session.int("shipid") != Some(ship) || c.world.with(system, move |s| s.pilot_ball(character).is_some_and(|b| b.id == ship)).await != Some(true) { return Err(invalid()); }
    }
    let user = c.login.user_id;
    if !selection::owns(&c.db, character, user).await.map_err(db_err)? { return Err(invalid()); }
    let now = crate::client::filetime_now();
    let sde = c.sde.clone();
    c.db.transaction(|tx, _: &mut crate::db::outbox::Outbox| {
        let sde = sde.clone();
        Box::pin(async move {
            let kind = if let Some(station) = station {
                selection::lock_character(tx, character).await.map_err(db_err)?;
                if !selection::owns(tx, character, user).await.map_err(db_err)? { return Err(invalid()); }
                let location = character_ship::current(tx, character).await.map_err(db_err)?.ok_or_else(invalid)?;
                if location.station != Some(station) || location.system != Some(system) { return Err(invalid()); }
                ship::lock_owned_at(tx, ship, character, station, FLAG_HANGAR).await.map_err(db_err)?
            } else {
                ship_configuration::lock_selected(tx, character, user, ship, None, system, 0).await.map_err(db_err)?
            }.ok_or_else(invalid)?;
            if sde.types.get(kind).and_then(|ty| ty.category_id) != Some(CATEGORY_SHIP) { return Err(invalid()); }
            let stamp = ship_dirt::reset(tx, ship, now).await.map_err(db_err)?;
            Ok::<_, super::CallError>(Value::Int(stamp))
        })
    }, |value, _| async move { Ok(value) }).await
}
