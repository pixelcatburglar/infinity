//! Guarded item quantity updates and deletes in `game.items`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Planning, transactions, refusals, notifications and World are caller concerns; other item mutations are not handled here.

use std::collections::BTreeMap;
use crate::db::{self, executor::Executor};
use super::loading::Item;

#[derive(Clone, Copy)]
pub(crate) struct QuantityChange {
    pub expected_quantity: i32,
    pub quantity: Option<i32>,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i32,
    pub singleton: bool,
}

impl QuantityChange {
    pub(crate) fn new(item: &Item, quantity: Option<i32>) -> Self {
        Self { expected_quantity: item.quantity, quantity, owner_id: item.owner_id, location_id: item.location_id, flag_id: item.flag_id, singleton: item.singleton }
    }
}

pub(crate) async fn apply(db: &(impl Executor + ?Sized), changes: &BTreeMap<i64, QuantityChange>) -> Result<bool, db::Error> {
    for (&id, change) in changes {
        let affected = match change.quantity {
            Some(quantity) => db.execute(
                "UPDATE game.items SET quantity = $7 WHERE item_id = $1 AND quantity = $2 AND owner_id = $3 AND location_id = $4 AND flag_id = $5 AND singleton = $6 AND jump_clone_id IS NULL",
                &[&id, &change.expected_quantity, &change.owner_id, &change.location_id, &change.flag_id, &change.singleton, &quantity],
            ).await,
            None => db.execute(
                "DELETE FROM game.items WHERE item_id = $1 AND quantity = $2 AND owner_id = $3 AND location_id = $4 AND flag_id = $5 AND singleton = $6 AND jump_clone_id IS NULL",
                &[&id, &change.expected_quantity, &change.owner_id, &change.location_id, &change.flag_id, &change.singleton],
            ).await,
        }?;
        if affected == 0 { return Ok(false); }
    }
    Ok(true)
}
