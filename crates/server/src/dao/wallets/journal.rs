//! Persistent wallet journal writes and exact reads for account wallet journal.
//! Mirrors eve/client/script/ui/services/accountsvc.py; uses game.wallet_accounts,
//! game.wallet_journal and character/corporation balances. It does not fabricate history.
use infinity_core::consts::{ACCOUNT_CASH, FILETIME_SECOND, FILETIME_UNIX_EPOCH, OWNER_SCC, REF_BROKER_FEE, REF_MARKET_ESCROW, REF_MARKET_TRANSACTION, REF_TRANSACTION_TAX};
use crate::db::executor::Executor;
use crate::isk::IskAmount;
use super::WalletError;

pub(crate) struct Entry<'a> { pub(crate) actor_id:i64, pub(crate) entry_type_id:i32, pub(crate) owner_id1:i64, pub(crate) owner_id2:i64, pub(crate) reference_id:i64, pub(crate) created_at:i64, pub(crate) description:&'a str, pub(crate) market_transaction_id:Option<i64>, pub(crate) order_id:Option<i64>, pub(crate) side:i16 }
#[derive(Clone, Copy)]
pub(crate) enum Period { Since(i64), Month { year:i32, month:i32 } }
pub(crate) struct Row { pub(crate) transaction_id:i64, pub(crate) date:i64, pub(crate) entry_type_id:i32, pub(crate) owner1:i64, pub(crate) owner2:i64, pub(crate) reference_id:i64, pub(crate) amount:IskAmount, pub(crate) balance:String, pub(crate) description:String }
impl Row {
    fn from_row(r:&tokio_postgres::Row)->Result<Self,WalletError> {
        Ok(Self { transaction_id:r.get("transaction_id"), date:r.get("date"), entry_type_id:r.get("entry_type_id"), owner1:r.get("owner_id1"), owner2:r.get("owner_id2"), reference_id:r.get("reference_id"), amount:IskAmount::parse_decimal(r.get("amount"))?, balance:r.get("balance_after"), description:r.get("description") })
    }
}

macro_rules! balance_sql { () => { "COALESCE((SELECT c.balance FROM game.characters c WHERE c.character_id = a.owner_id AND a.account_key = $2), (SELECT w.balance FROM game.corp_wallets w WHERE w.corporation_id = a.owner_id AND w.account_key = a.account_key))" }; }
macro_rules! insert_columns { () => { "account_id, actor_id, entry_type_id, owner_id1, owner_id2, reference_id, amount, balance_after, created_at, description, market_transaction_id, order_id, side" }; }
const APPEND_SQL: &str = concat!("INSERT INTO game.wallet_journal (", insert_columns!(), ") SELECT $1,$3,$4,$5,$6,$7,$8::text::numeric,", balance_sql!(), ",$9,$10,$11,$12,$13 FROM game.wallet_accounts a WHERE a.account_id=$1 RETURNING journal_id");
const APPEND_ORDER_ESCROW_SQL: &str = concat!("WITH b AS (SELECT ", balance_sql!(), " AS balance FROM game.wallet_accounts a WHERE a.account_id=$1) INSERT INTO game.wallet_journal (", insert_columns!(), ") SELECT $1,$5,$6,$7,$8,$9,-$3::text::numeric,b.balance + $4::text::numeric,$10,$11,$12,$13,$14 FROM b RETURNING journal_id");
const APPEND_ORDER_FEE_SQL: &str = concat!("WITH b AS (SELECT ", balance_sql!(), " AS balance FROM game.wallet_accounts a WHERE a.account_id=$1) INSERT INTO game.wallet_journal (", insert_columns!(), ") SELECT $1,$4,$5,$6,$7,$8,-$3::text::numeric,b.balance,$9,$10,$11,$12,$13 FROM b RETURNING journal_id");
const APPEND_SALE_GROSS_SQL: &str = concat!("WITH b AS (SELECT ", balance_sql!(), " AS balance FROM game.wallet_accounts a WHERE a.account_id=$1) INSERT INTO game.wallet_journal (", insert_columns!(), ") SELECT $1,$5,$6,$7,$8,$9,$3::text::numeric,b.balance + $4::text::numeric,$10,$11,$12,$13,$14 FROM b RETURNING journal_id");
const APPEND_SALE_TAX_SQL: &str = concat!("WITH b AS (SELECT ", balance_sql!(), " AS balance, a.owner_id FROM game.wallet_accounts a WHERE a.account_id=$1) INSERT INTO game.wallet_journal (", insert_columns!(), ") SELECT $1,$4,$5,b.owner_id,$6,$7,-$3::text::numeric,b.balance,$8,$9,$10,$11,$12 FROM b RETURNING journal_id");
pub(crate) async fn append(ex:&(impl Executor + ?Sized), account_id:i64, amount:IskAmount, entry:&Entry<'_>)->Result<(),WalletError> {
    let amount=amount.decimal();
    ex.query_one(APPEND_SQL, &[&account_id,&ACCOUNT_CASH,&entry.actor_id,&entry.entry_type_id,&entry.owner_id1,&entry.owner_id2,&entry.reference_id,&amount,&entry.created_at,&entry.description,&entry.market_transaction_id,&entry.order_id,&entry.side]).await?;
    Ok(())
}
pub(crate) async fn append_order_charge(ex:&(impl Executor + ?Sized), account_id:i64, escrow:IskAmount, fee:IskAmount, entry:&Entry<'_>)->Result<(),WalletError> {
    if escrow.is_zero() && fee.is_zero() { return Ok(()); }
    let fee_text=fee.decimal();
    if !escrow.is_zero() {
        let escrow_text=escrow.decimal();
        ex.query_one(APPEND_ORDER_ESCROW_SQL, &[&account_id,&ACCOUNT_CASH,&escrow_text,&fee_text,&entry.actor_id,&REF_MARKET_ESCROW,&entry.owner_id1,&OWNER_SCC,&entry.reference_id,&entry.created_at,&entry.description,&entry.market_transaction_id,&entry.order_id,&entry.side]).await?;
    }
    if !fee.is_zero() {
        ex.query_one(APPEND_ORDER_FEE_SQL, &[&account_id,&ACCOUNT_CASH,&fee_text,&entry.actor_id,&REF_BROKER_FEE,&entry.owner_id1,&entry.owner_id2,&entry.reference_id,&entry.created_at,&entry.description,&entry.market_transaction_id,&entry.order_id,&entry.side]).await?;
    }
    Ok(())
}

