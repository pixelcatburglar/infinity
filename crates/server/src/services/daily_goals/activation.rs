//! Period-scoped daily content selection from the retained publication history.
//! The client daily periods use UTC boundaries supplied by the period DAO.
//! Retained content is not a gateway admission; pins live only for one operation.
use std::{collections::HashMap, sync::Arc};
use crate::{dao::lua_content::activations::ActivationRow, lua::content::PinnedPolicy};
use super::{Error, Resources, DAILY_GOALS_NAMESPACE};

/// An operation-scoped snapshot of the retained activation history.
pub(crate) struct ActivationHistory { rows: Arc<Vec<ActivationRow>>, pins: HashMap<String, PinnedPolicy> }

pub(crate) fn load(resources: &Resources) -> Result<ActivationHistory, Error> {
    Ok(ActivationHistory { rows: resources.policies.activations(DAILY_GOALS_NAMESPACE)?, pins: HashMap::new() })
}

/// Select by publication ordinal; callers provide normalized UTC period boundaries.
pub(crate) fn select(rows: &[ActivationRow], start: i64, end: i64) -> Result<Option<&ActivationRow>, Error> {
    if start >= end { return Err(Error::Invalid); }
    let mut selected = None;
    let mut previous = 0;
    for (index, row) in rows.iter().enumerate() {
        if row.ordinal <= previous || row.published_at <= 0 || row.namespace != DAILY_GOALS_NAMESPACE || row.version.is_empty() || row.fingerprint.len() != 32 {
            return Err(anyhow::anyhow!("malformed daily content activation log").into());
        }
        previous = row.ordinal;
        if row.published_at < if index == 0 { end } else { start } { selected = Some(row); }
    }
    Ok(selected)
}

/// Proof that this exact retained package was selected for one period.
pub(crate) struct PeriodPolicy { kind: String, start: i64, end: i64, policy: PinnedPolicy }
impl PeriodPolicy {
    pub(crate) fn kind(&self) -> &str { &self.kind }
    pub(crate) fn start(&self) -> i64 { self.start }
    pub(crate) fn end(&self) -> i64 { self.end }
    pub(crate) fn policy(&self) -> &PinnedPolicy { &self.policy }
}

pub(crate) fn resolve(resources: &Resources, history: &mut ActivationHistory, kind: &str, start: i64, end: i64) -> Result<Option<PeriodPolicy>, Error> {
    if kind != "day" && kind != "month" { return Err(Error::Invalid); }
    let Some(row) = select(history.rows.as_slice(), start, end)? else { return Ok(None); };
    let (policy, newly_resolved) = match history.pins.get(&row.version) {
        Some(policy) => (policy.clone(), false),
        None => (resources.policies.resolve(DAILY_GOALS_NAMESPACE, &row.version)?, true),
    };
    if policy.namespace() != row.namespace || policy.version() != row.version || policy.fingerprint().as_slice() != row.fingerprint.as_slice() || policy.daily_goals().is_none() {
        return Err(anyhow::anyhow!("retained daily content does not match its activation").into());
    }
    if newly_resolved { history.pins.insert(row.version.clone(), policy.clone()); }
    Ok(Some(PeriodPolicy { kind: kind.to_owned(), start, end, policy }))
}

#[cfg(all(test, feature = "tests-content"))] mod tests;
