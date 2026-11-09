//! Rookie ships, their persistent fit items, and positioned singleton items in `game.items`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Ship selection, character association, transactions, refusals, notifications and World are caller concerns.
//! Rookie fitting does not insert charges; the supplied executor owns atomicity.
use infinity_core::consts::FLAG_DRONE_BAY;
use crate::db::{self, executor::Executor};
use super::inventory::ItemId;
pub(crate) async fn rookie_ship(ex: &(impl Executor + ?Sized), ship_type: i32, character_id: i64, station_id: i64, flag: i32) -> Result<i64, db::Error> {
    Ok(ItemId::from_row(&ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton)
             VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id", &[&ship_type, &character_id, &station_id, &flag]).await?).item_id)
}
pub(crate) async fn fitted_items(ex: &(impl Executor + ?Sized), ship: i64, owner: i64, items: &[(i32, i32, i32)]) -> Result<(), db::Error> {
    for &(type_id, flag_id, quantity) in items {
        let singleton = flag_id != FLAG_DRONE_BAY;
        ex.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, online)
                    VALUES ($1, $2, $3, $4, $5, $6, $6)", &[&type_id, &owner, &ship, &flag_id, &i64::from(quantity), &singleton]).await?;
    }
    Ok(())
}
pub(crate) async fn positioned(ex: &(impl Executor + ?Sized), kind: i32, owner: i64, system: i64, pos: [f64; 3]) -> Result<i64, db::Error> {
    Ok(ItemId::from_row(&ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z)
             VALUES ($1, $2, $3, 0, 1, true, $4, $5, $6) RETURNING item_id", &[&kind, &owner, &system, &pos[0], &pos[1], &pos[2]]).await?).item_id)
}
