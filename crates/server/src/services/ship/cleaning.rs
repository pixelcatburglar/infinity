//! Physical-hull dirt RPCs sharing the dirt refusal for client cleanShipButton.py.
//! Timestamps persist in `game.ship_dirt`, independent of bay settings and ownership.
//! Visibility comes from the shared configuration hull check and the space actor.
//! Cleaning has no wallet charge; a committed reset republishes the live slim
//! through the existing system actor broadcast instead of a dedicated notification.
use super::{appearance, Call, CallResult, Client, Value, db_err};
use infinity_core::consts::{CATEGORY_SHIP, FLAG_HANGAR};
use crate::dao::{characters::{selection, ship as character_ship}, items::{ship, ship_appearance::ObservedHull, ship_configuration, ship_dirt}};
use crate::db::outbox::Outbox;
use super::appearance::dirt::refuse;
use crate::services::market;

fn ship_arg(call: &Call<'_>) -> Option<i64> {
    match call.args { [Value::Int(ship)] if *ship > 0 => Some(*ship), _ => None }
}

pub(super) async fn get(c: &Client, call: &Call<'_>) -> CallResult {
    let ship = ship_arg(call).ok_or_else(refuse)?;
    let character = c.session.int("charid").filter(|id| *id > 0).ok_or_else(refuse)?;
    super::configuration::current_binding(c, call).ok_or_else(refuse)?;
    if !selection::owns(&c.db, character, c.login.user_id).await.map_err(db_err)? { return Err(refuse()); }
    let item = ObservedHull::from(&super::configuration::visible_hull(c, ship, character).await?.ok_or_else(refuse)?);
    let now = crate::client::filetime_now();
    c.db.transaction(|tx, _: &mut Outbox| {
        let item = item;
        Box::pin(async move {
            let stamp = ship_dirt::get_or_init(tx, &item, now).await.map_err(db_err)?.ok_or_else(refuse)?;
            Ok::<_, super::CallError>(appearance::dirt_timestamp_value(stamp))
        })
    }, |value, _| async move { Ok(value) }).await
}

pub(super) async fn reset(c: &Client, call: &Call<'_>) -> CallResult {
    let ship = ship_arg(call).ok_or_else(refuse)?;
    let character = c.session.int("charid").filter(|id| *id > 0).ok_or_else(refuse)?;
    let (station, system) = super::configuration::current_binding(c, call).ok_or_else(refuse)?;
    if let Some(station) = station {
        if !c.sde.map.station(station).is_some_and(|s| s.solar_system_id == Some(system)) { return Err(refuse()); }
    } else {
        if c.session.int("shipid") != Some(ship) || c.world.with(system, move |s| s.pilot_ball(character).is_some_and(|b| b.id == ship)).await != Some(true) { return Err(refuse()); }
    }
    let user = c.login.user_id;
    if !selection::owns(&c.db, character, user).await.map_err(db_err)? { return Err(refuse()); }
    let now = crate::client::filetime_now();
    let sde = c.sde.clone();
    let world = c.world.clone();
    // The market guard serializes the write and its post-commit publication
    // with cold hull preparation and transfers; the actor turn is never awaited
    // inside the transaction.
    let _market = market::lock().await;
    // A space-bound hull captures its live incarnation up front: a stopped
    // actor permits a persisted-only write, while an invalid live membership refuses.
    let fence = if station.is_none() {
        match world.with(system, move |s| s.capture_appearance(ship)).await {
            Some(fence) => {
                let fence = fence.ok_or_else(refuse)?;
                // The RPC authorizes only the caller's own piloted ship; a live
                // incarnation piloted by anyone else keeps the write out.
                if fence.pilot != Some(character) { return Err(refuse()); }
                Some(fence)
            }
            None => None,
        }
    } else { None };
    let (stamp, _publication) = c.db.transaction(|tx, outbox: &mut Outbox<appearance::AppearanceChanged>| {
        let sde = sde.clone();
        let fence = fence.clone();
        Box::pin(async move {
            let (kind, location, flag) = if let Some(station) = station {
                selection::lock_character(tx, character).await.map_err(db_err)?;
                if !selection::owns(tx, character, user).await.map_err(db_err)? { return Err(refuse()); }
                let location = character_ship::current(tx, character).await.map_err(db_err)?.ok_or_else(refuse)?;
                if location.station != Some(station) || location.system != Some(system) { return Err(refuse()); }
                (ship::lock_owned_at(tx, ship, character, station, FLAG_HANGAR).await.map_err(db_err)?, station, FLAG_HANGAR)
            } else {
                (ship_configuration::lock_selected(tx, character, user, ship, None, system, 0).await.map_err(db_err)?, system, 0)
            };
            let kind = kind.ok_or_else(refuse)?;
            if sde.types.get(kind).and_then(|ty| ty.category_id) != Some(CATEGORY_SHIP) { return Err(refuse()); }
            let hull = ObservedHull { item_id: ship, owner_id: character, location_id: location, flag_id: flag, type_id: kind };
            appearance::set_dirt_tx(tx, &hull, now, fence.as_ref(), outbox).await
        })
    }, |stamp, outbox| appearance::dispatch_changed(stamp, outbox, &world)).await?;
    Ok(appearance::dirt_timestamp_value(stamp))
}
