//! Journaled wallet mutations for account wallet journal reads in
//! eve/client/script/ui/services/accountsvc.py. Uses wallet accounts and balances through
//! the DAO; callers own transactions and authorization. Historical entries are not fabricated.
use crate::dao::wallets::{accounts, journal::{self, Entry}};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::isk::{AmountError, IskAmount};
use crate::services::{corps::is_player_corp, db_err, wallets::ownership::{self, FinancialOwner}, CallError};

pub(crate) async fn credit<X:Send>(ex:&(impl Executor + ?Sized), outbox:&mut Outbox<X>, owner:FinancialOwner, amount:IskAmount, entry:&Entry<'_>)->Result<(),CallError> {
    if amount.is_negative() { return Err(AmountError.into()); }
    if amount.is_zero() { return Ok(()); }
    ownership::credit(ex,outbox,owner,amount).await?;
    let account = accounts::ensure(ex,owner.id,owner.wallet_key).await.map_err(db_err)?;
    journal::append(ex,account.id,amount,entry).await.map_err(db_err)
}
pub(crate) async fn debit<X:Send>(ex:&(impl Executor + ?Sized), outbox:&mut Outbox<X>, owner:FinancialOwner, amount:IskAmount, entry:&Entry<'_>)->Result<(),CallError> {
    if amount.is_negative() { return Err(AmountError.into()); }
    if amount.is_zero() { return Ok(()); }
    let signed = amount.checked_neg().ok_or(AmountError)?;
    ownership::debit(ex,outbox,owner,amount).await?;
    let account = accounts::ensure(ex,owner.id,owner.wallet_key).await.map_err(db_err)?;
    journal::append(ex,account.id,signed,entry).await.map_err(db_err)
}
pub(crate) async fn debit_order_charge<X:Send>(ex:&(impl Executor + ?Sized), outbox:&mut Outbox<X>, owner:FinancialOwner, total:IskAmount, escrow:IskAmount, entry:&Entry<'_>)->Result<(),CallError> {
    if total.is_negative() || escrow.is_negative() || escrow > total { return Err(AmountError.into()); }
    let fee = total.checked_sub(escrow).ok_or(AmountError)?;
    if total.is_zero() { return Ok(()); }
    ownership::debit(ex,outbox,owner,total).await?;
    let account = accounts::ensure(ex,owner.id,owner.wallet_key).await.map_err(db_err)?;
    journal::append_order_charge(ex,account.id,escrow,fee,entry).await.map_err(db_err)
}
pub(crate) async fn credit_sale<X:Send>(ex:&(impl Executor + ?Sized), outbox:&mut Outbox<X>, owner:FinancialOwner, account_id:i64, gross:IskAmount, net:IskAmount, entry:&Entry<'_>)->Result<(),CallError> {
    if gross.is_negative() || net.is_negative() || net > gross { return Err(AmountError.into()); }
    let tax = gross.checked_sub(net).ok_or(AmountError)?;
    if gross.is_zero() { return Ok(()); }
    ownership::credit(ex,outbox,owner,net).await?;
    journal::append_sale(ex,account_id,gross,tax,entry).await.map_err(db_err)?;
    // accountsvc.py OnAccountChange invalidates the personal and corporation journal caches.
    if net.is_zero() {
        let balance = if is_player_corp(owner.id) {
            super::corp_balance(ex, owner.id, owner.wallet_key).await?
        } else {
            super::balance(ex, owner.id).await?
        };
        super::announce(ex, outbox, owner, balance).await?;
    }
    Ok(())
}
