//! Share ownership and transfers for `eve/client/script/ui/services/corporation/bco_shares.py`.
//! Uses the corporation share ledger and transfer receipts; share issuance votes are unsupported.
use super::*;
use infinity_core::consts::{ROLE_ACCOUNTANT, ROLE_JUNIOR_ACCOUNTANT};
use crate::dao::corporations::shares as dao;
use infinity_core::messages::corporations as text;
use crate::services::{crowset, has_role_or_is_ceo};

const COLUMNS: [&str; 4] = ["corporationID", "shareholderID", "shareholderCorporationID", "shares"];

fn header() -> DbRowDescriptor {
    DbRowDescriptor::new([(COLUMNS[0], DbType::I8), (COLUMNS[1], DbType::I8), (COLUMNS[2], DbType::I8), (COLUMNS[3], DbType::I8)])
}
fn line(r: &dao::Holding) -> Vec<Value> {
    vec![Value::Int(r.corporation_id), Value::Int(r.shareholder_id), r.shareholder_corporation_id.map(Value::Int).unwrap_or(Value::None), Value::Int(r.shares)]
}
fn change(old: Option<&dao::Holding>, new: Option<&dao::Holding>) -> Value {
    let old = old.map(line); let new = new.map(line);
    Value::dict(COLUMNS.iter().enumerate().map(|(i, key)| (*key, Value::tuple([old.as_ref().map(|v| v[i].clone()).unwrap_or(Value::None), new.as_ref().map(|v| v[i].clone()).unwrap_or(Value::None)]))))
}

pub(crate) async fn seed(db: &Db) -> Result<(), CallError> {
    let _lock = super::super::market::lock().await;
    db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        if dao::initialize_existing(tx, crate::config::game().corporations.initial_shares).await.map_err(db_err)?.is_none() {
            return Err(CallError::Internal(anyhow::anyhow!("cannot initialize corporation shares: unissued corporation already has holdings")));
        }
        Ok(())
    }), |value, _: Outbox| async move { Ok(value) }).await
}

pub(super) async fn holdings(db: &Db, actor: i64, bound: i64, company: bool) -> CallResult {
    let (corp, _) = membership(db, actor).await?;
    if bound != corp { return Err(notify_error(text::SHARE_AUTHORITY_REQUIRED)); }
    let holder = if company {
        if !has_role_or_is_ceo(db, actor, corp, ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT).await? { return Err(notify_error(text::SHARE_AUTHORITY_REQUIRED)); }
        corp
    } else { actor };
    let rows = dao::holdings(db, holder).await.map_err(db_err)?;
    let h = header();
    Ok(crowset(&h, rows.iter().map(|r| h.row(&line(r))).collect()))
}

pub(super) async fn shareholders(db: &Db, actor: i64, bound: i64, issuer: i64) -> CallResult {
    let (corp, _) = membership(db, actor).await?;
    if bound != corp { return Err(notify_error(text::SHARE_AUTHORITY_REQUIRED)); }
    let active = registry::corporation(db, issuer).await.map_err(db_err)?.is_some_and(|r| !r.deleted);
    let rows = if !active { vec![] } else {
        let personal = dao::balance(db, issuer, actor).await.map_err(db_err)? > 0;
        let corporate = dao::balance(db, issuer, corp).await.map_err(db_err)? > 0 && has_role_or_is_ceo(db, actor, corp, ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT).await?;
        if issuer != corp && !personal && !corporate { return Err(notify_error(text::SHARE_AUTHORITY_REQUIRED)); }
        dao::shareholders(db, issuer).await.map_err(db_err)?
    };
    let h = header();
    Ok(Value::Reduce { data: Box::new(Value::tuple([Value::global("carbon.common.script.sys.crowset.CIndexedRowset"), Value::tuple([h.to_value(), Value::str("shareholderID")])])), list: vec![], dict: rows.iter().map(|r| (Value::Int(r.shareholder_id), h.row(&line(r)))).collect() })
}

pub(super) async fn transfer_core<X: Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, op: i64, actor: i64, bound: i64, issuer: i64, to: i64, quantity: i64, company: bool, now: i64) -> Result<(), CallError> {
    if dao::receipt_exists(tx, op).await.map_err(db_err)? { return Ok(()); }
    let (corp, _) = membership(tx, actor).await?;
    if bound != corp || (company && !has_role_or_is_ceo(tx, actor, corp, ROLE_DIRECTOR).await?) { return Err(notify_error(text::SHARE_AUTHORITY_REQUIRED)); }
    let from = if company { corp } else { actor };
    if issuer <= 0 || to <= 0 || quantity <= 0 || from == to || !dao::lock_issuer(tx, issuer).await.map_err(db_err)? { return Err(notify_error(text::INVALID_SHARE_TRANSFER)); }
    let (fresh_corp, _) = membership(tx, actor).await?;
    if fresh_corp != corp || bound != fresh_corp || (company && !has_role_or_is_ceo(tx, actor, fresh_corp, ROLE_DIRECTOR).await?) { return Err(notify_error(text::SHARE_AUTHORITY_REQUIRED)); }
    if !dao::valid_holder(tx, to).await.map_err(db_err)? { return Err(notify_error(text::INVALID_SHARE_TRANSFER)); }
    if !dao::transfer_receipt(tx, op, issuer, from, to, quantity, actor, now).await.map_err(db_err)? { return Ok(()); }
    let holders = [from, to];
    let old = dao::transfer_holdings(tx, issuer, &holders).await.map_err(db_err)?;
    if !dao::transfer(tx, issuer, from, to, quantity).await.map_err(db_err)? { return Err(notify_error(text::INVALID_SHARE_TRANSFER)); }
    let new = dao::transfer_holdings(tx, issuer, &holders).await.map_err(db_err)?;
    for holder in [from, to] {
        let ch = change(old.iter().find(|r| r.shareholder_id == holder), new.iter().find(|r| r.shareholder_id == holder));
        for observer in dao::observers(tx, holder).await.map_err(db_err)? {
            outbox.notify_pilot(observer, "OnShareChange", vec![Value::Int(holder), Value::Int(issuer), ch.clone()]);
        }
    }
    Ok(())
}

pub(super) async fn transfer(db: &Db, world: &World, actor: i64, bound: i64, issuer: i64, to: i64, quantity: i64, company: bool) -> Result<(), CallError> {
    let _lock = super::super::market::lock().await;
    let op = dao::operation_id(db).await.map_err(db_err)?;
    let now = filetime_now();
    db.transaction(|tx, outbox| Box::pin(transfer_core(tx, outbox, op, actor, bound, issuer, to, quantity, company, now)), |value, outbox| async move { dispatch_world(value, outbox, world) }).await
}
