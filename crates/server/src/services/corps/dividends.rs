//! Shareholder dividends for `eve/client/script/ui/services/corporation/base_corporation.py`.
//! Uses corporation dividend receipts and the shared wallet ledger; member payouts are unsupported.
//! Each recipient's share of the original request is floored independently to cents.
use super::*;
use crate::dao::corporations::{dividends as dao, shares};
use infinity_core::messages::corporations as text;


pub(super) async fn payout_core<X: Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, op: i64, actor: i64, bound: i64, amount: f64, now: i64) -> Result<(), CallError> {
    if dao::receipt_exists(tx, op).await.map_err(db_err)? { return Ok(()); }
    if !amount.is_finite() || amount <= 0.0 { return Err(notify_error(text::INVALID_DIVIDEND)); }
    let (corp, _) = membership(tx, actor).await?;
    if corp != bound || !permissions::is_active_ceo(tx, actor, corp).await? || !super::super::corpwallet::may_take(tx, actor, corp, ACCOUNT_CASH).await? {
        return Err(notify_error(text::DIVIDEND_AUTHORITY_REQUIRED));
    }
    if !shares::lock_issuer(tx, corp).await.map_err(db_err)? { return Err(notify_error(text::DIVIDEND_AUTHORITY_REQUIRED)); }
    let (fresh_corp, _) = membership(tx, actor).await?;
    if fresh_corp != corp || bound != fresh_corp || !permissions::is_active_ceo(tx, actor, fresh_corp).await? || !super::super::corpwallet::may_take(tx, actor, fresh_corp, ACCOUNT_CASH).await? {
        return Err(notify_error(text::DIVIDEND_AUTHORITY_REQUIRED));
    }
    if !dao::claim(tx, op, corp, ACCOUNT_CASH, amount, actor, now).await.map_err(db_err)? { return Ok(()); }
    let Some(allocations) = dao::allocate(tx, op, corp, amount).await.map_err(db_err)? else { return Err(notify_error(text::INVALID_DIVIDEND)); };
    if allocations.is_empty() { return Err(notify_error(text::NO_DIVIDEND_RECIPIENTS)); }
    let total = dao::distributed(tx, op).await.map_err(db_err)?;
    super::super::wallets::corp_debit(tx, outbox, corp, ACCOUNT_CASH, crate::isk::IskAmount::from_isk(total)?).await?;
    for allocation in allocations {
        let amount = crate::isk::IskAmount::from_isk(allocation.amount)?;
        if allocation.corporation { super::super::wallets::corp_credit(tx, outbox, allocation.holder_id, ACCOUNT_CASH, amount).await?; }
        else { super::super::wallets::credit(tx, outbox, allocation.holder_id, amount).await?; }
    }
    Ok(())
}

pub(super) async fn payout(db: &Db, world: &World, actor: i64, bound: i64, amount: f64) -> Result<(), CallError> {
    if !amount.is_finite() || amount <= 0.0 { return Err(notify_error(text::INVALID_DIVIDEND)); }
    let _lock = super::super::market::lock().await;
    let op = shares::operation_id(db).await.map_err(db_err)?;
    let now = filetime_now();
    db.transaction(|tx, outbox| Box::pin(payout_core(tx, outbox, op, actor, bound, amount, now)), |value, outbox| async move { dispatch_world(value, outbox, world) }).await
}
