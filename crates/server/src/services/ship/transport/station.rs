//! Station arrival transaction shared by GM transport and ordinary docking.
//! Mirrors `carbon/common/script/net/machoNet.py` session change behavior
//! (`ProcessSessionChange`) for docked relocation and the inventory
//! invalidation the client's `eve/client/script/environment/invCache.py`
//! applies on `OnItemChange`. The active hull's root location and flag and
//! the pilot's station, system, constellation and region columns move in one
//! transaction; hull contents, owner, singleton and item ID stay unchanged.
//! Data comes from `game.characters`, `game.items` and the SDE map. Session
//! changes, client notifications and lifecycle events belong to the caller;
//! the callback contract is documented on `enter`. Station-to-space
//! relocation, Upwell structures and moving other pilots are unsupported.

use infinity_core::consts::{CATEGORY_SHIP, FLAG_HANGAR, IX_FLAG, IX_LOCATION_ID};
use infinity_core::messages;
use infinity_core::session::OnlineSession;
use marshal::Value;
use crate::dao::items::loading::{Item, ItemQuery};
use crate::db::{executor::Executor, outbox::Outbox, Db};
use crate::sde::Sde;
use crate::services::{db_err, identity::Identity, notify_error, CallError};
use crate::space::World;

/// A resolved NPC station destination with its complete map ancestry.
#[derive(Clone, Copy)]
pub(crate) struct Destination {
    pub station_id: i64,
    pub system_id: i64,
    pub constellation_id: i64,
    pub region_id: i64,
}

fn no_ship() -> CallError { notify_error(messages::gm::GM_TRANSPORT_NO_SHIP) }
fn conflict() -> CallError { notify_error(messages::gm::GM_TRANSPORT_CONFLICT) }
fn trade_open() -> CallError { notify_error(messages::gm::GM_TRANSPORT_TRADE_OPEN) }
fn invalid_station() -> CallError { notify_error(messages::gm::GM_TRANSPORT_INVALID_STATION) }

/// Resolve an SDE NPC station and its full ancestry. An unknown station is a
/// business refusal; a missing ancestry field on an existing row is an SDE
/// fault and is never filled with a default.
pub(crate) fn resolve(sde: &Sde, station: i64) -> Result<Destination, CallError> {
    let row = sde.map.station(station).ok_or_else(invalid_station)?;
    Ok(Destination {
        station_id: station,
        system_id: super::super::required(row.solar_system_id, "station system")?,
        constellation_id: super::super::required(row.constellation_id, "station constellation")?,
        region_id: super::super::required(row.region_id, "station region")?,
    })
}

/// The complete station-arrival session update: station bound, ballpark
/// cleared, and system ancestry at the destination.
pub(crate) fn session_change(destination: Destination) -> super::super::SessionChange {
    super::super::SessionChange {
        updates: vec![
            ("stationid", Value::Int(destination.station_id)),
            ("solarsystemid", Value::None),
            ("solarsystemid2", Value::Int(destination.system_id)),
            ("constellationid", Value::Int(destination.constellation_id)),
            ("regionid", Value::Int(destination.region_id)),
        ],
    }
}

