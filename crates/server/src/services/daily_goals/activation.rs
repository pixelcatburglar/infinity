//! Period-scoped daily content selection from the durable publication log.
//! The client daily periods use UTC boundaries supplied by the period DAO.
//! Retained content is not a gateway admission; pins live only for one operation.
use std::collections::HashMap;
use crate::{dao::lua_content::activations::{self, ActivationRow}, lua::content::PinnedPolicy};
use super::{Error, Resources, DAILY_GOALS_NAMESPACE};

/// An operation-scoped snapshot read from the authoritative publication log.
pub(crate) struct ActivationHistory { rows: Vec<ActivationRow>, pins: HashMap<String, PinnedPolicy> }

pub(crate) async fn load(resources: &Resources) -> Result<ActivationHistory, Error> {
    Ok(ActivationHistory { rows: activations::list(&resources.db, DAILY_GOALS_NAMESPACE).await?, pins: HashMap::new() })
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

pub(crate) async fn resolve(resources: &Resources, history: &mut ActivationHistory, kind: &str, start: i64, end: i64) -> Result<Option<PeriodPolicy>, Error> {
    if kind != "day" && kind != "month" { return Err(Error::Invalid); }
    let Some(row) = select(&history.rows, start, end)? else { return Ok(None); };
    let (policy, newly_resolved) = match history.pins.get(&row.version) {
        Some(policy) => (policy.clone(), false),
        None => (resources.policies.resolve(DAILY_GOALS_NAMESPACE, &row.version).await?, true),
    };
    if policy.namespace() != row.namespace || policy.version() != row.version || policy.fingerprint().as_slice() != row.fingerprint.as_slice() || policy.daily_goals().is_none() {
        return Err(anyhow::anyhow!("retained daily content does not match its activation").into());
    }
    if newly_resolved { history.pins.insert(row.version.clone(), policy.clone()); }
    Ok(Some(PeriodPolicy { kind: kind.to_owned(), start, end, policy }))
}

#[cfg(test)] mod tests;
