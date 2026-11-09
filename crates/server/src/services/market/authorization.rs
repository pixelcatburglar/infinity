//! Trade and division-read authority for `marketProxy`
//! (`eve/client/script/ui/services/marketsvc.py`). Uses game corporation membership,
//! admission locks and wallet roles, not cached session roles. Managers may read authorized
//! divisions but cannot modify or cancel another issuer's order.
use super::{db_err, notify_error, CallError, Order, TradeParty};
use infinity_core::consts::{ACCOUNT_CASH, ROLE_ACCOUNTANT, ROLE_TRADER};
use crate::dao::corporations::registry;
use crate::db::executor::Executor;
use infinity_core::messages;
use crate::services::{corps, corpwallet, identity::Identity, wallets::ownership::FinancialOwner};

pub(super) async fn authorize_trade(ex: &(impl Executor + ?Sized), identity: &Identity, corp_wallet: Option<i32>) -> Result<TradeParty, CallError> {
    let refuse = || notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED);
    let corps = corp_wallet.map(|_| vec![identity.corp_id]).unwrap_or_default();
    if !registry::lock_admission(ex, &[], &corps, &[identity.char_id]).await.map_err(db_err)?
        || !registry::active_character(ex, identity.char_id).await.map_err(db_err)? {
        return Err(refuse());
    }
    let financial = if let Some(key) = corp_wallet {
        let allowed = corps::permissions::load(ex, identity.char_id).await?.is_some_and(|authority|
            corps::permissions::allows(&authority, identity.corp_id, ROLE_ACCOUNTANT | ROLE_TRADER)
                && corpwallet::can_take(&authority, identity.corp_id, key));
        if !allowed { return Err(refuse()); }
        FinancialOwner { id: identity.corp_id, wallet_key: key }
    } else { FinancialOwner::cash(identity.char_id) };
    Ok(TradeParty { issuer: identity.char_id, financial })
}

pub(super) async fn authorize_order(ex: &(impl Executor + ?Sized), identity: &Identity, order: &Order) -> Result<TradeParty, CallError> {
    let refuse = || notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED);
    if order.char_id != Some(identity.char_id) || order.corporation_id.is_some_and(|corp| corp != identity.corp_id) {
        return Err(refuse());
    }
    authorize_trade(ex, identity, order.corporation_id.map(|_| order.wallet_key)).await
}

pub(super) fn read_denied() -> CallError {
    crate::services::user_error(messages::MSG_CRP_ACCESS_DENIED, vec![("reason".into(), marshal::Value::tuple([
        marshal::Value::Int(infinity_core::consts::UE_LOC), marshal::Value::str(messages::account::NO_WALLET_DIVISION_ACCESS),
    ]))])
}

pub(super) async fn readable_divisions(ex: &(impl Executor + ?Sized), identity: &Identity, key: Option<i32>) -> Result<Vec<i32>, CallError> {
    let refuse = read_denied;
    if let Some(key) = key {
        if !corpwallet::may_read(ex, identity.char_id, identity.corp_id, Some(key)).await? { return Err(refuse()); }
        return Ok(vec![key]);
    }
    let Some(authority) = corps::permissions::load(ex, identity.char_id).await? else { return Err(refuse()) };
    let all = corpwallet::may_read(ex, identity.char_id, identity.corp_id, None).await?;
    let keys: Vec<_> = (ACCOUNT_CASH..ACCOUNT_CASH + corpwallet::DIVISIONS)
        .filter(|&key| all || corpwallet::can_take(&authority, identity.corp_id, key)).collect();
    if keys.is_empty() { return Err(refuse()); }
    Ok(keys)
}
