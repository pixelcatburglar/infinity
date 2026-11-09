//! Bounty placement and leaderboard mechanics backed by the bounty DAO.
//! The client contract is described by eve/client/script/ui/shared/bountySvc.py.
use super::*;
use infinity_core::messages;
use crate::{client::filetime_now, dao::bounties as dao, db::outbox::{dispatch_world, Outbox}};
use crate::services::{db_err, keyval, notify_error, wallets, market};
use marshal::Value;

/// `game.bounties.kind`: character owner.
pub(super) const CHARACTER: i16 = 1;
/// `game.bounties.kind`: corporation owner.
pub(super) const CORPORATION: i16 = 2;
/// `game.bounties.kind`: alliance owner.
pub(super) const ALLIANCE: i16 = 3;
/// Client notificationconst bounty group and placement types.
const BOUNTY_GROUP: i32 = 10;
const PLACED_CHARACTER: i32 = 112;
const PLACED_CORPORATION: i32 = 113;
const PLACED_ALLIANCE: i32 = 114;

fn optional(id: Option<i64>) -> Value { id.map(Value::Int).unwrap_or(Value::None) }

pub(super) fn row(b: &dao::Bounty) -> Value {
    keyval([("targetID", Value::Int(b.target_id)), ("bounty", Value::Float(b.amount)),
        ("corporationID", optional(b.corporation_id)), ("allianceID", optional(b.alliance_id))])
}
fn contribution(b: &dao::Contribution) -> Value {
    keyval([("contributionID", Value::Int(b.contribution_id)), ("targetID", Value::Int(b.target_id)),
        ("amount", Value::Float(b.amount)), ("corporationID", optional(b.corporation_id)), ("allianceID", optional(b.alliance_id))])
}
fn hunter(b: &dao::Hunter) -> Value {
    keyval([("bountyHunterID", Value::Int(b.bounty_hunter_id)), ("rowNumber", Value::Int(b.row_number)),
        ("corporationID", optional(b.corporation_id)), ("allianceID", optional(b.alliance_id)),
        ("bountiesClaimed", Value::Float(b.bounties_claimed)), ("numberOfKills", Value::Int(b.number_of_kills))])
}
pub(super) fn valid_amount(kind: i16, amount: f64) -> bool {
    let minimum = match kind { CHARACTER => MIN_CHARACTER_BOUNTY, CORPORATION => MIN_CORPORATION_BOUNTY, ALLIANCE => MIN_ALLIANCE_BOUNTY, _ => return false };
    amount.is_finite() && (minimum..=MAX_BOUNTY).contains(&amount)
}

pub(super) async fn add(c: &Client, target: i64, amount: f64) -> CallResult {
    if !amount.is_finite() { return Err(notify_error(messages::bounty::INVALID_BOUNTY_AMOUNT)); }
    let amount = market::round2(amount);
    let me = c.session.int("charid").unwrap_or(0);
    let _lock = market::lock().await;
    let now = filetime_now();
    c.db.transaction(|db, outbox: &mut Outbox| Box::pin(async move {
        // social::owners returns the supplied labels; these are schema kinds, not SDE type IDs.
        let owner = crate::dao::characters::social::owners(db, &[target], CHARACTER as i64, CORPORATION as i64, ALLIANCE as i64).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::bounty::UNKNOWN_OWNER))?;
        let kind = owner.type_id as i16;
        if !valid_amount(kind, amount) { return Err(notify_error(messages::bounty::INVALID_BOUNTY_AMOUNT)); }
        wallets::debit(db, outbox, me, crate::isk::IskAmount::from_isk(amount)?).await?;
        if !dao::add(db, target, kind, amount, MAX_BOUNTY).await.map_err(db_err)? { return Err(notify_error(messages::bounty::INVALID_BOUNTY_AMOUNT)); }
        dao::contribute(db, target, me, amount, now).await.map_err(db_err)?;
        let type_id = match kind { CHARACTER => PLACED_CHARACTER, CORPORATION => PLACED_CORPORATION, _ => PLACED_ALLIANCE };
        let data = format!("{{bountyPlacerID: {me}, bounty: {amount}}}");
        for receiver in dao::members(db, target, kind).await.map_err(db_err)? {
            super::super::mail::notify(db, outbox, receiver, type_id, BOUNTY_GROUP, me, &data).await?;
        }
        let result = dao::get(db, &[target]).await.map_err(db_err)?;
        Ok(row(&result[0]))
    }), |value, outbox| async move { dispatch_world(value, outbox, &c.world) }).await
}

pub(super) fn pairs(rows: &[dao::Bounty]) -> Value {
    Value::list(rows.iter().map(|b| Value::tuple([Value::Int(b.target_id), row(b)])))
}

pub(super) async fn get(c: &Client, ids: &[i64]) -> CallResult {
    Ok(pairs(&dao::get(&c.db, ids).await.map_err(db_err)?))
}
pub(super) async fn combined(c: &Client, ids: &[i64], targets: &[i64]) -> CallResult {
    let bounties = pairs(&dao::get(&c.db, ids).await.map_err(db_err)?);
    let rights = super::kill_rights::visible(c, targets).await?;
    Ok(Value::tuple([bounties, rights]))
}
pub(super) async fn mine(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    Ok(Value::list(dao::contributions(&c.db, me).await.map_err(db_err)?.iter().map(contribution)))
}
fn offset(page: i64) -> i64 { page.max(0).saturating_mul(TOP_LIST_PAGE) }
pub(super) async fn top(c: &Client, kind: i16, page: i64) -> CallResult {
    let rows = dao::top(&c.db, kind, offset(page), TOP_LIST_PAGE).await.map_err(db_err)?;
    Ok(Value::tuple([Value::list(rows.iter().map(row)), Value::Int(filetime_now())]))
}
pub(super) async fn hunters(c: &Client, kind: i16, page: i64) -> CallResult {
    let rows = dao::hunters(&c.db, kind, offset(page), TOP_LIST_PAGE).await.map_err(db_err)?;
    Ok(Value::tuple([Value::list(rows.iter().map(hunter))]))
}
pub(super) async fn search(c: &Client, kind: i16, target: i64) -> CallResult {
    let rows = dao::search(&c.db, kind, target, SEARCH_CONTEXT).await.map_err(db_err)?;
    if rows.is_empty() { return Err(notify_error(messages::bounty::NO_BOUNTY)); }
    Ok(Value::list(rows.iter().map(|r| Value::tuple([Value::Int(r.rank), row(&r.bounty)]))))
}
