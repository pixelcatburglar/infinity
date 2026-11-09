//! Corporation wallet division read and withdrawal authorization.
//! Mirrors the division roles used by `eve/client/script/ui/shared/neocom/corporation/corporations.py`.
//! Uses current character membership, effective roles and corporation CEO data.
//! Balance mutations and journals are owned by the shared wallet and account services.
use super::corps::is_player_corp;
use super::{db_err, CallError};
use infinity_core::consts::{ACCOUNT_CASH, ROLE_ACCOUNTANT, ROLE_ACCOUNT_CAN_TAKE_1, ROLE_DIRECTOR, ROLE_JUNIOR_ACCOUNTANT};
use crate::dao::wallets::corporations;
use crate::db::executor::Executor;

/// Client corporation wallet convention: seven account divisions.
pub(super) const DIVISIONS: i32 = 7;

pub(super) fn valid_key(key: i32) -> bool {
    (ACCOUNT_CASH..ACCOUNT_CASH + DIVISIONS).contains(&key)
}

fn read_mask(key: Option<i32>) -> Option<i64> {
    let take = match key {
        Some(key) if valid_key(key) => ROLE_ACCOUNT_CAN_TAKE_1 << (key - ACCOUNT_CASH),
        Some(_) => return None,
        None => 0,
    };
    Some(ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT | take)
}

/// Whether `char_id` may pay out of `corp`'s division `key`.
pub(super) fn can_take(authority: &super::corps::permissions::Authority, corp: i64, key: i32) -> bool {
    if !is_player_corp(corp) || !valid_key(key) { return false; }
    let role = ROLE_ACCOUNT_CAN_TAKE_1 << (key - ACCOUNT_CASH);
    super::corps::permissions::allows(authority, corp, role)
}
pub(super) async fn may_take(db: &(impl Executor + ?Sized), char_id: i64, corp: i64, key: i32) -> Result<bool, CallError> {
    Ok(super::corps::permissions::load(db, char_id).await?.is_some_and(|authority| can_take(&authority, corp, key)))
}

/// Read permissions use one DAO predicate for both single-actor and set-based audience checks.
pub(super) async fn may_read(db: &(impl Executor + ?Sized), char_id: i64, corp: i64, key: Option<i32>) -> Result<bool, CallError> {
    if !is_player_corp(corp) { return Ok(false); }
    let Some(mask) = read_mask(key) else { return Ok(false); };
    corporations::read_allowed(db, char_id, corp, mask).await.map_err(db_err)
}

/// Return active corporation members authorized to receive a division wallet update.
pub(super) async fn readable_members(db: &(impl Executor + ?Sized), corp: i64, key: i32) -> Result<Vec<i64>, CallError> {
    if !is_player_corp(corp) { return Ok(vec![]); }
    let Some(mask) = read_mask(Some(key)) else { return Ok(vec![]); };
    corporations::readable_members(db, corp, mask).await.map_err(db_err)
}

#[cfg(test)]
mod tests;
