//! Persistent ship SKIN licenses and applications in `game.ship_skin_licenses`,
//! `game.ship_skin_applications` and `game.items`. The owning services mirror
//! `cosmeticsSvc.py` and `shipSkinApplicationSvc.py`; this DAO has no wire contract.
//! Compatibility, visibility, expiry scheduling and transaction ownership are not handled here.
//! Item owner changes and repackaging clear invalid applications atomically at their
//! mutation roots; same-owner assembled moves retain them, and item deletion cascades.
//! Personal licenses remain with their character. Drone return and abandonment are
//! drone-category-only service paths (`services/drones/inventory/returning.rs` and
//! `services/drones/inventory.rs`), so they cannot carry ship applications.

use crate::{dao::items::{inventory::Matched, loading::Item}, db::{executor::Executor, Error}};
use tokio_postgres::Row;

pub(crate) struct License {
    pub skin_id: i32,
    pub expires_at: Option<i64>,
}

impl License {
    fn from_row(row: &Row) -> Self {
        Self { skin_id: row.get("skin_id"), expires_at: row.get("expires_at") }
    }
}

pub(crate) struct Application {
    pub ship_item_id: i64,
    pub type_id: i32,
    pub location_id: i64,
}

impl Application {
    fn from_row(row: &Row) -> Self {
        Self {
            ship_item_id: row.get("ship_item_id"), type_id: row.get("type_id"),
            location_id: row.get("location_id"),
        }
    }
}

pub(crate) struct RemovedLicense {
    pub skin_id: i32,
    pub applications: Vec<Application>,
}

pub(crate) struct ShipState {
    pub ship_item_id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub character_id: Option<i64>,
    pub skin_id: Option<i32>,
}

impl ShipState {
    fn from_row(row: &Row) -> Self {
        Self {
            ship_item_id: row.get("ship_item_id"), type_id: row.get("type_id"),
            owner_id: row.get("owner_id"), character_id: row.get("character_id"),
            skin_id: row.get("skin_id"),
        }
    }
}

pub(crate) async fn licensed(ex: &(impl Executor + ?Sized), character_id: i64, now: i64) -> Result<Vec<License>, Error> {
    Ok(ex.query("SELECT l.skin_id, l.expires_at FROM game.ship_skin_licenses l JOIN game.characters c ON c.character_id=l.character_id WHERE l.character_id=$1 AND NOT c.deleted AND (l.expires_at IS NULL OR l.expires_at >= $2) ORDER BY l.skin_id", &[&character_id, &now]).await?.iter().map(License::from_row).collect())
}

pub(crate) async fn get_license(ex: &(impl Executor + ?Sized), character_id: i64, skin_id: i32, now: i64) -> Result<Option<License>, Error> {
    Ok(ex.query_opt("SELECT l.skin_id, l.expires_at FROM game.ship_skin_licenses l JOIN game.characters c ON c.character_id=l.character_id WHERE l.character_id=$1 AND l.skin_id=$2 AND NOT c.deleted AND (l.expires_at IS NULL OR l.expires_at >= $3)", &[&character_id, &skin_id, &now]).await?.as_ref().map(License::from_row))
}

pub(crate) async fn get_stored_license(ex: &(impl Executor + ?Sized), character_id: i64, skin_id: i32) -> Result<Option<License>, Error> {
    Ok(ex.query_opt("SELECT skin_id, expires_at FROM game.ship_skin_licenses WHERE character_id=$1 AND skin_id=$2", &[&character_id, &skin_id]).await?.as_ref().map(License::from_row))
}

pub(crate) async fn applications_for_character(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<Vec<Application>, Error> {
    Ok(ex.query("SELECT a.ship_item_id, i.type_id, i.location_id FROM game.ship_skin_applications a JOIN game.items i ON i.item_id=a.ship_item_id WHERE a.character_id=$1 ORDER BY a.ship_item_id", &[&character_id]).await?.iter().map(Application::from_row).collect())
}

pub(crate) async fn due_licenses(ex: &(impl Executor + ?Sized), character_id: i64, now: i64) -> Result<Vec<License>, Error> {
    Ok(ex.query("SELECT l.skin_id, l.expires_at FROM game.ship_skin_licenses l JOIN game.characters c ON c.character_id=l.character_id WHERE l.character_id=$1 AND NOT c.deleted AND l.expires_at < $2 ORDER BY l.skin_id", &[&character_id, &now]).await?.iter().map(License::from_row).collect())
}

pub(crate) async fn remove_license(ex: &(impl Executor + ?Sized), character_id: i64, skin_id: i32, expires_before: Option<i64>) -> Result<Option<RemovedLicense>, Error> {
    let rows = ex.query("WITH applications AS MATERIALIZED (SELECT a.ship_item_id, i.type_id, i.location_id FROM game.ship_skin_applications a JOIN game.items i ON i.item_id=a.ship_item_id WHERE a.character_id=$1 AND a.skin_id=$2), removed AS (DELETE FROM game.ship_skin_licenses WHERE character_id=$1 AND skin_id=$2 AND ($3::bigint IS NULL OR expires_at < $3) RETURNING skin_id) SELECT r.skin_id AS removed_skin_id, a.ship_item_id, a.type_id, a.location_id FROM removed r LEFT JOIN applications a ON true ORDER BY a.ship_item_id", &[&character_id, &skin_id, &expires_before]).await?;
    Ok(rows.first().map(|first| RemovedLicense {
        skin_id: first.get("removed_skin_id"),
        applications: rows.iter().filter(|row| row.get::<_, Option<i64>>("ship_item_id").is_some()).map(Application::from_row).collect(),
    }))
}

pub(crate) async fn next_expiry(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_one("SELECT MIN(l.expires_at) AS expires_at FROM game.ship_skin_licenses l JOIN game.characters c ON c.character_id=l.character_id WHERE l.character_id=$1 AND NOT c.deleted", &[&character_id]).await?.get("expires_at"))
}

