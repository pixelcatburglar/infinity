//! Corporation division wallet balances, credits and debits in `game.corp_wallets`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! It evaluates current live membership and effective global roles for single and
//! readable-member lookups; callers supply masks and own policy, transactions,
//! refusals, notifications and World concerns.

use std::sync::LazyLock;

use crate::dao::characters::roles::{CURRENT_CORPORATION_JOIN_SQL, EFFECTIVE_ROLES_SQL};
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
use crate::isk::IskAmount;
use super::{amount, mutation, WalletError, WalletMutation, MAX_BALANCE};

static READ_PERMISSION_PREDICATE: LazyLock<String> = LazyLock::new(|| format!(
    "co.corporation_id IS NOT NULL AND NOT ch.deleted AND ch.corporation_id = $1 AND ({EFFECTIVE_ROLES_SQL} & $2) <> 0"
));
static READ_ALLOWED_SQL: LazyLock<String> = LazyLock::new(|| format!(
    "SELECT ({}) AS allowed
     {CURRENT_CORPORATION_JOIN_SQL}
     WHERE ch.character_id = $3",
    READ_PERMISSION_PREDICATE.as_str()
));
static READABLE_MEMBERS_SQL: LazyLock<String> = LazyLock::new(|| format!(
    "SELECT ch.character_id
     {CURRENT_CORPORATION_JOIN_SQL}
     WHERE {}
     ORDER BY ch.character_id",
    READ_PERMISSION_PREDICATE.as_str()
));

struct AllowedRow { allowed: bool }
impl AllowedRow { fn from_row(row: &Row) -> Self { Self { allowed: row.get("allowed") } } }
struct MemberRow { character_id: i64 }
impl MemberRow { fn from_row(row: &Row) -> Self { Self { character_id: row.get("character_id") } } }

/// Read authority is evaluated in one set-based predicate shared by single-actor and batch reads.
pub(crate) async fn read_allowed(ex: &(impl Executor + ?Sized), char_id: i64, corp: i64, role_mask: i64) -> Result<bool, Error> {
    Ok(ex.query_opt(READ_ALLOWED_SQL.as_str(), &[&corp, &role_mask, &char_id]).await?.as_ref().is_some_and(|row| AllowedRow::from_row(row).allowed))
}

/// Return active corporation members authorized by the same read predicate as `read_allowed`.
pub(crate) async fn readable_members(ex: &(impl Executor + ?Sized), corp: i64, role_mask: i64) -> Result<Vec<i64>, Error> {
    Ok(ex.query(READABLE_MEMBERS_SQL.as_str(), &[&corp, &role_mask]).await?
        .iter().map(MemberRow::from_row).map(|row| row.character_id).collect())
}

pub(crate) async fn balance(ex: &(impl Executor + ?Sized), corp: i64, key: i32) -> Result<IskAmount, WalletError> {
    match ex.query_opt("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&corp,&key]).await? {
        Some(row) => amount(&row), None => Ok(IskAmount::ZERO),
    }
}

pub(crate) async fn credit(ex: &(impl Executor + ?Sized), corp: i64, key: i32, value: IskAmount) -> Result<WalletMutation, WalletError> {
    let value = value.decimal();
    let row = ex.query_one("WITH updated AS (INSERT INTO game.corp_wallets (corporation_id,account_key,balance) SELECT $1,$2,$3::text::numeric WHERE $3::text::numeric >= 0 ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=game.corp_wallets.balance+$3::text::numeric WHERE game.corp_wallets.balance <= $4::text::numeric-$3::text::numeric RETURNING balance) SELECT CASE WHEN EXISTS(SELECT 1 FROM updated) THEN 'applied' ELSE 'overflow' END AS status, (SELECT balance::text FROM updated) AS balance", &[&corp,&key,&value,&MAX_BALANCE]).await?;
    mutation(&row)
}

pub(crate) async fn debit(ex: &(impl Executor + ?Sized), corp: i64, key: i32, value: IskAmount) -> Result<WalletMutation, WalletError> {
    let value = value.decimal();
    let row = ex.query_one("WITH updated AS (UPDATE game.corp_wallets SET balance=balance-$3::text::numeric WHERE corporation_id=$1 AND account_key=$2 AND $3::text::numeric >= 0 AND balance >= $3::text::numeric RETURNING balance), existing AS (SELECT balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2) SELECT CASE WHEN EXISTS(SELECT 1 FROM updated) THEN 'applied' ELSE 'insufficient' END AS status, COALESCE((SELECT balance::text FROM updated),(SELECT balance::text FROM existing),'0.00') AS balance", &[&corp,&key,&value]).await?;
    mutation(&row)
}
