//! Active fit data from game items and module charges; skill levels come from `skills::levels`.
//! This DAO has no direct client service; dogma evaluation and SDE category filtering remain in the caller.
//! Shared item type lookup lives in `items::inventory`; activation and consumption are not handled here.
use crate::db::{executor::Executor, Error};
use crate::consts::{FLAG_BOOSTER, FLAG_DRONE_BAY, FLAG_FITTING_FIRST, FLAG_FITTING_LAST, FLAG_IMPLANT, FLAG_RIG_FIRST, FLAG_RIG_LAST, FLAG_SUBSYSTEM_FIRST, FLAG_SUBSYSTEM_LAST};
use tokio_postgres::Row;

pub(crate) struct Module { pub item_id: i64, pub type_id: i32, pub online: bool, pub flag_id: i32 }
impl Module { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id"), type_id: r.get("type_id"), online: r.get("online"), flag_id: r.get("flag_id") } } }
pub(crate) struct Charge { pub flag_id: i32, pub type_id: i32 }
impl Charge { fn from_row(r: &Row) -> Self { Self { flag_id: r.get("flag_id"), type_id: r.get("type_id") } } }

pub(crate) struct BrainItem { pub item_id: i64, pub type_id: i32, pub flag_id: i32 }
impl BrainItem { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id"), type_id: r.get("type_id"), flag_id: r.get("flag_id") } } }
pub(crate) struct Drone { pub item_id: i64, pub type_id: i32 }
impl Drone { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id"), type_id: r.get("type_id") } } }

pub(crate) async fn brain_items(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Vec<BrainItem>, Error> {
    Ok(ex.query("SELECT item_id, type_id, flag_id FROM game.items WHERE owner_id = $1 AND location_id = $1 AND flag_id IN ($2, $3) ORDER BY item_id",
        &[&char_id, &FLAG_BOOSTER, &FLAG_IMPLANT]).await?.iter().map(BrainItem::from_row).collect())
}
pub(crate) async fn drones(ex: &(impl Executor + ?Sized), char_id: i64, ship: i64) -> Result<Vec<Drone>, Error> {
    Ok(ex.query("SELECT item_id, type_id FROM game.items WHERE owner_id = $1 AND location_id = $2 AND flag_id = $3 ORDER BY item_id",
        &[&char_id, &ship, &FLAG_DRONE_BAY]).await?.iter().map(Drone::from_row).collect())
}

pub(crate) async fn modules(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<Module>, Error> {
    Ok(ex.query("SELECT item_id, type_id, COALESCE(online, false) AS online, flag_id FROM game.items WHERE location_id = $1
                 AND (flag_id BETWEEN $2 AND $3 OR flag_id BETWEEN $4 AND $5 OR flag_id BETWEEN $6 AND $7)",
        &[&ship, &FLAG_FITTING_FIRST, &FLAG_FITTING_LAST, &FLAG_RIG_FIRST, &FLAG_RIG_LAST, &FLAG_SUBSYSTEM_FIRST, &FLAG_SUBSYSTEM_LAST]).await?.iter().map(Module::from_row).collect())
}

/// Owned hulls containing online fitting items, in hull lock order.
pub(crate) async fn owner_online_ships(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT DISTINCT h.item_id AS ship_id FROM game.items h JOIN game.items m ON m.location_id = h.item_id AND m.owner_id = h.owner_id
                 WHERE h.owner_id = $1 AND m.online AND (m.flag_id BETWEEN $2 AND $3 OR m.flag_id BETWEEN $4 AND $5 OR m.flag_id BETWEEN $6 AND $7)
                 ORDER BY h.item_id",
        &[&owner, &FLAG_FITTING_FIRST, &FLAG_FITTING_LAST, &FLAG_RIG_FIRST, &FLAG_RIG_LAST, &FLAG_SUBSYSTEM_FIRST, &FLAG_SUBSYSTEM_LAST]).await?.iter().map(|row| row.get("ship_id")).collect())
}
pub(crate) async fn charges(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<Charge>, Error> {
    Ok(ex.query("SELECT flag_id, type_id FROM game.module_charges WHERE ship_id = $1", &[&ship]).await?.iter().map(Charge::from_row).collect())
}
