//! Station rookie ship and one-time spare kit grants for `dogmaIM`.
//! Mirrors the docked corvette and bonus buttons in `eve/client/script/ui/station/base.py`.
//! The shared server-authored rookie catalogue supplies both grants; game characters
//! and items provide durable authority. Structures and in-space grants are unsupported.

use std::sync::Arc;
use std::collections::HashSet;
use crate::client::Client;
use infinity_core::consts::{CATEGORY_SHIP, FLAG_HANGAR, IX_LOCATION_ID};
use crate::dao::{characters::newbie as characters, items::{creation, loading::{Item, ItemQuery}, transfers}};
use crate::db::{executor::Executor, outbox::Outbox};
use infinity_core::messages::newbie as text;
use crate::sde::{rookie_fits::RookieFit, Sde};
use crate::services::{Call, CallError, CallResult, db_err, notify_error};
use crate::services::{effects::online::{self, OnlineChanged, OnlineFences}, identity::Identity, inventory::notifications::notify_item_change, items::load_items, ship::boarding};
use marshal::Value;

/// Server grant policy: the shared racial kit has six entries.
const KIT_ENTRIES: usize = 6;

fn unavailable() -> CallError { notify_error(text::UNAVAILABLE) }

#[derive(Clone, Copy)]
enum Grant { Ship, Bonus }

async fn authorize<'a>(ex: &(impl Executor + ?Sized), sde: &'a Sde, identity: Identity, user: i64, station: i64) -> Result<(&'a RookieFit, Item, Option<i64>), CallError> {
    let row = characters::lock(ex, identity.char_id).await.map_err(db_err)?.ok_or_else(unavailable)?;
    let old = identity.ship_id.ok_or_else(unavailable)?;
    if row.user_id != user || row.station_id != Some(station) || row.ship_id != Some(old) { return Err(unavailable()); }
    let hull = load_items(ex, sde, ItemQuery::Id(old)).await.map_err(db_err)?.into_iter().next().ok_or_else(unavailable)?;
    if hull.owner_id != identity.char_id || hull.location_id != station || hull.flag_id != FLAG_HANGAR
        || !hull.singleton || hull.quantity != 1 || hull.category_id != i64::from(CATEGORY_SHIP) { return Err(unavailable()); }
    let ship_type = sde.characters.race(i64::from(row.race_id)).and_then(|race| race.ship_type_id).ok_or_else(unavailable)?;
    let fit = sde.rookie_fits.get(ship_type).ok_or_else(unavailable)?;
    if fit.items.len() != KIT_ENTRIES || fit.items.iter().any(|(kind, _, _)| sde.types.get(*kind).is_none()) {
        return Err(CallError::Internal(anyhow::anyhow!("invalid rookie grant catalogue")));
    }
    Ok((fit, hull, row.claimed_at))
}

fn validate_batch(rows: &[Item], fit: &RookieFit, owner: i64, station: i64, ship: i64) -> Result<(), CallError> {
    let fail = || CallError::Internal(anyhow::anyhow!("rookie ship creation did not match catalogue"));
    if rows.len() != fit.items.len() + 1 { return Err(fail()); }
    let mut ids = HashSet::with_capacity(rows.len());
    let mut children = Vec::with_capacity(fit.items.len());
    let mut found_hull = false;
    for item in rows {
        if !ids.insert(item.item_id) || item.owner_id != owner { return Err(fail()); }
        if item.item_id == ship {
            if item.type_id != fit.ship || item.location_id != station || item.flag_id != FLAG_HANGAR || !item.singleton || item.quantity != 1 { return Err(fail()); }
            found_hull = true;
        } else {
            if item.location_id != ship { return Err(fail()); }
            children.push((item.type_id, item.flag_id, item.quantity));
        }
    }
    let mut expected: Vec<_> = fit.items.iter().map(|&(kind, flag, quantity)| (kind, flag, i64::from(quantity))).collect();
    children.sort_unstable();
    expected.sort_unstable();
    if !found_hull || children != expected { return Err(fail()); }
    Ok(())
}

