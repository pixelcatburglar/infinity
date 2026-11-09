//! Shared exact character and corporation wallet operations.
//! Uses game.characters and game.corp_wallets through the DAOs; callers own transactions.
//! Nonzero successful mutations enqueue notifications and durable wallet events.

pub(crate) mod ownership;
pub(crate) mod journal;

use super::{db_err, notify_error, user_error, CallError};
use super::corps::is_player_corp;
use infinity_core::consts::ACCOUNT_CASH;
use crate::dao::wallets::{characters, corporations, WalletMutation};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::isk::{AmountError, IskAmount, personal_client_cap, corporation_client_cap, parse_decimal_coefficient, MAX_JOURNAL_CENTS};
use infinity_core::messages;
use marshal::Value;

impl From<AmountError> for CallError {
    fn from(_: AmountError) -> Self { notify_error(messages::wallets::INVALID_AMOUNT) }
}

pub(crate) async fn balance(db: &(impl Executor + ?Sized), char_id: i64) -> Result<IskAmount, CallError> {
    characters::balance(db, char_id).await.map_err(db_err)
}
pub(super) async fn corp_balance(db: &(impl Executor + ?Sized), corp: i64, key: i32) -> Result<IskAmount, CallError> {
    corporations::balance(db, corp, key).await.map_err(db_err)
}
pub(super) fn not_enough_money(have: IskAmount, amount: IskAmount) -> CallError {
    user_error(messages::MSG_NOT_ENOUGH_MONEY, vec![("balance".into(), Value::Float(have.client_isk(personal_client_cap()))), ("amount".into(), Value::Float(amount.client_isk(personal_client_cap())))])
}
fn outcome(result: WalletMutation, amount: IskAmount, cap: IskAmount) -> Result<IskAmount, CallError> {
    match result {
        WalletMutation::Applied(balance) => Ok(balance),
        WalletMutation::Insufficient(have) => Err(user_error(messages::MSG_NOT_ENOUGH_MONEY, vec![("balance".into(), Value::Float(have.client_isk(cap))), ("amount".into(), Value::Float(amount.client_isk(cap)))])),
        WalletMutation::Overflow => Err(notify_error(messages::wallets::AMOUNT_TOO_LARGE)),
        WalletMutation::Missing => Err(db_err(anyhow::anyhow!("wallet owner missing"))),
    }
}
fn validate(amount: IskAmount) -> Result<(), CallError> {
    if amount.is_negative() { Err(AmountError.into()) } else { Ok(()) }
}
pub(crate) async fn credit<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, amount: IskAmount) -> Result<(), CallError> {
    validate(amount)?;
    if amount.is_zero() { return Ok(()); }
    let balance = outcome(characters::credit(db, char_id, amount).await.map_err(db_err)?, amount, personal_client_cap())?;
    announce(db, outbox, ownership::FinancialOwner::cash(char_id), balance).await?;
    outbox.event(crate::lua::events::GameEvent::WalletChanged { owner_id: char_id, account_key: ACCOUNT_CASH, balance, delta: amount });
    Ok(())
}
pub(crate) async fn debit<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, amount: IskAmount) -> Result<(), CallError> {
    validate(amount)?;
    if amount.is_zero() { return Ok(()); }
    let balance = outcome(characters::debit(db, char_id, amount).await.map_err(db_err)?, amount, personal_client_cap())?;
    announce(db, outbox, ownership::FinancialOwner::cash(char_id), balance).await?;
    outbox.event(crate::lua::events::GameEvent::WalletChanged { owner_id: char_id, account_key: ACCOUNT_CASH, balance, delta: amount.checked_neg().ok_or(AmountError)? });
    Ok(())
}
fn key_name(key: i32) -> String {
    match key - ACCOUNT_CASH { 0 => "cash".to_string(), n => format!("cash{}", n + 1) }
}
async fn announce<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: ownership::FinancialOwner, balance: IskAmount) -> Result<(), CallError> {
    if is_player_corp(owner.id) {
        let ids = super::corpwallet::readable_members(db, owner.id, owner.wallet_key).await?;
        if ids.is_empty() { return Ok(()); }
        let name = key_name(owner.wallet_key);
        let projected = balance.client_isk(corporation_client_cap());
        for id in ids { outbox.notify_pilot(id, "OnAccountChange", vec![Value::str(&name), Value::Int(owner.id), Value::Float(projected)]); }
    } else {
        outbox.notify_pilot(owner.id, "OnAccountChange", vec![Value::str(key_name(owner.wallet_key)), Value::Int(owner.id), Value::Float(balance.client_isk(personal_client_cap()))]);
    }
    Ok(())
}
pub(super) async fn corp_credit<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, corp: i64, key: i32, amount: IskAmount) -> Result<(), CallError> {
    validate(amount)?;
    if amount.is_zero() || !is_player_corp(corp) { return Ok(()); }
    let balance = outcome(corporations::credit(db, corp, key, amount).await.map_err(db_err)?, amount, corporation_client_cap())?;
    announce(db, outbox, ownership::FinancialOwner { id: corp, wallet_key: key }, balance).await?;
    outbox.event(crate::lua::events::GameEvent::WalletChanged { owner_id: corp, account_key: key, balance, delta: amount });
    Ok(())
}
pub(super) async fn corp_debit<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, corp: i64, key: i32, amount: IskAmount) -> Result<(), CallError> {
    validate(amount)?;
    if amount.is_zero() { return Ok(()); }
    let balance = outcome(corporations::debit(db, corp, key, amount).await.map_err(db_err)?, amount, corporation_client_cap())?;
    announce(db, outbox, ownership::FinancialOwner { id: corp, wallet_key: key }, balance).await?;
    outbox.event(crate::lua::events::GameEvent::WalletChanged { owner_id: corp, account_key: key, balance, delta: amount.checked_neg().ok_or(AmountError)? });
    Ok(())
}

/// Validate a numeric(37,2) journal snapshot and project it symmetrically.
pub(crate) fn journal_balance_client_isk(value: &str, cap: IskAmount) -> Result<f64, AmountError> {
    let cents = parse_decimal_coefficient(value, MAX_JOURNAL_CENTS)?;
    Ok(IskAmount::client_cents(cents, cap))
}

#[cfg(test)]
mod tests;
