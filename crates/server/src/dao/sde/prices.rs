//! Startup projection of the post-upsert retail price table.
//! This server-startup DAO has no direct client mirror.
//! Source: `game.retail_prices`, read once after the captured-price upsert and
//! market reprice complete; every stored row is projected, including types
//! absent from the SDE, since the startup upsert never deletes rows.
//! Runtime per-type queries and dynamic repricing are unsupported; consumers
//! read the immutable snapshot in `sde::prices`.

use crate::{db::{executor::Executor, Error}, sde::prices::Price};

impl Price {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { type_id: row.get("type_id"), average: row.get("average"), adjusted: row.get("adjusted") }
    }
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<Price>, Error> {
    Ok(ex.query("SELECT type_id, average, adjusted FROM game.retail_prices", &[]).await?.iter().map(Price::from_row).collect())
}