/// Lock the pilot's active hull and load its root item inside the caller's
/// transaction. A positive character first locks its character location row;
/// `user` additionally binds the account and ordinary docking/checkpoint
/// callers may pass `None`, keeping the character/hull association guards.
/// The anonymous `char_id == 0` headless checkpoint only locks the hull at
/// its space source, keeps its real owner and never touches a character row.
pub(crate) async fn lock_hull(
    ex: &(impl Executor + ?Sized), sde: &Sde,
    identity: &Identity, user: Option<i64>,
) -> Result<Item, CallError> {
    let (Some(ship), Some(system)) = (identity.ship_id, identity.system_id) else {
        return Err(no_ship());
    };
    if identity.char_id > 0 {
        let station = identity.station_id;
        let (location, flag) = match station {
            Some(station) => (station, FLAG_HANGAR),
            None => (system, 0),
        };
        if !crate::dao::characters::ship::lock_location(ex, identity.char_id, user, ship, station, system).await.map_err(db_err)?
            || crate::dao::items::ship::lock_owned_at(ex, ship, identity.char_id, location, flag).await.map_err(db_err)?.is_none() {
            return Err(no_ship());
        }
        let hull = crate::services::items::load_items(ex, sde, ItemQuery::Id(ship)).await.map_err(db_err)?
            .into_iter().next().ok_or_else(no_ship)?;
        if hull.category_id != i64::from(CATEGORY_SHIP) || hull.owner_id != identity.char_id
            || hull.location_id != location || hull.flag_id != flag || !hull.singleton || hull.quantity != 1 {
            return Err(no_ship());
        }
        return Ok(hull);
    }
    if identity.char_id == 0 && identity.station_id.is_none() {
        if !crate::dao::items::ship::lock_reload_ship(ex, ship).await.map_err(db_err)? {
            return Err(no_ship());
        }
        let hull = crate::services::items::load_items(ex, sde, ItemQuery::Id(ship)).await.map_err(db_err)?
            .into_iter().next().ok_or_else(no_ship)?;
        if hull.category_id != i64::from(CATEGORY_SHIP) || hull.location_id != system
            || hull.flag_id != 0 || !hull.singleton || hull.quantity != 1 {
            return Err(no_ship());
        }
        return Ok(hull);
    }
    Err(no_ship())
}

/// Move the already-locked hull into `destination` and relocate its pilot,
/// guarded by the source tuple read under `lock_hull`; a guarded miss fails
/// the whole transaction. Anonymous hulls (`char_id == 0`) move without
/// writing any character row. Returns the committed root item without a
/// reread.
pub(crate) async fn persist(
    ex: &(impl Executor + ?Sized), identity: &Identity,
    ship: Item, destination: Destination,
) -> Result<Item, CallError> {
    if !crate::dao::items::inventory::move_item(ex, &ship, destination.station_id,
        FLAG_HANGAR, ship.owner_id, ship.singleton).await.map_err(db_err)? {
        return Err(conflict());
    }
    if identity.char_id > 0 {
        let source_system = identity.system_id.ok_or_else(conflict)?;
        if !crate::dao::characters::ship::relocate(ex, identity.char_id, ship.item_id,
            identity.station_id, source_system, Some(destination.station_id),
            destination.system_id, destination.constellation_id, destination.region_id)
            .await.map_err(db_err)? {
            return Err(conflict());
        }
    }
    Ok(Item { location_id: destination.station_id, flag_id: FLAG_HANGAR, ..ship })
}

/// The single root-hull OnItemChange arguments: the committed row and the
/// old location and flag values the client must invalidate.
fn item_change_args(ship: &Item, old_location: i64, old_flag: i32) -> Vec<Value> {
    let mut changes = vec![(Value::Int(IX_LOCATION_ID), Value::Int(old_location))];
    if old_flag != ship.flag_id { changes.push((Value::Int(IX_FLAG), Value::Int(i64::from(old_flag)))); }
    vec![ship.row(), Value::Dict(changes), Value::None]
}

