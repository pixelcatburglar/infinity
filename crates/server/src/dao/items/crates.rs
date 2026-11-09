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

/// A receipt acknowledgement is deliberately independent of the reward row's lifetime.
/// Opened rewards may subsequently be consumed or merged, but their receipt remains
/// the durable authority for idempotent ClaimLoot acknowledgements.
pub(crate) async fn acknowledge(
    ex: &(impl Executor + ?Sized),
    item_id: i64,
    character_id: i64,
    destination_location: i64,
    destination_flag: i32,
    acknowledged_at: i64,
) -> Result<bool, db::Error> {
    Ok(ex.query_opt(
        "UPDATE game.crate_rewards SET acknowledged_at = COALESCE(acknowledged_at, $5)
         WHERE item_id = $1 AND character_id = $2 AND destination_location = $3 AND destination_flag = $4
         RETURNING item_id",
        &[&item_id, &character_id, &destination_location, &destination_flag, &acknowledged_at],
    ).await?.is_some())
}


