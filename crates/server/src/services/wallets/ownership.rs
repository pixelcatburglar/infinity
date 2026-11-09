//! Routes economic ownership to character and corporation wallets; there is no direct
//! client service or client module mirror. Uses game.characters and game.corp_wallets
//! through wallet APIs; callers handle authorization and journaling.

use infinity_core::consts::ACCOUNT_CASH;
use crate::db::{executor::Executor, outbox::Outbox};
use crate::isk::IskAmount;
use crate::services::{corps, wallets, CallError};

#[derive(Clone, Copy)]
pub(crate) struct FinancialOwner { pub(crate) id: i64, pub(crate) wallet_key: i32 }
impl FinancialOwner {
    pub(crate) fn cash(id: i64) -> Self { Self { id, wallet_key: ACCOUNT_CASH } }
}

pub(crate) fn actor_owner(me: i64, corp: i64, for_corp: bool, key: i32) -> FinancialOwner {
    if for_corp { FinancialOwner { id: corp, wallet_key: key } } else { FinancialOwner { id: me, wallet_key: ACCOUNT_CASH } }
}

pub(crate) async fn credit<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: FinancialOwner, amount: IskAmount) -> Result<(), CallError> {
    if corps::is_player_corp(owner.id) { wallets::corp_credit(ex, outbox, owner.id, owner.wallet_key, amount).await } else { wallets::credit(ex, outbox, owner.id, amount).await }
}
pub(crate) async fn debit<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: FinancialOwner, amount: IskAmount) -> Result<(), CallError> {
    if corps::is_player_corp(owner.id) { wallets::corp_debit(ex, outbox, owner.id, owner.wallet_key, amount).await } else { wallets::debit(ex, outbox, owner.id, amount).await }
}