async fn grant_core(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<OnlineChanged>, fences: &OnlineFences, identity: Identity, user: i64, station: i64, action: Grant, now: i64) -> Result<Option<i64>, CallError> {
    let (fit, old, claimed) = authorize(ex, sde, identity, user, station).await?;
    match action {
        Grant::Bonus => {
            if claimed.is_some() || !characters::claim_bonus_if_unclaimed(ex, identity.char_id, now).await.map_err(db_err)? {
                return Err(notify_error(text::ALREADY_CLAIMED));
            }
            for &(kind, _, quantity) in &fit.items {
                let id = transfers::insert(ex, kind, identity.char_id, station, FLAG_HANGAR, i64::from(quantity)).await.map_err(db_err)?;
                notify_item_change(ex, sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
            }
            Ok(None)
        }
        Grant::Ship => {
            let rookie_group = sde.types.get(fit.ship).and_then(|item| item.group_id).ok_or_else(unavailable)?;
            if old.group_id == i64::from(rookie_group) { return Err(notify_error(text::ALREADY_ROOKIE)); }
            let target = creation::rookie_ship(ex, fit.ship, identity.char_id, station, FLAG_HANGAR).await.map_err(db_err)?;
            creation::fitted_items(ex, target, identity.char_id, &fit.items).await.map_err(db_err)?;
            let mut rows = load_items(ex, sde, ItemQuery::IdOrLocationExceptFlag(target, FLAG_HANGAR)).await.map_err(db_err)?;
            validate_batch(&rows, fit, identity.char_id, station, target)?;
            rows.sort_unstable_by_key(|item| item.item_id);
            for item in rows {
                outbox.notify_client("OnItemChange", vec![item.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(0))]), Value::None]);
            }
            boarding::board_in_station_core(ex, sde, outbox, fences, identity.char_id, old.item_id, target, station, old.type_id, fit.ship).await?;
            Ok(Some(target))
        }
    }
}

async fn execute(c: &mut Client, call: &Call<'_>, action: Grant) -> CallResult {
    if !(matches!(call.kwargs, Value::None) || matches!(call.kwargs, Value::Dict(v) if v.is_empty())) { return Err(unavailable()); }
    let (requested_old, station) = match (action, call.args) {
        (Grant::Ship, [Value::Int(old), Value::Int(station)]) if *old > 0 && *station > 0 => (Some(*old), *station),
        (Grant::Bonus, [Value::Int(station)]) if *station > 0 => (None, *station),
        _ => return Err(unavailable()),
    };
    let identity = Identity::of_client(c);
    let user = c.login.user_id;
    if identity.char_id <= 0 || user <= 0 || identity.ship_id.is_none_or(|id| id <= 0)
        || identity.station_id != Some(station) || requested_old.is_some_and(|old| identity.ship_id != Some(old))
        || c.session.int("structureid").is_some() || c.session.int("solarsystemid").is_some()
        || c.sde.map.station(station).is_none() { return Err(unavailable()); }
    let expected = c.world.online_session(identity.char_id).ok_or_else(unavailable)?;
    if infinity_core::session::OnlineSession::from_session(&c.session).as_ref() != Some(&expected)
        || expected.user_id != Some(user) || expected.ship_id != identity.ship_id || expected.station_id != Some(station) { return Err(unavailable()); }
    let _lock = crate::services::market::lock().await;
    let fences = online::capture_online_fences(&c.world, &[identity.char_id]).await;
    if c.world.online_session(identity.char_id).as_ref() != Some(&expected) { return Err(unavailable()); }
    let db = c.db.clone();
    let sde = c.sde.clone();
    let now = crate::client::filetime_now();
    let selected = db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            Box::pin(async move { grant_core(tx, &sde, outbox, &fences, identity, user, station, action, now).await })
        },
        |selected, outbox| {
            let c = &mut *c;
            async move {
                let world = c.world.clone();
                online::dispatch_entries(selected, outbox, &world, |notification| match notification {
                    notification @ crate::db::outbox::Notification::DirectClient { .. } => notification.dispatch(&world, Some(&mut *c)),
                    notification => online::dispatch_notification(notification, &world, &c.notify_tx, c.session.int("charid")),
                })
            }
        },
    ).await?;
    if let Some(ship) = selected {
        crate::services::combat::load_docked_condition(&db, &c.world, identity.char_id, ship).await?;
        boarding::finish(c, Some(ship), None, Some(&expected)).await?;
    }
    Ok(Value::None)
}

pub(in crate::services) async fn ship(c: &mut Client, call: &Call<'_>) -> CallResult { execute(c, call, Grant::Ship).await }
pub(in crate::services) async fn bonus(c: &mut Client, call: &Call<'_>) -> CallResult { execute(c, call, Grant::Bonus).await }

#[cfg(test)]
mod tests;
