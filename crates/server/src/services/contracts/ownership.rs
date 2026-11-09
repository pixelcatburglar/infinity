//! Financial owners and corporate authority for contractProxy, mirroring
//! `eve/client/script/ui/shared/neocom/contracts/contracts.py` and contract details.
//! Uses `game.contracts`, `game.characters` and `game.corp_wallets` through shared APIs.
//! Corporate actions require contract-manager authority and access to the selected wallet;
//! corporate notifications go to active members. Contract-specific journals are not kept.

use infinity_core::consts::{ACCOUNT_CASH, ROLE_CONTRACT_MANAGER};
use crate::dao::{characters::members, contracts::Contract};
use crate::db::{executor::Executor, outbox::Outbox};
use infinity_core::messages;
use crate::services::{corps, corpwallet, db_err, notify_error, CallError};
use crate::services::wallets::ownership::{actor_owner, FinancialOwner};
use marshal::Value;

impl Contract {
    pub(super) fn issuer_owner(&self) -> FinancialOwner {
        actor_owner(self.issuer, self.issuer_corp, self.for_corp, self.issuer_wallet_key)
    }
    pub(super) fn acceptor_owner(&self) -> FinancialOwner {
        FinancialOwner { id: self.acceptor, wallet_key: if corps::is_player_corp(self.acceptor) { self.acceptor_wallet_key } else { ACCOUNT_CASH } }
    }
}

pub(super) async fn authorize_corporate_action(ex: &(impl Executor + ?Sized), actor: i64, corp: i64, wallet_key: i32) -> Result<(), CallError> {
    if !corps::is_player_corp(corp) || !crate::dao::corporations::registry::lock_admission(ex, &[], &[corp], &[actor]).await.map_err(db_err)? {
        return Err(notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED));
    }
    let allowed = corps::permissions::load(ex, actor).await?.is_some_and(|authority|
        corps::permissions::allows(&authority, corp, ROLE_CONTRACT_MANAGER) && corpwallet::can_take(&authority, corp, wallet_key));
    if !allowed { return Err(notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED)); }
    Ok(())
}

pub(super) async fn tell<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner_id: i64, event: &str, contract_id: i64) -> Result<(), CallError> {
    if corps::is_player_corp(owner_id) {
        for id in members::active_ids(ex, owner_id).await.map_err(db_err)? { outbox.notify_pilot(id, event, vec![Value::Int(contract_id)]); }
    } else if owner_id != 0 {
        outbox.notify_pilot(owner_id, event, vec![Value::Int(contract_id)]);
    }
    Ok(())
}
