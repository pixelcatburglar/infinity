//! Combat item, ammunition and wreck persistence in game tables.
//! Combat policy, transactions, refusals, effects and notifications remain with services.
//! Positioned creation and cargo stack insertion live in `items::creation` and `items::transfers`.
use crate::db::{executor::Executor, Error};
use super::{charges::{Loaded, Slave}, transfers};
use tokio_postgres::Row;
pub(crate) struct Loot { pub type_id: i32 }
impl Loot { fn from_row(row: &Row) -> Self { Self { type_id: row.get("type_id") } } }

pub(crate) async fn delete_charge(ex: &(impl Executor + ?Sized), ship: i64, flag: i32) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2", &[&ship, &flag]).await
}
pub(crate) async fn delete_empty_charge(ex: &(impl Executor + ?Sized), ship: i64, flag: i32) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2 AND quantity = 0", &[&ship, &flag]).await
}
pub(crate) async fn consume_charge(ex: &(impl Executor + ?Sized), ship: i64, flag: i32) -> Result<Option<Loaded>, Error> {
    Ok(ex.query_opt("UPDATE game.module_charges SET quantity = quantity - 1 WHERE ship_id = $1 AND flag_id = $2 AND quantity > 0 RETURNING type_id, quantity", &[&ship, &flag]).await?.as_ref().map(Loaded::from_row))
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn set_online(ex: &(impl Executor + ?Sized), item: i64, ship: i64, owner: i64, flag: i32, item_type: i32, hull_type: i32, hull_location: i64, hull_flag: i32, hull_singleton: bool, online: bool) -> Result<u64, Error> {
    ex.execute("UPDATE game.items SET online = $10 WHERE item_id = $1 AND location_id = $2 AND owner_id = $3 AND flag_id = $4 AND type_id = $5 AND singleton AND online <> $10 AND EXISTS (SELECT 1 FROM game.items hull WHERE hull.item_id = $2 AND hull.owner_id = $3 AND hull.type_id = $6 AND hull.location_id = $7 AND hull.flag_id = $8 AND hull.singleton = $9)", &[&item, &ship, &owner, &flag, &item_type, &hull_type, &hull_location, &hull_flag, &hull_singleton, &online]).await
}
pub(crate) async fn loot(ex: &(impl Executor + ?Sized), bounty: f64) -> Result<Vec<Loot>, Error> {
    Ok(ex.query("SELECT type_id FROM game.retail_prices WHERE average BETWEEN 1000 AND GREATEST($1::float8, 50000)", &[&bounty]).await?.iter().map(Loot::from_row).collect())
}
pub(crate) async fn insert_loot(ex: &(impl Executor + ?Sized), kind: i32, owner: i64, wreck: i64, quantity: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, 0, $4)", &[&kind, &owner, &wreck, &quantity]).await
}

pub(crate) async fn clear_banks(ex: &(impl Executor + ?Sized), ship: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.weapon_banks WHERE ship_id = $1", &[&ship]).await
}
pub(crate) async fn move_bank(ex: &(impl Executor + ?Sized), ship: i64, target: i64, source: i64) -> Result<(), Error> {
    ex.execute("UPDATE game.weapon_banks SET master_id = $2 WHERE ship_id = $1 AND master_id = $3", &[&ship, &target, &source]).await?;
    ex.execute("INSERT INTO game.weapon_banks (ship_id, master_id, slave_id) VALUES ($1, $2, $3)", &[&ship, &target, &source]).await?;
    Ok(())
}
pub(crate) async fn peel_first(ex: &(impl Executor + ?Sized), ship: i64, master: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("DELETE FROM game.weapon_banks WHERE ship_id = $1 AND slave_id = (SELECT b.slave_id FROM game.weapon_banks b JOIN game.items i ON i.item_id = b.slave_id WHERE b.ship_id = $1 AND b.master_id = $2 ORDER BY i.flag_id, i.item_id LIMIT 1) RETURNING slave_id", &[&ship, &master]).await?.as_ref().map(Slave::from_row).map(|r| r.slave_id))
}
pub(crate) async fn destroy(ex: &(impl Executor + ?Sized), ship: i64, master: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.weapon_banks WHERE ship_id = $1 AND master_id = $2", &[&ship, &master]).await
}
pub(crate) async fn remove_slave(ex: &(impl Executor + ?Sized), ship: i64, slave: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.weapon_banks WHERE ship_id = $1 AND slave_id = $2", &[&ship, &slave]).await
}
pub(crate) async fn delete_reserved(ex: &(impl Executor + ?Sized), ship: i64, rows: &std::collections::BTreeMap<i32, (i32, i64, i64)>) -> Result<bool, Error> {
    for (&flag, &(kind, quantity, _)) in rows {
        if ex.execute("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2 AND type_id = $3 AND quantity = $4", &[&ship, &flag, &kind, &quantity]).await? == 0 { return Ok(false); }
    }
    Ok(true)
}
pub(crate) async fn insert_cargo(ex: &(impl Executor + ?Sized), ship: i64, flag: i32, rows: &std::collections::BTreeMap<(i64, i32), i64>) -> Result<Vec<i64>, Error> {
    let mut ids = Vec::with_capacity(rows.len());
    for (&(owner, kind), &quantity) in rows {
        ids.push(transfers::insert(ex, kind, owner, ship, flag, quantity).await?);
    }
    Ok(ids)
}
pub(crate) async fn apply_loads(ex: &(impl Executor + ?Sized), ship: i64, loads: &[(i64, i32, i32, i64, i64)]) -> Result<(), Error> {
    for &(_, flag, kind, quantity, _) in loads {
        ex.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1, $2, $3, $4)
                         ON CONFLICT (ship_id, flag_id) DO UPDATE SET type_id = EXCLUDED.type_id, quantity = EXCLUDED.quantity", &[&ship, &flag, &kind, &quantity]).await?;
    }
    Ok(())
}
