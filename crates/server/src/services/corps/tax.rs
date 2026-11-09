//! Tax on actual NPC bounty and mission rewards, settled in the caller's transaction.
//! Other wallet and loyalty mutations do not pass through this producer-specific path.
use infinity_core::consts::ACCOUNT_CASH;
use crate::{dao::corporations::tax as dao, db::{executor::Executor, outbox::Outbox}, services::{db_err, CallError}};

/// Client appConst.minCorporationTaxAmount; server policy taxes only gross rewards strictly above it.
const MIN_CORPORATION_TAX_AMOUNT: f64 = 100_000.0;

pub(crate) struct LoyaltyReward { pub issuer: i64, pub amount: i64, pub mutation: super::super::standings::LoyaltyMutation }

/// Split one actual reward item using current corporation terms and the existing outbox.
/// Positive corporate credits use the existing member-audience notification paths.
pub(crate) async fn settle_reward<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, recipient: i64, gross_isk: f64, loyalty: Option<LoyaltyReward>) -> Result<(), CallError> {
    if !gross_isk.is_finite() { return Err(CallError::Internal(anyhow::anyhow!("nonfinite corporation reward"))); }
    let gross = super::super::market::round2(gross_isk);
    if !gross.is_finite() { return Err(CallError::Internal(anyhow::anyhow!("corporation reward overflow"))); }
    let exact_gross = crate::isk::IskAmount::from_isk(gross)?;
    settle_exact_reward(ex, outbox, recipient, gross, exact_gross, loyalty).await
}

pub(crate) async fn settle_whole_reward<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, recipient: i64, gross: i64, loyalty: Option<LoyaltyReward>) -> Result<(), CallError> {
    let exact_gross = crate::isk::IskAmount::from_whole_isk(i128::from(gross)).ok_or(crate::isk::AmountError)?;
    settle_exact_reward(ex, outbox, recipient, gross as f64, exact_gross, loyalty).await
}

async fn settle_exact_reward<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, recipient: i64, gross: f64, exact_gross: crate::isk::IskAmount, loyalty: Option<LoyaltyReward>) -> Result<(), CallError> {
    let terms = dao::terms(ex, recipient).await.map_err(db_err)?.filter(|row| super::is_player_corp(row.corporation_id));
    if let Some(row) = &terms {
        if !row.isk_rate.is_finite() || !(0.0..=1.0).contains(&row.isk_rate) || !row.lp_rate.is_finite() || !(0.0..=1.0).contains(&row.lp_rate) {
            return Err(CallError::Internal(anyhow::anyhow!("invalid stored corporation tax rate")));
        }
    }
    let isk_tax = terms.as_ref().filter(|_| gross > MIN_CORPORATION_TAX_AMOUNT)
        .map_or(0.0, |row| super::super::market::round2(gross * row.isk_rate));
    let exact_tax = crate::isk::IskAmount::from_isk(isk_tax)?;
    let net = exact_gross.checked_sub(exact_tax).ok_or(crate::isk::AmountError)?;
    super::super::wallets::credit(ex, outbox, recipient, net).await?;
    if let Some(row) = terms.as_ref().filter(|_| isk_tax > 0.0) {
        super::super::wallets::corp_credit(ex, outbox, row.corporation_id, ACCOUNT_CASH, exact_tax).await?;
    }
    if let Some(loyalty) = loyalty {
        let lp_tax = match terms.as_ref() {
            Some(row) if loyalty.amount > 0 && loyalty.issuer != 0 && row.lp_rate > 0.0 => dao::loyalty_tax(ex, loyalty.amount, row.lp_rate).await.map_err(db_err)?,
            _ => 0,
        };
        let corporate_mutation = (lp_tax > 0).then(|| super::super::standings::LoyaltyMutation {
            reason: loyalty.mutation.reason,
            occurred_at: loyalty.mutation.occurred_at,
            context: loyalty.mutation.context.clone(),
        });
        super::super::standings::add_lp(ex, outbox, recipient, loyalty.issuer, loyalty.amount - lp_tax, loyalty.mutation).await?;
        if let (Some(row), Some(mutation)) = (terms.as_ref(), corporate_mutation) {
            super::super::standings::change_lp(ex, outbox, row.corporation_id, loyalty.issuer, lp_tax, true, mutation).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