pub(crate) async fn finite_characters(ex: &(impl Executor + ?Sized)) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT DISTINCT l.character_id FROM game.ship_skin_licenses l JOIN game.characters c ON c.character_id=l.character_id WHERE l.expires_at IS NOT NULL AND NOT c.deleted ORDER BY l.character_id", &[]).await?.iter().map(|row| row.get("character_id")).collect())
}

pub(crate) async fn update_expiry(ex: &(impl Executor + ?Sized), character_id: i64, skin_id: i32, expected: Option<i64>, expires: Option<i64>) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.ship_skin_licenses SET expires_at=$4 WHERE character_id=$1 AND skin_id=$2 AND expires_at IS NOT DISTINCT FROM $3", &[&character_id, &skin_id, &expected, &expires]).await? == 1)
}

pub(crate) async fn insert_license(ex: &(impl Executor + ?Sized), character_id: i64, skin_id: i32, expires_at: Option<i64>) -> Result<bool, Error> {
    Ok(ex.execute("INSERT INTO game.ship_skin_licenses (character_id, skin_id, expires_at) VALUES ($1,$2,$3) ON CONFLICT (character_id, skin_id) DO NOTHING", &[&character_id, &skin_id, &expires_at]).await? == 1)
}

pub(crate) async fn states_for_ships(ex: &(impl Executor + ?Sized), ship_ids: &[i64], now: i64) -> Result<Vec<ShipState>, Error> {
    Ok(ex.query("SELECT i.item_id AS ship_item_id, i.type_id, i.owner_id, a.character_id, a.skin_id FROM game.items i LEFT JOIN (game.ship_skin_applications a JOIN game.ship_skin_licenses l ON l.character_id=a.character_id AND l.skin_id=a.skin_id AND (l.expires_at IS NULL OR l.expires_at >= $2)) ON a.ship_item_id=i.item_id AND a.character_id=i.owner_id AND i.singleton AND i.jump_clone_id IS NULL WHERE i.item_id=ANY($1) ORDER BY i.item_id", &[&ship_ids, &now]).await?.iter().map(ShipState::from_row).collect())
}

pub(crate) async fn set_application(ex: &(impl Executor + ?Sized), character_id: i64, expected_ship: &Item, skin_id: i32, now: i64) -> Result<bool, Error> {
    Ok(ex.execute("INSERT INTO game.ship_skin_applications (ship_item_id, character_id, skin_id) SELECT i.item_id, $1, l.skin_id FROM game.items i JOIN game.ship_skin_licenses l ON l.character_id=$1 AND l.skin_id=$8 AND (l.expires_at IS NULL OR l.expires_at >= $9) WHERE i.item_id=$2 AND i.type_id=$3 AND i.owner_id=$1 AND i.owner_id=$4 AND i.location_id=$5 AND i.flag_id=$6 AND i.quantity=$7 AND i.singleton AND i.singleton=$10 AND i.jump_clone_id IS NULL ON CONFLICT (ship_item_id) DO UPDATE SET character_id=EXCLUDED.character_id, skin_id=EXCLUDED.skin_id", &[&character_id, &expected_ship.item_id, &expected_ship.type_id, &expected_ship.owner_id, &expected_ship.location_id, &expected_ship.flag_id, &expected_ship.quantity, &skin_id, &now, &expected_ship.singleton]).await? == 1)
}

pub(crate) async fn clear_application(ex: &(impl Executor + ?Sized), character_id: i64, expected_ship: &Item) -> Result<bool, Error> {
    let row = ex.query_one("WITH matched AS (SELECT i.item_id FROM game.items i WHERE i.item_id=$2 AND i.type_id=$3 AND i.owner_id=$1 AND i.owner_id=$4 AND i.location_id=$5 AND i.flag_id=$6 AND i.quantity=$7 AND i.singleton AND i.singleton=$8 AND i.jump_clone_id IS NULL), removed AS (DELETE FROM game.ship_skin_applications a USING matched m WHERE a.ship_item_id=m.item_id) SELECT EXISTS (SELECT 1 FROM matched) AS matched", &[&character_id, &expected_ship.item_id, &expected_ship.type_id, &expected_ship.owner_id, &expected_ship.location_id, &expected_ship.flag_id, &expected_ship.quantity, &expected_ship.singleton]).await?;
    Ok(Matched::from_row(&row).matched)
}

#[cfg(test)]
mod tests;
