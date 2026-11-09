//! Account balances, existing transfers, and market and direct skill purchase wallet journal reads.
//! Mirrors `eve/client/script/ui/services/accountsvc.py` GetTransactions and
//! GetEntryTypes, and the corporation wallet division and balance calls.
//! Uses SDE accounting entry types and character, corporation wallet, account
//! identity and journal tables via their DAOs. Other economic journals, legacy
//! GetJournal implementations and corporation market activation are unsupported.
//! Journal amounts and balances are projected to the personal or corporation client cap.
use super::{db_err, keyval, notify_error, user_error, Call, CallError, CallResult};
use super::corps::is_player_corp;
use super::corpwallet::{may_read, may_take, valid_key, DIVISIONS};
use super::wallets::{corp_balance, corp_credit, corp_debit};
use crate::client::{Client, filetime_now};
use infinity_core::consts::{ACCOUNT_CASH, FILETIME_DAY, UE_LOC};
use crate::isk::{IskAmount, personal_client_cap, corporation_client_cap};
use crate::dao::wallets::{characters, journal::{self, Period, Row}};
use crate::db::{executor::Executor, outbox::{Outbox, dispatch_world}};
use infinity_core::messages;
use marshal::Value;
use tracing::info;

/// Client journal's Last30Days option.
const JOURNAL_DAYS: i64 = 30;
/// FILETIME starts in 1601; the server accepts Gregorian dates through year 9999.
const MIN_YEAR: i64 = 1601;
/// Server-supported Gregorian range ends at year 9999.
const MAX_YEAR: i64 = 9999;
/// Gregorian calendar months are numbered 1 through 12.
const MAX_MONTH: i64 = 12;

