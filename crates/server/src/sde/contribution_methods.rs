//! Immutable native contribution-method metadata for goal configuration.
//! Client readers include `goals/client/contributionMethods.py`.
//! Reads `sde_native.contributionmethods` once; goal progress is not implemented here.
use std::collections::HashMap;
use anyhow::{ensure, Context};
use serde::Deserialize;
use crate::{dao::sde::contribution_methods as dao, db::Db};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Method {
    #[serde(rename = "_key")]
    pub(crate) key: String,
    #[serde(rename = "titleID")]
    pub(crate) title_id: i64,
    #[serde(rename = "descriptionID")]
    pub(crate) description_id: i64,
    #[serde(rename = "progressDescriptionID")]
    pub(crate) progress_description_id: Option<i64>,
    #[serde(rename = "rewardingDescriptionID")]
    pub(crate) rewarding_description_id: Option<i64>,
    #[serde(rename = "targetValueDescriptionID")]
    pub(crate) target_value_description_id: Option<i64>,
    pub(crate) unit: Option<i64>,
    #[serde(default)]
    pub(crate) parameters: Vec<Parameter>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Parameter {
    pub(crate) parameter_key: String,
    pub(crate) parameter_type: String,
    pub(crate) is_required: u8,
    pub(crate) max_entries: Option<u32>,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct ContributionMethods { methods: HashMap<String, Method> }

impl ContributionMethods {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        Self::from_rows(dao::load(db).await?)
    }
    pub(crate) fn get(&self, key: &str) -> Option<&Method> { self.methods.get(key) }

    fn from_rows(rows: Vec<dao::Row>) -> anyhow::Result<Self> {
        let mut methods = HashMap::with_capacity(rows.len());
        for row in rows {
            let key = row.key.context("contribution method key missing")?;
            let method: Method = serde_json::from_str(&row.data).context("invalid contribution method metadata")?;
            ensure!(!key.is_empty() && key == method.key, "contribution method key mismatch");
            ensure!(method.title_id > 0 && method.description_id > 0, "invalid contribution method localization reference");
            ensure!([method.progress_description_id, method.rewarding_description_id, method.target_value_description_id, method.unit].into_iter().flatten().all(|id| id > 0), "invalid contribution method metadata reference");
            ensure!(method.parameters.iter().all(|p| !p.parameter_key.is_empty() && !p.parameter_type.is_empty() && p.is_required <= 1 && p.max_entries.is_none_or(|n| n > 0)), "invalid contribution method parameter");
            ensure!(methods.insert(key, method).is_none(), "duplicate contribution method");
        }
        Ok(Self { methods })
    }
}

#[cfg(test)] mod tests;