/// Enter `destination` from a station (cold path) or a live space system
/// (through the shared condition-checkpoint worker).
///
/// `expected` must match the world's online snapshot for `identity` before
/// the move starts, and the GM entry rejects `char_id <= 0`. `publish` runs
/// exactly once after a successful commit: the cold path calls it inside the
/// post-commit dispatch while the market lock is still held; the space path
/// calls it after the checkpoint worker has released the market and source
/// snapshot guards and projected the online session to the destination. The
/// callback must only enqueue session, item and chat effects; it must not
/// await the socket, an actor or another asset transaction. A same-station
/// request is still validated but performs no business writes, trade checks
/// or publications.
pub(in crate::services) async fn enter(
    identity: &Identity, expected: &OnlineSession,
    db: &Db, world: &World, destination: Destination,
    publish: impl FnOnce(super::super::SessionChange, Outbox) -> Result<(), CallError> + Send,
) -> Result<(), CallError> {
    let character = identity.char_id;
    let (Some(ship), Some(source_system)) = (identity.ship_id, identity.system_id) else {
        return Err(no_ship());
    };
    // The account binding is mandatory for this entry; ordinary
    // docking/checkpoint callers may omit it.
    let Some(user) = expected.user_id else { return Err(conflict()); };
    if character <= 0 || expected.character_id != character || expected.ship_id != Some(ship)
        || expected.station_id != identity.station_id || expected.system_id != Some(source_system)
        || world.online_session(character).as_ref() != Some(expected) {
        return Err(conflict());
    }
    // Transaction futures are checked for any borrow lifetime, so the
    // operation captures owned values only.
    let identity = *identity;
    if let Some(station) = identity.station_id {
        // Cold station source: one guarded transaction under the market lock,
        // then the synchronous caller callback before the lock is released.
        let market = crate::services::market::lock().await;
        if world.online_session(character).as_ref() != Some(expected) {
            return Err(conflict());
        }
        let sde = world.sde().clone();
        db.transaction(
            |tx, outbox: &mut Outbox| {
                // Each attempt gets its own owned captures; the boxed future
                // cannot borrow the closure's environment.
                let identity = identity;
                let sde = sde.clone();
                Box::pin(async move {
                    let identity = identity;
                    let sde = sde;
                    let hull = lock_hull(tx, sde.as_ref(), &identity, Some(user)).await?;
                    if station == destination.station_id { return Ok(false); }
                    if crate::dao::trades::open_for(tx, character).await.map_err(db_err)?.is_some() {
                        return Err(trade_open());
                    }
                    let (old_location, old_flag) = (hull.location_id, hull.flag_id);
                    let committed = persist(tx, &identity, hull, destination).await?;
                    outbox.notify_client("OnItemChange", item_change_args(&committed, old_location, old_flag));
                    Ok(true)
                })
            },
            |moved, outbox| async move {
                if moved {
                    // The persisted move is committed; a replaced online session
                    // must not receive the success publication.
                    if world.online_session(character).as_ref() != Some(expected) {
                        return Err(CallError::Internal(anyhow::anyhow!("station relocation committed but the online session changed")));
                    }
                    publish(session_change(destination), outbox)?;
                }
                Ok(())
            },
        ).await?;
        drop(market);
        return Ok(());
    }
    // Live space source: the caller's market and source snapshot guards move
    // into the checkpoint worker, which owns the transaction, source actor
    // removal, session projection and guard release before answering with the
    // committed hull.
    let admission = world.admission(source_system).map_err(db_err)?;
    // Refuse new loaded use on the departing hull and wait out admitted finite
    // work before the market guard serializes the boundary transaction.
    let held_loaded = crate::space::system::boarding::quiesce_ship_loaded(world, source_system, &[ship]).await
        .map_err(|refusal| refusal.into_call_error(conflict()))?;
    let market = crate::services::market::lock().await;
    admission.check().map_err(db_err)?;
    let gate = world.lock_condition_snapshot(source_system).await.map_err(db_err)?;
    if world.online_session(character).as_ref() != Some(expected) {
        return Err(conflict());
    }
    let expected_actor = expected.clone();
    let transition = world.with(source_system, move |s| {
        if !s.combat_session_current(character, Some(&expected_actor)) {
            return Err(conflict());
        }
        if s.pilot_ball(character).is_none_or(|ball| ball.id != ship) {
            return Err(no_ship());
        }
        let mut transition = s.begin_condition_transitions(&[ship]).ok_or_else(conflict)?;
        transition.held_loaded = Some(held_loaded);
        Ok(transition)
    }).await.ok_or_else(conflict)??;
    let committed = world.checkpoint_dock(db.clone(), character, ship, source_system,
        destination, transition, market, gate, admission).await?;
    let mut projected = expected.clone();
    projected.station_id = Some(destination.station_id);
    projected.system_id = Some(destination.system_id);
    if world.online_session(character).as_ref() != Some(&projected) {
        return Err(CallError::Internal(anyhow::anyhow!("station arrival committed but the online session changed")));
    }
    let mut outbox = Outbox::default();
    outbox.notify_client("OnItemChange", item_change_args(&committed, source_system, 0));
    publish(session_change(destination), outbox)
}