fn denied() -> CallError {
    user_error(messages::MSG_CRP_ACCESS_DENIED, vec![("reason".into(), Value::tuple([Value::Int(UE_LOC), Value::str(messages::account::NO_WALLET_DIVISION_ACCESS)]))])
}
fn optional_key(value: Option<&Value>) -> Result<Option<i32>, CallError> {
    match value {
        None | Some(Value::None) => Ok(None),
        Some(v) => v.as_int().and_then(|n| i32::try_from(n).ok()).filter(|&k| valid_key(k)).map(Some).ok_or_else(denied),
    }
}
fn period(year: Option<&Value>, month: Option<&Value>) -> Result<Period, CallError> {
    match (year, month) {
        (Some(Value::None), Some(Value::None)) => Ok(Period::Since(filetime_now() - JOURNAL_DAYS * FILETIME_DAY)),
        (Some(y), Some(m)) => match (y.as_int(), m.as_int()) {
            (Some(y), Some(m)) if (MIN_YEAR..=MAX_YEAR).contains(&y) && (1..=MAX_MONTH).contains(&m) => Ok(Period::Month { year: y as i32, month: m as i32 }),
            _ => Err(notify_error(messages::account::INVALID_JOURNAL_DATE)),
        },
        _ => Err(notify_error(messages::account::INVALID_JOURNAL_DATE)),
    }
}
fn journal_row(r: Row, cap: IskAmount) -> Result<Value, CallError> {
    Ok(keyval([( "transactionID", Value::Int(r.transaction_id)), ("transactionDate", Value::Int(r.date)), ("entryTypeID", Value::Int(r.entry_type_id.into())), ("ownerID1", Value::Int(r.owner1)), ("ownerID2", Value::Int(r.owner2)), ("referenceID", Value::Int(r.reference_id)), ("amount", Value::Float(r.amount.client_isk(cap))), ("balance", Value::Float(super::wallets::journal_balance_client_isk(&r.balance, cap).map_err(db_err)?)), ("description", Value::unicode(r.description))]))
}
async fn pay_to<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, to: i64, to_key: Option<i32>, amount: IskAmount) -> Result<(), CallError> {
    if is_player_corp(to) {
        let key = to_key.filter(|k| valid_key(*k)).unwrap_or(ACCOUNT_CASH);
        return corp_credit(db, outbox, to, key, amount).await;
    }
    let exists = characters::recipient_exists(db, to).await.map_err(db_err)?;
    if !exists { return Err(notify_error(messages::TRANSFER_RECIPIENT_NOT_PLAYER)); }
    super::wallets::credit(db, outbox, to, amount).await
}
fn amount_arg(call: &Call<'_>, i: usize) -> f64 {
    call.args.get(i).and_then(|v| v.as_float().or_else(|| v.as_int().map(|i| i as f64))).unwrap_or(0.0)
}
fn key_kwarg(call: &Call<'_>, name: &str, i: usize) -> Option<i32> {
    call.kwargs.get(name).or_else(|| call.args.get(i)).and_then(|v| v.as_int()).map(|k| k as i32)
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "account" { return None; }
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    Some(match call.method {
        "GetCashBalance" => async {
            if call.args.first().and_then(|v| v.as_int()).unwrap_or(0) == 0 { return super::wallets::balance(db, me).await.map(|v| Value::Float(v.client_isk(personal_client_cap()))); }
            let key = optional_key(call.kwargs.get("accountKey").or_else(|| call.args.get(1)))?;
            if !may_read(db, me, corp, key).await? { return Err(denied()); }
            let selected = match key {
                Some(k) => k,
                None => match c.session.int("corpAccountKey") {
                    None => ACCOUNT_CASH,
                    Some(k) => i32::try_from(k).ok().filter(|&k| valid_key(k)).ok_or_else(denied)?,
                },
            };
            corp_balance(db, corp, selected).await.map(|v| Value::Float(v.client_isk(corporation_client_cap())))
        }.await,
        "GetWalletDivisionsInfo" => async {
            if !is_player_corp(corp) { return Ok(Value::list([])); }
            let mut out = Vec::new();
            for key in ACCOUNT_CASH..ACCOUNT_CASH + DIVISIONS {
                let balance = if may_read(db, me, corp, Some(key)).await? { corp_balance(db, corp, key).await?.client_isk(corporation_client_cap()) } else { 0.0 };
                out.push(keyval([("key", Value::Int(key.into())), ("balance", Value::Float(balance))]));
            }
            Ok(Value::List(out))
        }.await,
        "GetTransactions" => async {
            let key = optional_key(call.args.first())?;
            let when = period(call.args.get(1), call.args.get(2))?;
            let is_corp = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(0) != 0 || matches!(call.args.get(3), Some(Value::Bool(true)));
            let owner = if is_corp {
                if !may_read(db, me, corp, key).await? { return Err(denied()); }
                corp
            } else {
                if key != Some(ACCOUNT_CASH) { return Err(notify_error(messages::account::INVALID_ACCOUNT_KEY)); }
                me
            };
            let cap = if is_corp { corporation_client_cap() } else { personal_client_cap() };
            let rows = journal::list(db, owner, key, when).await.map_err(db_err)?;
            Ok(Value::list(rows.into_iter().map(|row| journal_row(row, cap)).collect::<Result<Vec<_>, _>>()?))
        }.await,
        "GetEntryTypes" => Ok(Value::list(c.sde.accounting.iter().map(|e| keyval([("entryTypeID", Value::Int(e.id.into())), ("entryTypeNameID", Value::Int(e.name_id.into()))])))),
        "GiveCash" => {
            let to = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let amount = amount_arg(call, 1);
            if amount <= 0.0 || to == me { return Some(Err(notify_error(messages::TRANSFER_INVALID))); }
            let amount = match IskAmount::from_isk(amount) { Ok(v) if !v.is_zero() => v, Ok(_) => return Some(Err(notify_error(messages::TRANSFER_INVALID))), Err(e) => return Some(Err(e.into())) };
            let to_key = key_kwarg(call, "toAccountKey", 3);
            let _trading = super::market::lock().await;
            let result = db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    super::wallets::debit(tx, outbox, me, amount).await?;
                    pay_to(tx, outbox, to, to_key, amount).await
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await;
            result.map(|()| { info!(user = c.login.user_id, "{me} gave {} ISK to {to}", amount.decimal()); Value::None })
        }
        "GiveCashFromCorpAccount" => {
            let to = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let amount = amount_arg(call, 1);
            let from_key = key_kwarg(call, "fromAccountKey", 2).unwrap_or(ACCOUNT_CASH);
            let to_key = key_kwarg(call, "toAccountKey", 3);
            if amount <= 0.0 || !valid_key(from_key) || (to == corp && to_key == Some(from_key)) { return Some(Err(notify_error(messages::TRANSFER_INVALID))); }
            let amount = match IskAmount::from_isk(amount) { Ok(v) if !v.is_zero() => v, Ok(_) => return Some(Err(notify_error(messages::TRANSFER_INVALID))), Err(e) => return Some(Err(e.into())) };
            let _trading = super::market::lock().await;
            let result = db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    if !crate::dao::corporations::registry::lock_admission(tx, &[], &[corp], &[me]).await.map_err(db_err)?
                        || !may_take(tx, me, corp, from_key).await? {
                        return Err(notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED));
                    }
                    corp_debit(tx, outbox, corp, from_key, amount).await?;
                    pay_to(tx, outbox, to, to_key, amount).await
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await;
            result.map(|()| { info!(user = c.login.user_id, "{me} paid {} ISK from {corp}/{from_key} to {to}", amount.decimal()); Value::None })
        }
        _ => return None,
    })
}
#[cfg(test)]
mod tests;
