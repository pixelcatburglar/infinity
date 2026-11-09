//! Server-authored historical retail policy for expert-system consumables in
//! `content/expert_system_store.json`, used by `vgs/client/vgsClient.py` and
//! `vgs/common/utils.py`. Entries are validated against typed SDE inventory
//! types and expert definitions. The current official catalogue and direct
//! activation are not supported by this policy.
use std::collections::HashSet;
use anyhow::{bail, Result};
use serde::Deserialize;

/// Server-owned retail-policy schema version.
const POLICY_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExpertOffer { pub(crate) type_id: i32, pub(crate) quantity: i32, pub(crate) plex_price: i64 }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy { version: u32, offers: Vec<Entry> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry { type_id: i32, quantity: i32, plex_price: i64, source_url: String, observed_on: String, price_basis: String }

pub(super) fn parse(content: &str, valid_type: impl Fn(i32) -> bool, valid_expert: impl Fn(i32) -> bool) -> Result<Vec<ExpertOffer>> {
    let policy: Policy = serde_json::from_str(content)?;
    if policy.version != POLICY_VERSION { bail!("unknown expert store policy version") }
    let mut seen = HashSet::new();
    let mut offers = Vec::with_capacity(policy.offers.len());
    for entry in policy.offers {
        if entry.type_id <= 0 || entry.quantity <= 0 || entry.plex_price <= 0 { bail!("invalid expert store offer") }
        if !seen.insert(entry.type_id) { bail!("duplicate expert store type {}", entry.type_id) }
        if !valid_type(entry.type_id) || !valid_expert(entry.type_id) { bail!("unknown expert store type {}", entry.type_id) }
        if entry.source_url.trim().is_empty() || entry.observed_on.trim().is_empty() || entry.price_basis != "observed historical retail price" { bail!("invalid expert store provenance") }
        offers.push(ExpertOffer { type_id: entry.type_id, quantity: entry.quantity, plex_price: entry.plex_price });
    }
    Ok(offers)
}

#[cfg(test)]
mod tests;
