//! Tax on actual NPC bounty and mission rewards, settled in the caller's transaction.
//! Other wallet and loyalty mutations do not pass through this producer-specific path.
use infinity_core::consts::ACCOUNT_CASH;
use crate::{dao::corporations::tax as dao, db::{executor::Executor, outbox::Outbox}, services::{db_err, CallError}};

/// Client appConst.minCorporationTaxAmount; server policy taxes only gross rewards strictly above it.
const MIN_CORPORATION_TAX_AMOUNT: f64 = 100_000.0;

pub(crate) struct LoyaltyReward { pub issuer: i64, pub amount: i64, pub mutation: super::super::standings::LoyaltyMutation }

/// The exact settled split of one reward: the net personal credit and the
/// corporation's actual positive tax, when one was credited.
pub(crate) struct RewardSettlement {
    pub net: crate::isk::IskAmount,
    pub corporation: Option<(i64, crate::isk::IskAmount)>,
}

/// Split one actual reward item using current corporation terms and the existing outbox.
/// The caller constructs the exact `gross_isk`; settlement preserves it and subtracts
/// the rounded tax in cents. The floating tax-rate policy itself is unchanged.
/// Positive corporate credits use the existing member-audience notification paths.
pub(crate) async fn settle_reward<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, recipient: i64, gross_isk: crate::isk::IskAmount, loyalty: Option<LoyaltyReward>) -> Result<RewardSettlement, CallError> {
    let terms = dao::terms(ex, recipient).await.map_err(db_err)?.filter(|row| super::is_player_corp(row.corporation_id));
    if let Some(row) = &terms {
        if !row.isk_rate.is_finite() || !(0.0..=1.0).contains(&row.isk_rate) || !row.lp_rate.is_finite() || !(0.0..=1.0).contains(&row.lp_rate) {
            return Err(CallError::Internal(anyhow::anyhow!("invalid stored corporation tax rate")));
        }
    }
    let gross = gross_isk.wire_isk();
    let isk_tax = terms.as_ref().filter(|_| gross > MIN_CORPORATION_TAX_AMOUNT)
        .map_or(0.0, |row| super::super::market::round2(gross * row.isk_rate));
    let exact_tax = crate::isk::IskAmount::from_isk(isk_tax)?;
    let net = gross_isk.checked_sub(exact_tax).ok_or(crate::isk::AmountError)?;
    super::super::wallets::credit(ex, outbox, recipient, net).await?;
    let mut corporation = None;
    if let Some(row) = terms.as_ref().filter(|_| isk_tax > 0.0) {
        super::super::wallets::corp_credit(ex, outbox, row.corporation_id, ACCOUNT_CASH, exact_tax).await?;
        corporation = Some((row.corporation_id, exact_tax));
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
    Ok(RewardSettlement { net, corporation })
}

#[cfg(all(test, feature = "tests-social"))]
mod tests;
