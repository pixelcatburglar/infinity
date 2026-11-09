//! Pure authored capital plans and guarded persistence for sovereignty transitions.
//! Callers prepare before locks, then lock policy before systems and revalidate the
//! complete alliance-owned set. Ordinary capital changes are unsupported.
use crate::{dao::sovereignty::{policy::{self, PolicyRow}, systems::SystemRow}, db::executor::Executor, lua::content::{PinnedPolicy, PolicyInput, PolicyPlan, Rule}, services::{db_err, CallError}};

fn input(policy: &PolicyRow, owned: &[SystemRow], system: i64, now: i64) -> PolicyInput {
    PolicyInput { now, system, current_system: policy.current_capital_system,
        first_appointment: policy.last_capital_changed_at, last_loss: policy.capital_lost_at,
        not_before: policy.capital_change_not_before, owned_count: owned.len() as u64 }
}

/// `owned` includes the planned unique claim for initialization.
pub(crate) fn prepare_initial(pin: &PinnedPolicy, expected: &PolicyRow, owned: &[SystemRow], system: i64, now: i64) -> Result<PolicyPlan, CallError> {
    pin.evaluate(Rule::InitializeCapital, &input(expected, owned, system, now))
}

pub(crate) fn prepare_loss(pin: &PinnedPolicy, expected: &PolicyRow, owned: &[SystemRow], system: i64, now: i64) -> Result<PolicyPlan, CallError> {
    pin.evaluate(Rule::CapitalLost, &input(expected, owned, system, now))
}

/// The caller holds the policy lock and revalidated all system dependencies.
pub(crate) async fn apply(ex: &(impl Executor + ?Sized), expected: &PolicyRow, plan: &PolicyPlan, now: i64) -> Result<PolicyRow, CallError> {
    let mut row = expected.clone();
    match plan {
        PolicyPlan::Unchanged => return Ok(row),
        PolicyPlan::Capital { current_system, not_before } => {
            row.current_capital_system = Some(*current_system);
            row.last_capital_changed_at = Some(now);
            row.capital_change_not_before = Some(*not_before);
        }
        PolicyPlan::CapitalLost { not_before } => {
            row.current_capital_system = None;
            row.capital_lost_at = Some(now);
            row.capital_change_not_before = Some(*not_before);
        }
        PolicyPlan::SelfDestruct { .. } => return Err(db_err(anyhow::anyhow!("invalid capital plan"))),
    }
    if !policy::replace(ex, expected.revision, &row).await.map_err(db_err)? {
        return Err(db_err(anyhow::anyhow!("stale capital policy")));
    }
    row.revision += 1;
    Ok(row)
}
