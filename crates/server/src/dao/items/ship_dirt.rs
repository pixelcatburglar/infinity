//! Physical hull dirt timestamps in `game.ship_dirt` and custody guards in `game.items`.
//! The ship service supplies the client `cleanShipButton.py` protocol.
//! Timestamps follow the item, not its owner; transactions and authorization are caller concerns.
//! This DAO does not emit notifications or apply bay configuration policy.
use crate::db::{self, executor::Executor};
#[derive(Clone, Copy)]
pub(crate) struct ObservedHull { pub item_id: i64, pub owner_id: i64, pub location_id: i64, pub flag_id: i32, pub type_id: i32 }
impl From<&super::loading::GameItem> for ObservedHull {
    fn from(item: &super::loading::GameItem) -> Self {
        Self { item_id: item.item_id, owner_id: item.owner_id, location_id: item.location_id, flag_id: item.flag_id, type_id: item.type_id }
    }
}

struct Timestamp { dirt_timestamp: i64 }
impl Timestamp { fn from_row(row: &tokio_postgres::Row) -> Self { Self { dirt_timestamp: row.get("dirt_timestamp") } } }

/// Lock the observed physical hull while initializing or reading its timestamp.
pub(crate) async fn get_or_init(ex: &(impl Executor + ?Sized), item: &ObservedHull, now: i64) -> Result<Option<i64>, db::Error> {
    ex.execute("INSERT INTO game.ship_dirt (ship_item_id,dirt_timestamp) SELECT item_id,$6 FROM game.items WHERE item_id=$1 AND owner_id=$2 AND location_id=$3 AND flag_id=$4 AND type_id=$5 AND singleton AND quantity=1 AND jump_clone_id IS NULL FOR UPDATE ON CONFLICT (ship_item_id) DO NOTHING", &[&item.item_id, &item.owner_id, &item.location_id, &item.flag_id, &item.type_id, &now]).await?;
    Ok(ex.query_opt("SELECT d.dirt_timestamp FROM game.ship_dirt d JOIN game.items i ON i.item_id=d.ship_item_id WHERE i.item_id=$1 AND i.owner_id=$2 AND i.location_id=$3 AND i.flag_id=$4 AND i.type_id=$5 AND i.singleton AND i.quantity=1 AND i.jump_clone_id IS NULL", &[&item.item_id, &item.owner_id, &item.location_id, &item.flag_id, &item.type_id]).await?.as_ref().map(|row| Timestamp::from_row(row).dirt_timestamp))
}

/// The caller holds the authorized hull lock until commit.
pub(crate) async fn reset(ex: &(impl Executor + ?Sized), ship: i64, now: i64) -> Result<i64, db::Error> {
    Ok(Timestamp::from_row(&ex.query_one("INSERT INTO game.ship_dirt (ship_item_id,dirt_timestamp) VALUES ($1,$2) ON CONFLICT (ship_item_id) DO UPDATE SET dirt_timestamp=EXCLUDED.dirt_timestamp RETURNING dirt_timestamp", &[&ship, &now]).await?).dirt_timestamp)
}
