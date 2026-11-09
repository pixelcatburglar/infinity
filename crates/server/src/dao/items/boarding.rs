//! Guarded ship selection and sampled positions in `game.characters` and `game.items`.
//! The client ship-boarding services supply type and location policy; there is no wire contract here.
//! The caller holds the market lock and a transaction, and must roll it back on any refusal.
//! Static ship classification, notifications and space publication are not handled here.

use infinity_core::consts::FLAG_HANGAR;
use crate::db::{self, executor::Executor};

/// Lock and validate the account's selected, assembled in-space hull.
pub(crate) async fn guard_current(ex: &(impl Executor + ?Sized), character: i64, user: i64, old: i64, system: i64, old_type: i32) -> Result<bool, db::Error> {
    if ex.query_opt("SELECT character_id FROM game.characters WHERE character_id=$1 AND user_id=$2 AND ship_id=$3 AND NOT deleted AND station_id IS NULL AND solar_system_id=$4 FOR UPDATE", &[&character, &user, &old, &system]).await?.is_none() { return Ok(false); }
    Ok(ex.query_opt("SELECT item_id FROM game.items WHERE item_id=$1 AND owner_id=$2 AND location_id=$3 AND type_id=$4 AND flag_id=0 AND singleton AND quantity=1 AND jump_clone_id IS NULL AND x > '-Infinity'::float8 AND x < 'Infinity'::float8 AND y > '-Infinity'::float8 AND y < 'Infinity'::float8 AND z > '-Infinity'::float8 AND z < 'Infinity'::float8 FOR UPDATE", &[&old, &character, &system, &old_type]).await?.is_some())
}

/// Position an already moved hull only if its committed custody still matches.
pub(crate) async fn position_moved(ex: &(impl Executor + ?Sized), ship: i64, owner: i64, system: i64, pos: [f64; 3]) -> Result<bool, db::Error> {
    if !pos.iter().all(|v| v.is_finite()) { return Ok(false); }
    Ok(ex.execute("UPDATE game.items SET x=$4,y=$5,z=$6 WHERE item_id=$1 AND owner_id=$2 AND location_id=$3 AND flag_id=0 AND singleton AND quantity=1 AND jump_clone_id IS NULL", &[&ship, &owner, &system, &pos[0], &pos[1], &pos[2]]).await? == 1)
}

/// Switch a locked current character to an unoccupied, positioned owned hull.
pub(crate) async fn select_moved(ex: &(impl Executor + ?Sized), character: i64, user: i64, old: i64, target: i64, system: i64) -> Result<bool, db::Error> {
    if old == target { return Ok(false); }
    Ok(ex.execute("UPDATE game.characters SET ship_id=$4 WHERE character_id=$1 AND user_id=$2 AND ship_id=$3 AND NOT deleted AND station_id IS NULL AND solar_system_id=$5 AND EXISTS (SELECT 1 FROM game.items WHERE item_id=$4 AND owner_id=$1 AND location_id=$5 AND flag_id=0 AND singleton AND quantity=1 AND jump_clone_id IS NULL AND x > '-Infinity'::float8 AND x < 'Infinity'::float8 AND y > '-Infinity'::float8 AND y < 'Infinity'::float8 AND z > '-Infinity'::float8 AND z < 'Infinity'::float8) AND NOT EXISTS (SELECT 1 FROM game.characters selected WHERE selected.ship_id=$4)", &[&character, &user, &old, &target, &system]).await? == 1)
}

/// Reject a hull selected by any character, including a disconnected pilot.
pub(crate) async fn unselected(ex: &(impl Executor + ?Sized), ship: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT character_id FROM game.characters WHERE ship_id=$1 LIMIT 1", &[&ship]).await?.is_none())
}


/// Captured facts required to distinguish a committed stored-hull transition.
#[derive(Clone, Copy)]
pub(crate) enum RecoveryPlan {
    Board { character: i64, user: i64, system: i64, old: i64, old_type: i32, target: i64, target_type: i32 },
    Store { character: i64, user: i64, system: i64, old: i64, old_type: i32, host: i64 },
    BoardStored { character: i64, user: i64, system: i64, old: i64, old_type: i32,
        host: i64, target: i64, target_type: i32, prior_owner: i64 },
}

pub(crate) enum Recovery {
    Committed { target: super::space_ships::FreeShip, old: Option<super::space_ships::FreeShip> },
    Aborted,
    Unknown,
}

