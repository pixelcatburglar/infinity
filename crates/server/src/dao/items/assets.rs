//! Batched owner inventory and ancestry facts for corporation assets and blueprints.
//! Reads `game.items` and retained `game.offices` through the existing item DAOs;
//! policy, static-data enrichment and wire projections belong to services.
use crate::db::{executor::Executor, Error};
use super::{deliveries::{self, LineageRow}, loading::{self, GameItem, ItemQuery}};

pub(crate) struct Facts {
    pub items: Vec<GameItem>,
    pub ancestry: Vec<LineageRow>,
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Facts, Error> {
    let items = loading::load_items(ex, ItemQuery::Owner(owner)).await?;
    let ids: Vec<_> = items.iter().map(|item| item.item_id).collect();
    let ancestry = deliveries::ancestry_many(ex, &ids).await?;
    Ok(Facts { items, ancestry })
}

/// Batch only the ancestry of the requested item identities.
pub(crate) async fn ancestry(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<LineageRow>, Error> {
    deliveries::ancestry_many(ex, ids).await
}
