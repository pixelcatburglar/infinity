//! Guarded persisted injector-credit overrides in game.item_attributes.
//! Equipped booster selection and credit policy belong to the caller.
use crate::db::{executor::Executor, Error};

pub(crate) async fn spend_credit(ex: &(impl Executor + ?Sized), item: i64, attribute: i32, expected: Option<f64>, remaining: f64) -> Result<bool, Error> {
    match expected {
        Some(value) => Ok(ex.execute("UPDATE game.item_attributes SET value = $4, cargo_unlimited = false WHERE item_id = $1 AND attribute_id = $2 AND value = $3", &[&item, &attribute, &value, &remaining]).await? == 1),
        None => Ok(ex.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value, cargo_unlimited) VALUES ($1, $2, $3, false) ON CONFLICT (item_id, attribute_id) DO NOTHING", &[&item, &attribute, &remaining]).await? == 1),
    }
}