/// Resolve commit or rollback from one database snapshot; ambiguous facts remain unknown.
pub(crate) async fn recovery(ex: &(impl Executor + ?Sized), plan: &RecoveryPlan) -> Result<Recovery, db::Error> {
    let (character, user, system, old, old_type, host, target, target_type, prior_owner, store, ordinary) = match *plan {
        RecoveryPlan::Board { character, user, system, old, old_type, target, target_type } =>
            (character, user, system, old, old_type, system, Some(target), target_type, character, false, true),
        RecoveryPlan::Store { character, user, system, old, old_type, host } =>
            (character, user, system, old, old_type, host, None, infinity_core::consts::TYPE_CAPSULE, character, true, false),
        RecoveryPlan::BoardStored { character, user, system, old, old_type, host, target, target_type, prior_owner } =>
            (character, user, system, old, old_type, host, Some(target), target_type, prior_owner, false, false),
    };
    let flag = infinity_core::consts::FLAG_SHIP_HANGAR;
    let row = ex.query_opt("SELECT ch.ship_id AS selected, ch.corporation_id,
        COALESCE(co.alliance_id,-1)::bigint AS alliance_id,
        old_item.x AS old_x, old_item.y AS old_y, old_item.z AS old_z,
        target_item.x AS target_x, target_item.y AS target_y, target_item.z AS target_z,
        EXISTS (SELECT 1 FROM game.items i WHERE i.item_id=$4 AND i.type_id=$5 AND i.owner_id=$1 AND i.location_id=$3 AND i.flag_id=0 AND i.singleton AND i.quantity=1 AND i.jump_clone_id IS NULL AND i.x > '-Infinity'::float8 AND i.x < 'Infinity'::float8 AND i.y > '-Infinity'::float8 AND i.y < 'Infinity'::float8 AND i.z > '-Infinity'::float8 AND i.z < 'Infinity'::float8) AS old_space,
        EXISTS (SELECT 1 FROM game.items i WHERE i.item_id=$4 AND i.type_id=$5 AND i.owner_id=$1 AND i.location_id=$6 AND i.flag_id=$10 AND i.singleton AND i.quantity=1 AND i.jump_clone_id IS NULL) AS old_stored,
        EXISTS (SELECT 1 FROM game.items i WHERE i.item_id=COALESCE($7,ch.ship_id) AND i.item_id<>$4 AND i.type_id=$8 AND i.owner_id=$1 AND i.location_id=$3 AND i.flag_id=0 AND i.singleton AND i.quantity=1 AND i.jump_clone_id IS NULL AND i.x > '-Infinity'::float8 AND i.x < 'Infinity'::float8 AND i.y > '-Infinity'::float8 AND i.y < 'Infinity'::float8 AND i.z > '-Infinity'::float8 AND i.z < 'Infinity'::float8) AS target_space,
        EXISTS (SELECT 1 FROM game.items i WHERE i.item_id=$7 AND i.type_id=$8 AND i.owner_id=$9 AND i.location_id=$6 AND i.flag_id=$10 AND i.singleton AND i.quantity=1 AND i.jump_clone_id IS NULL) AS target_stored,
        NOT EXISTS (SELECT 1 FROM game.characters c WHERE c.ship_id=$7) AS target_free,
        NOT EXISTS (SELECT 1 FROM game.characters c WHERE c.ship_id=$4) AS old_free,
        NOT EXISTS (SELECT 1 FROM game.characters c WHERE c.character_id<>$1 AND c.ship_id=ch.ship_id) AS target_exclusive
        FROM game.characters ch
        LEFT JOIN game.corporations co ON co.corporation_id=ch.corporation_id
        LEFT JOIN game.items old_item ON old_item.item_id=$4
        LEFT JOIN game.items target_item ON target_item.item_id=COALESCE($7,ch.ship_id)
        WHERE ch.character_id=$1 AND ch.user_id=$2 AND NOT ch.deleted AND ch.station_id IS NULL AND ch.solar_system_id=$3",
        &[&character, &user, &system, &old, &old_type, &host, &target, &target_type, &prior_owner, &flag]).await?;
    let Some(row) = row else { return Ok(Recovery::Unknown) };
    let selected: Option<i64> = row.get("selected");
    let old_space: bool = row.get("old_space");
    let old_stored: bool = row.get("old_stored");
    let target_space: bool = row.get("target_space");
    let target_stored: bool = row.get("target_stored");
    let target_free: bool = row.get("target_free");
    let old_free: bool = row.get("old_free");
    let target_exclusive: bool = row.get("target_exclusive");
    if old_free && target_exclusive && target_space && selected.is_some() && (store || selected == target) && (if store { old_stored } else { old_space }) {
        let corporation_id = row.get("corporation_id");
        let alliance_id = row.get("alliance_id");
        let target = super::space_ships::FreeShip {
            item_id: selected.unwrap(), type_id: target_type, owner_id: character, corporation_id, alliance_id,
            pos: [row.get("target_x"), row.get("target_y"), row.get("target_z")],
        };
        let old = if store { None } else { Some(super::space_ships::FreeShip {
            item_id: old, type_id: old_type, owner_id: character, corporation_id, alliance_id,
            pos: [row.get("old_x"), row.get("old_y"), row.get("old_z")],
        }) };
        return Ok(Recovery::Committed { target, old });
    }
    // Another character selecting the still-stored target can itself cause the refusal.
    if selected == Some(old) && old_space && (store || if ordinary { target_space && target_free } else { target_stored }) { return Ok(Recovery::Aborted); }
    Ok(Recovery::Unknown)
}

