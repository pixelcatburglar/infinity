//! Immutable LP store offer catalogue loaded once at startup.
//! This server-startup dataset has no direct client mirror; the client LP store
//! reaches it through the LPStoreMgr service.
//! Source: `game.lp_offers` via `dao::sde::lp_offers`, read once during
//! `Sde::load`. Operators supply the catalogue; importing or adjusting offers
//! takes effect on restart. Every stored row is kept, including corporations and
//! types absent from the SDE, and a malformed `required_items` payload fails
//! startup rather than omitting the offer. While the catalogue is globally empty
//! the caller synthesizes retail-derived fallback offers; a corporation absent
//! from a nonempty catalogue simply has no offers.

use std::collections::HashMap;
use anyhow::Context;
use serde::Deserialize;
use crate::db::executor::Executor;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct RequiredItem {
    pub type_id: i32,
    pub quantity: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Offer {
    pub corporation_id: i64,
    pub offer_id: i64,
    pub type_id: i32,
    pub quantity: i32,
    pub lp_cost: i64,
    pub isk_cost: i64,
    pub ak_cost: i64,
    pub required_items: Vec<RequiredItem>,
}

/// Offers grouped by issuing corporation, each group in ascending `offer_id`
/// order as delivered by the loader's ordered rows; every offer exists exactly
/// once and is only ever borrowed.
#[cfg_attr(test, derive(Default))]
pub(crate) struct LpOffers {
    by_corporation: HashMap<i64, Vec<Offer>>,
}

impl LpOffers {
    pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> anyhow::Result<Self> {
        Self::from_rows(crate::dao::sde::lp_offers::load(ex).await?)
    }

    // `required_items` is decoded exactly once here; a malformed payload fails
    // the whole load so a partial catalogue is never published, and the
    // offending offer is named for the operator. Arrays retain order and
    // duplicates; extra JSON fields are ignored.
    fn from_rows(rows: Vec<crate::dao::sde::lp_offers::OfferRow>) -> anyhow::Result<Self> {
        let mut by_corporation = HashMap::<i64, Vec<Offer>>::new();
        for row in rows {
            let required_items: Vec<RequiredItem> = serde_json::from_str(&row.required_items)
                .with_context(|| format!("LP offer corporation_id={} offer_id={}: invalid required_items", row.corporation_id, row.offer_id))?;
            anyhow::ensure!(!required_items.iter().any(|item| item.type_id <= 0 || item.quantity <= 0),
                "LP offer corporation_id={} offer_id={}: invalid required_items: LP offer requirement type and quantity must be positive",
                row.corporation_id, row.offer_id);
            by_corporation.entry(row.corporation_id).or_default().push(Offer {
                corporation_id: row.corporation_id,
                offer_id: row.offer_id,
                type_id: row.type_id,
                quantity: row.quantity,
                lp_cost: row.lp_cost,
                isk_cost: row.isk_cost,
                ak_cost: row.ak_cost,
                required_items,
            });
        }
        Ok(Self { by_corporation })
    }

    /// A corporation absent from the catalogue has an empty offer list.
    pub(crate) fn list(&self, corporation_id: i64) -> &[Offer] {
        self.by_corporation.get(&corporation_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Groups are stored in ascending `offer_id` order from the ordered loader
    /// query, so lookup is a binary search over the borrowed slice.
    pub(crate) fn get(&self, corporation_id: i64, offer_id: i64) -> Option<&Offer> {
        let offers = self.list(corporation_id);
        offers.binary_search_by_key(&offer_id, |offer| offer.offer_id).ok().map(|index| &offers[index])
    }

    /// Whether the whole catalogue has no offers; only a globally empty
    /// catalogue activates the caller's synthetic fallback.
    pub(crate) fn is_empty(&self) -> bool {
        self.by_corporation.is_empty()
    }
}

#[cfg(all(test, feature = "tests-sde"))]
mod tests;
