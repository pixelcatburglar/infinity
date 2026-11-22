//! Immutable command-burst modifier collections loaded once at startup.
//! Mirrors the client's `dbuff/collections` storage (`dbuff/storage.py`) and
//! the `dbuff/common/registry.py` modifier applier: each collection carries one
//! shared dogma operation, an aggregate mode that folds competing sources, and
//! four real selector arrays (item, location, location-group and
//! location-required-skill) applied to the receiving item anchor. The raw rows
//! have no domain field; compiled rows use the `itemID` domain so the existing
//! `modifier_applies` selector resolves the receiver anchor through `source`.
//! Rows come from `sde_native.dbuffcollections`; source application, expiry and
//! per-collection aggregation live in the space and fit layers, and display or
//! developer metadata is not stored.
use std::collections::HashMap;
use anyhow::{bail, ensure, Context};
use serde::Deserialize;
use crate::{dao::sde::dbuff as dao, db::Db, services::fit};

/// How the competing source values of one collection fold, from the raw
/// `aggregateMode` (`dbuff/common/aggregators.py`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DbuffAggregate { Maximum, Minimum }

/// One compiled selector row of a collection. The operand is the runtime
/// source's pulse scalar, so `modifier.modifying` stays zero.
pub(crate) struct DbuffModifier { pub(crate) modifier: crate::sde::dogma::Modifier }

/// One immutable dbuff collection.
pub(crate) struct DbuffCollection {
    pub(crate) aggregate: DbuffAggregate,
    pub(crate) modifiers: Vec<DbuffModifier>,
}

#[derive(Default)]
pub(crate) struct Dbuffs { collections: HashMap<i32, DbuffCollection> }

impl Dbuffs {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        Self::from_rows(dao::load(db).await?)
    }
    pub(crate) fn get(&self, id: i32) -> Option<&DbuffCollection> { self.collections.get(&id) }

    fn from_rows(rows: Vec<dao::Row>) -> anyhow::Result<Self> {
        let mut collections = HashMap::with_capacity(rows.len());
        for row in rows {
            let key = row.key.context("dbuff collection key missing")?;
            let raw: RawCollection = serde_json::from_str(&row.data).context("invalid dbuff collection")?;
            let id = i32::try_from(raw.key).context("dbuff collection id out of range")?;
            ensure!(id > 0 && key.parse::<i64>() == Ok(raw.key), "dbuff collection key mismatch");
            let aggregate = match raw.aggregate_mode.as_str() {
                "Maximum" => DbuffAggregate::Maximum,
                "Minimum" => DbuffAggregate::Minimum,
                _ => bail!("invalid dbuff collection aggregate mode"),
            };
            let op = operation(&raw.operation_name).context("invalid dbuff collection operation")?;
            let mut modifiers = Vec::new();
            for m in raw.item_modifiers { modifiers.push(selector("ItemModifier", op, m.attribute, None, None)?); }
            for m in raw.location_modifiers { modifiers.push(selector("LocationModifier", op, m.attribute, None, None)?); }
            for m in raw.location_group_modifiers { modifiers.push(selector("LocationGroupModifier", op, m.attribute, Some(m.group), None)?); }
            for m in raw.location_required_skill_modifiers { modifiers.push(selector("LocationRequiredSkillModifier", op, m.attribute, None, Some(m.skill))?); }
            ensure!(collections.insert(id, DbuffCollection { aggregate, modifiers }).is_none(), "duplicate dbuff collection");
        }
        Ok(Self { collections })
    }
}

/// Client `dbuff/storage.py` OPERATION_IDS_BY_NAME mapped onto the shared fit
/// operation constants; any name outside the table is refused.
fn operation(name: &str) -> Option<i32> {
    Some(match name {
        "PreAssignment" => fit::OP_PRE_ASSIGN,
        "PreMul" => fit::OP_PRE_MUL,
        "PreDiv" => fit::OP_PRE_DIV,
        "ModAdd" => fit::OP_MOD_ADD,
        "ModSub" => fit::OP_MOD_SUB,
        "PostMul" => fit::OP_POST_MUL,
        "PostDiv" => fit::OP_POST_DIV,
        "PostPercent" => fit::OP_POST_PERCENT,
        "PostAssignment" => fit::OP_POST_ASSIGN,
        _ => return None,
    })
}

/// `func` is the client dogma modifier class (`dogma/effects/modifiers.py`).
fn selector(func: &'static str, op: i32, attribute: i64, group: Option<i64>, skill: Option<i64>) -> anyhow::Result<DbuffModifier> {
    let modified = i32::try_from(attribute).context("dbuff modifier attribute out of range")?;
    ensure!(modified > 0, "dbuff modifier attribute invalid");
    let group = group.map(i32::try_from).transpose().context("dbuff modifier group out of range")?;
    ensure!(group.is_none_or(|id| id > 0), "dbuff modifier group invalid");
    let skill = skill.map(i32::try_from).transpose().context("dbuff modifier skill out of range")?;
    ensure!(skill.is_none_or(|id| id > 0), "dbuff modifier skill invalid");
    Ok(DbuffModifier { modifier: crate::sde::dogma::Modifier {
        func: func.to_string(), domain: "itemID".to_string(), op, modified,
        modifying: 0, group, skill, target_category: None }})
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCollection {
    #[serde(rename = "_key")]
    key: i64,
    aggregate_mode: String,
    operation_name: String,
    #[serde(default)]
    item_modifiers: Vec<RawAttribute>,
    #[serde(default)]
    location_modifiers: Vec<RawAttribute>,
    #[serde(default)]
    location_group_modifiers: Vec<RawGroupAttribute>,
    #[serde(default)]
    location_required_skill_modifiers: Vec<RawSkillAttribute>,
}
#[derive(Deserialize)]
struct RawAttribute { #[serde(rename = "dogmaAttributeID")] attribute: i64 }
#[derive(Deserialize)]
struct RawGroupAttribute { #[serde(rename = "groupID")] group: i64, #[serde(rename = "dogmaAttributeID")] attribute: i64 }
#[derive(Deserialize)]
struct RawSkillAttribute { #[serde(rename = "skillID")] skill: i64, #[serde(rename = "dogmaAttributeID")] attribute: i64 }

#[cfg(all(test, feature = "tests-sde"))] mod tests;
#[cfg(all(test, any(feature = "tests-space", feature = "tests-lua")))]
#[path = "dbuff/tests/fixture.rs"]
pub(crate) mod fixture;
