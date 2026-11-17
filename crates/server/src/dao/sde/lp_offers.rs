//! Startup projection of the imported LP offer table.
//! This server-startup DAO has no direct client mirror.
//! Source: `game.lp_offers`, read once during `Sde::load`; every stored row is
//! projected, including corporations and types absent from the SDE, ordered by
//! (corporation_id, offer_id) for the grouping consumer in `sde::lp_offers`.
//! Runtime queries are unsupported; consumers read the immutable snapshot.
//! `required_items` passes through as text; decoding is the consumer's concern.

use crate::db::{executor::Executor, Error};

pub(crate) struct OfferRow {
    pub corporation_id: i64,
    pub offer_id: i64,
    pub type_id: i32,
    pub quantity: i32,
    pub lp_cost: i64,
    pub isk_cost: i64,
    pub ak_cost: i64,
    pub required_items: String,
}

impl OfferRow {
    fn from_row(row: &tokio_postgres::Row) -> Result<Self, Error> {
        Ok(Self {
            corporation_id: row.try_get("corporation_id")?,
            offer_id: row.try_get("offer_id")?,
            type_id: row.try_get("type_id")?,
            quantity: row.try_get("quantity")?,
            lp_cost: row.try_get("lp_cost")?,
            isk_cost: row.try_get("isk_cost")?,
            ak_cost: row.try_get("ak_cost")?,
            required_items: row.try_get("required_items")?,
        })
    }
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<OfferRow>, Error> {
    ex.query("SELECT corporation_id, offer_id, type_id, quantity, lp_cost, isk_cost, ak_cost, required_items::text AS required_items FROM game.lp_offers ORDER BY corporation_id, offer_id", &[]).await?.iter().map(OfferRow::from_row).collect()
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
