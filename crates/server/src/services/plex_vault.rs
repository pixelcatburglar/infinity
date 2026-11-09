//! Transaction-local authority for the account PLEX vault used by vgs and vaultManager.
//! Persists exact balances and an append-only ledger in game.accounts and
//! game.plex_vault_transactions. Inventory conversion and transaction ownership belong
//! to callers; projected balances are only for the client protocol.
use infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT;
use crate::{client::filetime_now, dao::{accounts::plex, characters::persistence}, db::{executor::Executor, outbox::Outbox}, services::{db_err, CallError}};
use marshal::Value;

/// The numeric(36,0) account storage limit.
const MAX_BALANCE: i128 = 999_999_999_999_999_999_999_999_999_999_999_999;

#[derive(Clone, Copy)]
pub(crate) enum VaultChange { Add(i128), Set(i128) }

#[derive(Clone, Copy)]
pub(crate) enum VaultKind { StorePurchase, GmSet, GmAdd, InventoryDeposit, MarketWithdrawal, DailyReward, MarketFill { order_id: i64, trade_id: i64 }, MarketRefund { order_id: i64 } }
impl VaultKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::StorePurchase => "store_purchase", Self::GmSet => "gm_set", Self::GmAdd => "gm_add",
            Self::InventoryDeposit => "inventory_deposit", Self::MarketWithdrawal => "market_withdrawal",
            Self::DailyReward => "daily_reward",
            Self::MarketFill { .. } => "market_fill", Self::MarketRefund { .. } => "market_refund",
        }
    }
    fn market_ids(self) -> (Option<i64>, Option<i64>) {
        match self {
            Self::MarketFill { order_id, trade_id } => (Some(order_id), Some(trade_id)),
            Self::MarketRefund { order_id } => (Some(order_id), None),
            _ => (None, None),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum VaultAudience { Account, UnpublishedCaller { character_id: Option<i64> } }

#[derive(Clone, Copy)]
pub(crate) struct VaultContext { pub kind: VaultKind, pub item_id: Option<i64>, pub reference: Option<i64> }


pub(crate) async fn change_in<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, user_id: i64, actor_char_id: Option<i64>, change: VaultChange, context: VaultContext, audience: VaultAudience) -> Result<Option<VaultEntry>, CallError> {
    let before_text = plex::balance(ex, user_id).await.map_err(db_err)?;
    let before: i128 = before_text.parse().map_err(|e| CallError::Internal(anyhow::anyhow!("invalid PLEX balance: {e}")))?;
    let Some(after) = next_balance(before, change) else { return Ok(None) };
    let Some(delta) = after.checked_sub(before) else { return Ok(None) };
    if !plex::update(ex, user_id, &before_text, &after.to_string()).await.map_err(db_err)? { return Ok(None) }
    let now = filetime_now();
    plex::opening(ex, user_id, &before_text, now).await.map_err(db_err)?;
    let (order_id, trade_id) = context.kind.market_ids();
    plex::append(ex, user_id, actor_char_id, &delta.to_string(), &after.to_string(), context.kind.as_str(), now, context.item_id, context.reference, order_id, trade_id).await.map_err(db_err)?;
    let projected = after.min(i128::from(CLIENT_MAX_SHORT_AMOUNT)) as i64;
    let chars = persistence::names(ex, user_id).await.map_err(db_err)?;
    if let VaultAudience::UnpublishedCaller { .. } = audience {
        outbox.notify_client("OnAurumChangeFromVgs", vec![Value::dict([("userid", Value::Int(user_id)), ("balance", Value::Int(projected))])]);
    }
    for character in chars {
        let skip = matches!(audience, VaultAudience::UnpublishedCaller { character_id: Some(id) } if id == character.character_id);
        if !skip {
            outbox.notify_pilot(character.character_id, "OnAurumChangeFromVgs", vec![Value::dict([("userid", Value::Int(user_id)), ("balance", Value::Int(projected))])]);
        }
    }
    Ok(Some(VaultEntry { balance: after, projected }))
}

pub(crate) struct VaultEntry { pub balance: i128, pub projected: i64 }
fn next_balance(before: i128, change: VaultChange) -> Option<i128> {
    let after = match change { VaultChange::Add(delta) => before.checked_add(delta), VaultChange::Set(value) => Some(value) }?;
    (0..=MAX_BALANCE).contains(&after).then_some(after)
}



#[cfg(test)] mod tests;
