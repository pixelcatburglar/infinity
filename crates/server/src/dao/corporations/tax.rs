//! Current corporation reward-tax terms and exact loyalty-point arithmetic.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct Terms { pub corporation_id: i64, pub isk_rate: f64, pub lp_rate: f64 }
impl Terms {
    fn from_row(row: &Row) -> Self { Self { corporation_id: row.get("corporation_id"), isk_rate: row.get("tax_rate"), lp_rate: row.get("lp_tax_rate") } }
}

pub(crate) async fn terms(ex: &(impl Executor + ?Sized), character: i64) -> Result<Option<Terms>, Error> {
    Ok(ex.query_opt("SELECT co.corporation_id, co.tax_rate, co.lp_tax_rate FROM game.characters ch JOIN game.corporations co ON co.corporation_id=ch.corporation_id WHERE ch.character_id=$1 AND NOT ch.deleted AND NOT co.deleted", &[&character]).await?.as_ref().map(Terms::from_row))
}

pub(crate) async fn loyalty_tax(ex: &(impl Executor + ?Sized), amount: i64, rate: f64) -> Result<i64, Error> {
    let row = ex.query_one("SELECT floor($1::bigint::numeric * $2::double precision::numeric)::bigint AS amount", &[&amount, &rate]).await?;
    Ok(row.get("amount"))
}