/// Switch between owned positioned ships, saving both actor-sampled positions.
pub(crate) async fn board_in_space(ex: &(impl Executor + ?Sized), character: i64, old: i64, target: i64, system: i64, old_type: i32, target_type: i32, old_pos: [f64; 3], target_pos: [f64; 3]) -> Result<bool, db::Error> {
    if !old_pos.iter().chain(target_pos.iter()).all(|v| v.is_finite()) { return Ok(false); }
    board(ex, character, old, target, system, old_type, target_type, Some((old_pos, target_pos))).await
}

/// Switch between owned assembled ships in the character's station hangar.
pub(crate) async fn board_in_station(ex: &(impl Executor + ?Sized), character: i64, old: i64, target: i64, station: i64, old_type: i32, target_type: i32) -> Result<bool, db::Error> {
    board(ex, character, old, target, station, old_type, target_type, None).await
}

async fn board(ex: &(impl Executor + ?Sized), character: i64, old: i64, target: i64, location: i64, old_type: i32, target_type: i32, positions: Option<([f64; 3], [f64; 3])>) -> Result<bool, db::Error> {
    if old == target { return Ok(false); }
    let space = positions.is_some();
    let flag = if space { 0 } else { FLAG_HANGAR };
    if ex.query_opt("SELECT character_id FROM game.characters WHERE character_id=$1 AND ship_id=$2 AND NOT deleted AND (($3 AND station_id IS NULL AND solar_system_id=$4) OR (NOT $3 AND station_id=$4)) FOR UPDATE", &[&character, &old, &space, &location]).await?.is_none() { return Ok(false); }
    let ids = [old, target];
    let rows = ex.query("SELECT item_id FROM game.items WHERE item_id=ANY($1) AND jump_clone_id IS NULL ORDER BY item_id FOR UPDATE", &[&&ids[..]]).await?;
    if rows.len() != 2 { return Ok(false); }
    for (id, kind) in [(old, old_type), (target, target_type)] {
        if ex.query_opt("SELECT item_id FROM game.items WHERE item_id=$1 AND type_id=$2 AND owner_id=$3 AND location_id=$4 AND flag_id=$5 AND singleton AND quantity=1 AND (NOT $6 OR (x > '-Infinity'::float8 AND x < 'Infinity'::float8 AND y > '-Infinity'::float8 AND y < 'Infinity'::float8 AND z > '-Infinity'::float8 AND z < 'Infinity'::float8))", &[&id, &kind, &character, &location, &flag, &space]).await?.is_none() { return Ok(false); }
    }
    if !unselected(ex, target).await? { return Ok(false); }
    if let Some((old_pos, target_pos)) = positions {
        let mut writes = [(old, old_type, old_pos), (target, target_type, target_pos)];
        writes.sort_unstable_by_key(|entry| entry.0);
        for (id, kind, pos) in writes {
            if ex.execute("UPDATE game.items SET x=$6,y=$7,z=$8 WHERE item_id=$1 AND type_id=$2 AND owner_id=$3 AND location_id=$4 AND flag_id=$5 AND singleton AND quantity=1 AND x > '-Infinity'::float8 AND x < 'Infinity'::float8 AND y > '-Infinity'::float8 AND y < 'Infinity'::float8 AND z > '-Infinity'::float8 AND z < 'Infinity'::float8", &[&id, &kind, &character, &location, &flag, &pos[0], &pos[1], &pos[2]]).await? != 1 { return Ok(false); }
        }
    }
    Ok(ex.execute("UPDATE game.characters SET ship_id=$3 WHERE character_id=$1 AND ship_id=$2 AND NOT deleted AND (($4 AND station_id IS NULL AND solar_system_id=$5) OR (NOT $4 AND station_id=$5)) AND NOT EXISTS (SELECT 1 FROM game.characters selected WHERE selected.ship_id=$3)", &[&character, &old, &target, &space, &location]).await? == 1)
}

#[cfg(test)] mod tests;
