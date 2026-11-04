//! Durable receipts for direct-list crate openings.
//! The owning service supplies authorization, transactions and notifications.

use crate::db::{self, executor::Executor};

#[derive(Clone, Copy)]
pub(crate) struct RewardReceipt {
    pub item_id: i64,
    pub character_id: i64,
    pub crate_type_id: i32,
    pub destination_location: i64,
    pub destination_flag: i32,
    pub opened_at: i64,
}


pub(crate) async fn insert(ex: &(impl Executor + ?Sized), receipt: RewardReceipt) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.crate_rewards (item_id, character_id, crate_type_id, destination_location, destination_flag, opened_at) VALUES ($1,$2,$3,$4,$5,$6)",
        &[&receipt.item_id, &receipt.character_id, &receipt.crate_type_id, &receipt.destination_location, &receipt.destination_flag, &receipt.opened_at]).await?;
    Ok(())
}

pub(crate) async fn cargo_capacity(
    ex: &(impl Executor + ?Sized),
    ship_id: i64,
    cargo_location: i64,
    cargo_flag: i32,
) -> Result<Option<(f64, f64)>, db::Error> {
    let row = ex.query_opt(
        "SELECT COALESCE((SELECT SUM(i.quantity * t.volume) FROM game.items i JOIN sde.inv_types t ON t.type_id=i.type_id WHERE i.location_id=$2 AND i.flag_id=$3),0)::float8 AS used, COALESCE(ship_t.capacity, 0)::float8 AS capacity
         FROM game.items ship JOIN sde.inv_types ship_t ON ship_t.type_id=ship.type_id WHERE ship.item_id=$1 FOR UPDATE",
        &[&ship_id, &cargo_location, &cargo_flag],
    ).await?;
    Ok(row.map(|row| (row.get("used"), row.get("capacity"))))
}

