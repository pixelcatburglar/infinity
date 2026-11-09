//! Character wallet authority in game.characters. Callers own transactions and notifications.
//! Amounts are exact numeric text; client projection is outside this DAO.
use crate::{db::{executor::Executor, Error}, isk::IskAmount};
use super::{amount, mutation, WalletError, WalletMutation, MAX_BALANCE};

pub(crate) async fn balance(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<IskAmount, WalletError> {
    amount(&ex.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&char_id]).await?)
}
pub(crate) async fn covers(ex: &(impl Executor + ?Sized), char_id: i64, value: IskAmount) -> Result<bool, WalletError> {
    let value = value.decimal();
    Ok(ex.query_one("SELECT balance >= $2::text::numeric AS covers FROM game.characters WHERE character_id=$1", &[&char_id,&value]).await?.get("covers"))
}
pub(crate) async fn credit(ex: &(impl Executor + ?Sized), char_id: i64, value: IskAmount) -> Result<WalletMutation, WalletError> {
    let value = value.decimal();
    let row = ex.query_one("WITH updated AS (UPDATE game.characters SET balance=balance+$2::text::numeric WHERE character_id=$1 AND $2::text::numeric >= 0 AND balance <= $3::text::numeric-$2::text::numeric RETURNING balance), existing AS (SELECT balance FROM game.characters WHERE character_id=$1) SELECT CASE WHEN EXISTS(SELECT 1 FROM updated) THEN 'applied' WHEN EXISTS(SELECT 1 FROM existing) THEN 'overflow' ELSE 'missing' END AS status, (SELECT balance::text FROM updated) AS balance", &[&char_id,&value,&MAX_BALANCE]).await?;
    mutation(&row)
}
pub(crate) async fn debit(ex: &(impl Executor + ?Sized), char_id: i64, value: IskAmount) -> Result<WalletMutation, WalletError> {
    let value = value.decimal();
    let row = ex.query_one("WITH updated AS (UPDATE game.characters SET balance=balance-$2::text::numeric WHERE character_id=$1 AND $2::text::numeric >= 0 AND balance >= $2::text::numeric RETURNING balance), existing AS (SELECT balance FROM game.characters WHERE character_id=$1) SELECT CASE WHEN EXISTS(SELECT 1 FROM updated) THEN 'applied' WHEN EXISTS(SELECT 1 FROM existing) THEN 'insufficient' ELSE 'missing' END AS status, COALESCE((SELECT balance::text FROM updated),(SELECT balance::text FROM existing)) AS balance", &[&char_id,&value]).await?;
    mutation(&row)
}
pub(crate) async fn set(ex: &(impl Executor + ?Sized), char_id: i64, value: IskAmount) -> Result<WalletMutation, WalletError> {
    let value = value.decimal();
    let row = ex.query_one("WITH updated AS (UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1 AND $2::text::numeric >= 0 RETURNING balance) SELECT CASE WHEN EXISTS(SELECT 1 FROM updated) THEN 'applied' ELSE 'missing' END AS status, (SELECT balance::text FROM updated) AS balance", &[&char_id,&value]).await?;
    mutation(&row)
}
pub(crate) async fn recipient_exists(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<bool, Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.characters WHERE character_id = $1 AND NOT deleted", &[&char_id]).await?.is_some())
}
