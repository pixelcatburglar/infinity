//! Immutable retail price snapshot loaded once at startup.
//! This server-startup dataset has no direct client mirror.
//! Source: `game.retail_prices` via `dao::sde::prices`, read after the captured-price
//! upsert and market reprice complete; the upsert never deletes rows, so every stored
//! row is kept, including types absent from the SDE.
//! Consumers include exploration and combat loot sampling, killmail valuation,
//! insurance value, industry estimates, mining, the NPC store catalogue and
//! contract escrow display. There is no dynamic pricing: lookup, iteration and
//! range filters read only this snapshot, never the table, while the server runs.

use std::collections::HashMap;
use crate::{dao, db::Db};

/// Server loot policy: NPC wreck and exploration container loot is drawn from types
/// whose average price is at least this many ISK.
pub(crate) const LOOT_MINIMUM_AVERAGE: f64 = 1000.0;

/// PostgreSQL orders NaN above all non-NaN values, including zero.
pub(crate) fn positive_price(value: f64) -> bool { value > 0.0 || value.is_nan() }

pub(crate) struct Price {
    pub(crate) type_id: i32,
    pub(crate) average: f64,
    pub(crate) adjusted: f64,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct Prices {
    rows: Vec<Price>,
    by_id: HashMap<i32, usize>,
}

impl Prices {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        Ok(Self::from_rows(dao::sde::prices::load(db).await?))
    }

    fn from_rows(rows: Vec<Price>) -> Self {
        let mut by_id = HashMap::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() { by_id.insert(row.type_id, index); }
        Self { rows, by_id }
    }

    pub(crate) fn get(&self, type_id: i32) -> Option<&Price> {
        self.by_id.get(&type_id).map(|&index| &self.rows[index])
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Price> { self.rows.iter() }

    /// Both callers pass the finite [`LOOT_MINIMUM_AVERAGE`] lower bound; the upper
    /// bound is finite for exploration loot, while the raw static NPC bounty can be
    /// NaN, which PostgreSQL orders above every non-NaN value.
    pub(crate) fn average_between(&self, minimum: f64, maximum: f64) -> impl Iterator<Item = &Price> {
        self.iter().filter(move |row| {
            if maximum.is_nan() { row.average >= minimum || row.average.is_nan() }
            else { row.average >= minimum && row.average <= maximum }
        })
    }
}

#[cfg(all(test, feature = "tests-sde"))] mod tests;
