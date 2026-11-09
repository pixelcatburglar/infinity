//! Typed adaptation catalogue projected from one retained publisher bundle.
//! Inventory and NPC corporation references use the server SDE. Node task and
//! effect semantics remain unavailable; layout occurrences are not deduplicated.

use std::collections::{BTreeMap, BTreeSet};
use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use crate::sde::Sde;
use super::bundle::Bundle;

/// Root layout occurrences use -1 for the absent parent in decoded tree data.
const ROOT_PARENT: i64 = -1;

/// Keys of the generation-pinned player-visible refusal catalogue.
pub(crate) const INVALID: &str = "invalid";
pub(crate) const CONFLICT: &str = "conflict";
pub(crate) const ITEM: &str = "item";
pub(crate) const CHIPS: &str = "chips";
pub(crate) const OVERFLOW: &str = "overflow";
pub(crate) const REQUIRED_MESSAGES: &[&str] = &[INVALID, CONFLICT, ITEM, CHIPS, OVERFLOW];

#[derive(Deserialize)]
struct Node {
    node_type: i32, unlock_type: i32,
    #[serde(default)] target_ship_types: Vec<i32>,
    unlock_skill_id: Option<i32>,
    target_required_skill_types: Option<Value>,
}

#[derive(Clone, Copy)]
enum NodeTier { Root, T1, T2, Ship, T3 }
#[derive(Deserialize)]
struct NodeType { name: String }
#[derive(Deserialize)]
struct Layout { id: i64, node_id: i64, parent_id: i64 }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cost { amount: u32, item_type_id: i32 }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tier {
    #[serde(default)] base_unlock_chips: Vec<Cost>,
    #[serde(default)] base_unlock_items: Vec<Cost>,
    consume_multiplier: Vec<f64>,
}
#[derive(Deserialize)]
struct Tree {
    tree_nodes: Vec<Layout>,
    t1_node_consume: Tier, t2_node_consume: Tier, t3_node_consume: Tier,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration { factions: Vec<Faction> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Faction { faction_id: i32, ship_groups: Vec<ShipGroup> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShipGroup { group_id: i32, tree_id: i64 }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Chip { _key: i32, #[serde(rename = "defaultCorpBonus")] default: Bonus, #[serde(rename = "corpsBonus")] corporations: BTreeMap<String, Bonus> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bonus { #[serde(rename = "lpBonus")] lp: i64 }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exchange { #[serde(rename = "availableCorpIDs")] corporations: Vec<i64> }

#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Quote { default: i64, overrides: BTreeMap<i64, i64> }
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct Definitions {
    chip_types: Vec<i32>, trees: BTreeSet<u32>, quotes: BTreeMap<i32, Quote>,
    issuers: BTreeSet<i64>, messages: BTreeMap<String, String>,
}
impl Definitions {
    pub(crate) fn chip_types(&self) -> &[i32] { &self.chip_types }
    pub(crate) fn published_tree(&self, id: u32) -> bool { self.trees.contains(&id) }
    pub(crate) fn issuer(&self, id: i64) -> bool { self.issuers.contains(&id) }
    pub(crate) fn quote(&self, chip: i32, corporation: i64) -> Option<i64> {
        self.quotes.get(&chip).map(|q| q.overrides.get(&corporation).copied().unwrap_or(q.default))
    }
    pub(crate) fn quoted_count(&self) -> usize { self.quotes.len() }
    pub(crate) fn message(&self, key: &str) -> Option<&str> { self.messages.get(key).map(String::as_str) }
    pub(super) fn compatible(&self, previous: &Self) -> bool {
        previous.chip_types.iter().all(|id| self.chip_types.binary_search(id).is_ok())
    }
}
fn item(sde: &Sde, id: i32) -> Result<()> {
    ensure!(id > 0 && sde.types.get(id).is_some(), "unknown inventory type {id}");
    Ok(())
}
fn parse<T: for<'de> Deserialize<'de>>(value: &Value) -> Result<T> { Ok(T::deserialize(value)?) }
fn tier(sde: &Sde, tier: Tier, chip_types: &[i32], reachable: bool) -> Result<()> {
    ensure!(!reachable || !tier.consume_multiplier.is_empty(), "empty reachable tier multipliers");
    for multiplier in &tier.consume_multiplier { ensure!(multiplier.is_finite() && *multiplier >= 0.0, "invalid tier multiplier"); }
    for cost in &tier.base_unlock_chips {
        ensure!(chip_types.binary_search(&cost.item_type_id).is_ok(), "unlock chip is not a supported currency");
    }
    for cost in tier.base_unlock_chips.iter().chain(&tier.base_unlock_items) {
        item(sde, cost.item_type_id)?;
        for multiplier in &tier.consume_multiplier {
            let value = f64::from(cost.amount) * multiplier;
            ensure!(value.is_finite() && value <= f64::from(u32::MAX), "tier cost exceeds range");
        }
    }
    Ok(())
}
pub(super) fn build(bundle: &Bundle, sde: &Sde, messages: BTreeMap<String, String>) -> Result<Definitions> {
    ensure!(REQUIRED_MESSAGES.iter().all(|key| messages.get(*key).is_some_and(|value| !value.is_empty())), "missing adaptation refusal text");
    let chip_types = bundle.chip_types().to_vec();
    for &id in &chip_types {
        item(sde, id)?;
    }
    let mut node_types = BTreeMap::new();
    for row in bundle.records("shipadaptionnodetypes")? {
        let id: i32 = row.key.parse()?;
        let kind: NodeType = parse(&row.data)?;
        let kind = match kind.name.as_str() {
            "Root" => NodeTier::Root, "T1" => NodeTier::T1, "T2" => NodeTier::T2,
            "Ship" => NodeTier::Ship, "T3" => NodeTier::T3,
            _ => bail!("unsupported node type"),
        };
        ensure!(node_types.insert(id, kind).is_none(), "duplicate node type");
    }
    let unlock_types: BTreeSet<i32> = bundle.records("shipadaptionnodeunlocktypes")?.iter().map(|r| r.key.parse()).collect::<std::result::Result<_, _>>()?;
    let mut nodes = BTreeMap::new();
    for row in bundle.records("shipadaptionnodes")? {
        let id: u32 = row.key.parse()?;
        let node: Node = parse(&row.data)?;
        let kind = *node_types.get(&node.node_type).context("unknown node type")?;
        ensure!(unlock_types.contains(&node.unlock_type), "unknown node enum");
        ensure!(node.target_required_skill_types.is_none(), "unsupported target required skill types");
        if let Some(skill) = node.unlock_skill_id {
            item(sde, skill)?;
            ensure!(sde.types.get(skill).and_then(|ty| ty.category_id) == Some(infinity_core::consts::CATEGORY_SKILL), "unlock type is not a skill");
        }
        for target in node.target_ship_types {
            item(sde, target)?;
            ensure!(sde.types.get(target).and_then(|ty| ty.category_id) == Some(infinity_core::consts::CATEGORY_SHIP), "target type is not a ship");
        }
        nodes.insert(id, kind);
    }
    let mut available_trees = BTreeSet::new();
    for row in bundle.records("shipadaptiontrees")? {
        let id: u32 = row.key.parse()?;
        let tree: Tree = parse(&row.data)?;
        let labels: BTreeSet<i64> = tree.tree_nodes.iter().map(|layout| layout.id).collect();
        let (mut t1, mut t2, mut t3) = (false, false, false);
        for layout in tree.tree_nodes {
            let node_id = u32::try_from(layout.node_id)?;
            ensure!(u32::try_from(layout.id).is_ok() && (layout.parent_id == ROOT_PARENT || (u32::try_from(layout.parent_id).is_ok() && labels.contains(&layout.parent_id))), "invalid tree layout");
            match nodes.get(&node_id).context("unknown tree node")? {
                NodeTier::T1 => t1 = true,
                NodeTier::T2 => t2 = true,
                NodeTier::T3 => t3 = true,
                NodeTier::Root | NodeTier::Ship => (),
            }
        }
        tier(sde, tree.t1_node_consume, &chip_types, t1)?;
        tier(sde, tree.t2_node_consume, &chip_types, t2)?;
        tier(sde, tree.t3_node_consume, &chip_types, t3)?;
        available_trees.insert(id);
    }
    let config: Configuration = parse(&bundle.records("shipadaptionconfig")?.first().context("missing config")?.data)?;
    let mut trees = BTreeSet::new();
    for faction in config.factions {
        ensure!(faction.faction_id > 0, "invalid faction");
        for group in faction.ship_groups {
            let tree = u32::try_from(group.tree_id)?;
            ensure!(group.group_id > 0 && available_trees.contains(&tree), "unknown published tree");
            trees.insert(tree);
        }
    }
    let exchange: Exchange = parse(&bundle.records("shipadaptionchipexchangeconfig")?.first().context("missing exchange config")?.data)?;
    let mut issuers = BTreeSet::new();
    for id in exchange.corporations {
        ensure!(id > 0 && sde.npc_corporations.get(id).is_some() && issuers.insert(id), "invalid issuer");
    }
    let mut quotes = BTreeMap::new();
    for row in bundle.records("shipadaptionchips")? {
        let id: i32 = row.key.parse()?;
        ensure!(chip_types.binary_search(&id).is_ok(), "quoted type is not a supported currency");
        let chip: Chip = parse(&row.data)?;
        ensure!(chip._key == id && chip.default.lp >= 0, "invalid LP quote");
        let mut overrides = BTreeMap::new();
        for (key, bonus) in chip.corporations {
            let corp: i64 = key.parse()?;
            ensure!(corp > 0 && key == corp.to_string() && sde.npc_corporations.get(corp).is_some() && bonus.lp >= 0, "invalid corporation quote");
            if overrides.insert(corp, bonus.lp).is_some() { bail!("duplicate corporation quote"); }
        }
        quotes.insert(id, Quote { default: chip.default.lp, overrides });
    }
    Ok(Definitions { chip_types, trees, quotes, issuers, messages })
}

#[cfg(test)]
mod tests;
