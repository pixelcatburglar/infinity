//! GM hull-appearance business logic: read-only status and dirt timestamp or
//! player-kill count writes on the shared physical-hull appearance path. A
//! `Current` target freezes the caller's real `OnlineSession` and verifies it
//! against the live registry; an explicit `Item` target accepts foreign,
//! offline, stored and persistent unpiloted hulls alike. Writes hold the
//! market guard through post-commit publication, lock the hull in its
//! observed custody and re-verify the type.
//! Condition, position, fittings, ownership and other item fields never change.
use infinity_core::consts::{CATEGORY_SHIP, FLAG_HANGAR, GM_ROLES};
use infinity_core::messages;
use infinity_core::session::OnlineSession;
use crate::dao::items::{ship as item_dao, ship_appearance::{self, StoredAppearance}, ship_configuration};
use crate::db::{outbox::Outbox, Db};
use crate::sde::Sde;
use crate::services::{db_err, market, notify_error, CallError};
use crate::space::World;
use super::dirt::DirtSetting;
use super::{AppearanceChanged, Publication};

/// `game.items.flag_id` of a hull physically present in space.
const SPACE_SHIP_FLAG: i32 = 0;

/// A `/dirt` target: the caller's frozen current ship or one explicit item.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    Current(OnlineSession),
    Item(i64),
}

/// One appearance write; each variant carries its already validated setting.
#[derive(Clone, Copy)]
pub(crate) enum Change {
    Dirt(DirtSetting),
    /// Player-kill count; rejected up front when negative.
    Kills(i64),
}

fn denied() -> CallError { notify_error(messages::gm::GM_ROLE_REQUIRED) }
fn invalid() -> CallError { notify_error(messages::ship::HULL_APPEARANCE_INACCESSIBLE) }

/// Resolve the target ship and, for `Current`, require a complete frozen
/// session that the live registry still reports verbatim.
fn target_ship(world: &World, target: &Target) -> Result<i64, CallError> {
    match target {
        Target::Current(session) => {
            if session.user_id.is_none() || session.system_id.is_none() { return Err(invalid()); }
            let ship = session.ship_id.filter(|id| *id > 0).ok_or_else(invalid)?;
            if world.online_session(session.character_id).as_ref() != Some(session) { return Err(invalid()); }
            Ok(ship)
        }
        Target::Item(ship) => (*ship > 0).then_some(*ship).ok_or_else(invalid),
    }
}

/// Refuse anything that is not a singleton physical ship per the SDE category.
fn physical_hull(sde: &Sde, stored: StoredAppearance) -> Result<StoredAppearance, CallError> {
    if sde.types.get(stored.hull.type_id).and_then(|ty| ty.category_id) != Some(CATEGORY_SHIP) { return Err(invalid()); }
    Ok(stored)
}

/// Read the stored appearance. A hull without a `game.ship_dirt` row returns
/// `dirt_timestamp: None` verbatim; status never initializes or writes.
pub(crate) async fn inspect(db: &Db, world: &World, sde: &Sde, role: i64, target: &Target) -> Result<StoredAppearance, CallError> {
    if role & GM_ROLES == 0 { return Err(denied()); }
    let ship = target_ship(world, target)?;
    physical_hull(sde, ship_appearance::load(db, ship).await.map_err(db_err)?.ok_or_else(invalid)?)
}

/// Write one appearance change under the market guard and publish it after
/// commit. A space-rooted hull in a real solar system probes its actor once:
/// a stopped actor permits a persisted-only `NotLoaded` write, while a running
/// actor without a valid live incarnation refuses before anything is written.
/// The returned `StoredAppearance` carries the committed scalar on the field
/// the change targeted, never the pre-write load result.
pub(crate) async fn change(db: &Db, world: &World, sde: &Sde, role: i64, target: Target, change: Change)
    -> Result<(StoredAppearance, Publication), CallError> {
    if let Change::Kills(count) = change {
        if count < 0 { return Err(notify_error(messages::gm::GM_KILLMARKS_COUNT_INVALID)); }
    }
    if role & GM_ROLES == 0 { return Err(denied()); }
    let ship = target_ship(world, &target)?;
    let _market = market::lock().await;
    let stored = physical_hull(sde, ship_appearance::load(db, ship).await.map_err(db_err)?.ok_or_else(invalid)?)?;
    // The frozen Current snapshot can rotate while the market guard is awaited;
    // the live registry must still report it verbatim, and the stored hull must
    // be that session's own ship in its expected custody, never an older ship.
    if let Target::Current(session) = &target {
        if world.online_session(session.character_id).as_ref() != Some(session) { return Err(invalid()); }
        let flag = if session.station_id.is_some() { FLAG_HANGAR } else { SPACE_SHIP_FLAG };
        let location = session.station_id.or(session.system_id).ok_or_else(invalid)?;
        if stored.hull.owner_id != session.character_id || stored.hull.location_id != location || stored.hull.flag_id != flag {
            return Err(invalid());
        }
    }
    let fence = if stored.hull.flag_id == SPACE_SHIP_FLAG && sde.map.system(stored.hull.location_id).is_some() {
        match world.with(stored.hull.location_id, move |s| s.capture_appearance(ship)).await {
            Some(Some(fence)) => {
                if fence.owner != stored.hull.owner_id || fence.type_id != stored.hull.type_id { return Err(invalid()); }
                Some(fence)
            }
            Some(None) => return Err(invalid()),
            None => None,
        }
    } else { None };
    // StoredAppearance carries an owned name, so the retryable operation returns
    // only the committed scalar; the loaded row is moved into the result once,
    // after the post-commit dispatch has settled.
    let hull = stored.hull;
    let (committed, publication) = db.transaction(|tx, outbox: &mut Outbox<AppearanceChanged>| {
        let target = target.clone();
        let fence = fence.clone();
        Box::pin(async move {
            let kind = match &target {
                Target::Current(session) => {
                    let flag = if session.station_id.is_some() { FLAG_HANGAR } else { SPACE_SHIP_FLAG };
                    let system = session.system_id.ok_or_else(invalid)?;
                    let user = session.user_id.ok_or_else(invalid)?;
                    ship_configuration::lock_selected(tx, session.character_id, user, ship, session.station_id, system, flag).await.map_err(db_err)?
                }
                Target::Item(_) => item_dao::lock_owned_at(tx, ship, hull.owner_id, hull.location_id, hull.flag_id).await.map_err(db_err)?,
            };
            if kind != Some(hull.type_id) { return Err(invalid()); }
            match change {
                Change::Dirt(setting) => super::set_dirt_tx(tx, &hull, setting.timestamp, fence.as_ref(), outbox).await,
                Change::Kills(count) => super::set_kills_tx(tx, &hull, count, fence.as_ref(), outbox).await,
            }
        })
    }, |scalar, outbox| super::dispatch_changed(scalar, outbox, world)).await?;
    let stored = match change {
        Change::Dirt(_) => StoredAppearance { dirt_timestamp: Some(committed), ..stored },
        Change::Kills(_) => StoredAppearance { player_kills: committed, ..stored },
    };
    Ok((stored, publication))
}