pub(crate) async fn append_sale(ex:&(impl Executor + ?Sized), account_id:i64, gross:IskAmount, tax:IskAmount, entry:&Entry<'_>)->Result<(),WalletError> {
    if gross.is_zero() && tax.is_zero() { return Ok(()); }
    let tax_text=tax.decimal();
    if !gross.is_zero() {
        let gross_text=gross.decimal();
        ex.query_one(APPEND_SALE_GROSS_SQL, &[&account_id,&ACCOUNT_CASH,&gross_text,&tax_text,&entry.actor_id,&REF_MARKET_TRANSACTION,&entry.owner_id1,&entry.owner_id2,&entry.reference_id,&entry.created_at,&entry.description,&entry.market_transaction_id,&entry.order_id,&entry.side]).await?;
    }
    if !tax.is_zero() {
        ex.query_one(APPEND_SALE_TAX_SQL, &[&account_id,&ACCOUNT_CASH,&tax_text,&entry.actor_id,&REF_TRANSACTION_TAX,&OWNER_SCC,&entry.reference_id,&entry.created_at,&entry.description,&entry.market_transaction_id,&entry.order_id,&entry.side]).await?;
    }
    Ok(())
}

pub(crate) async fn list(ex:&(impl Executor + ?Sized), owner_id:i64, key:Option<i32>, period:Period)->Result<Vec<Row>,WalletError> {
    macro_rules! list_base { () => { "SELECT j.journal_id AS transaction_id, j.created_at AS date, j.entry_type_id, j.owner_id1, j.owner_id2, j.reference_id, j.amount::text AS amount, j.balance_after::text AS balance_after, j.description FROM game.wallet_journal j JOIN game.wallet_accounts a ON a.account_id=j.account_id WHERE a.owner_id = $1 AND ($2::integer IS NULL OR a.account_key = $2) AND " }; }
    let rows = match period {
        Period::Since(at) => ex.query(concat!(list_base!(), "j.created_at >= $3 ORDER BY j.created_at DESC,j.journal_id DESC"), &[&owner_id,&key,&at]).await?,
        Period::Month { year, month } => ex.query(concat!(list_base!(), "j.created_at >= EXTRACT(EPOCH FROM make_date($3,$4,1)::timestamp AT TIME ZONE 'UTC') * $5::bigint + $6::bigint AND j.created_at < EXTRACT(EPOCH FROM (make_date($3,$4,1) + interval '1 month') AT TIME ZONE 'UTC') * $5::bigint + $6::bigint ORDER BY j.created_at DESC,j.journal_id DESC"), &[&owner_id,&key,&year,&month,&FILETIME_SECOND,&FILETIME_UNIX_EPOCH]).await?,
    };
    rows.iter().map(Row::from_row).collect()
}
#[cfg(test)]
mod tests;
