//! Share ledger for corpRegistry share panels in `eve/client/script/ui/services/corporation`.
//! Uses game corporations, characters, holdings and transfer receipts. Issuance changes go
//! through the guarded `issue` statement; policy, transactions and notifications are not implemented here.
use crate::db::{self, executor::Executor};

pub(crate) struct Holding {
    pub(crate) corporation_id: i64,
    pub(crate) shareholder_id: i64,
    pub(crate) shareholder_corporation_id: Option<i64>,
    pub(crate) shares: i64,
}
impl Holding {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { corporation_id: r.get("corporation_id"), shareholder_id: r.get("shareholder_id"), shareholder_corporation_id: r.get("shareholder_corporation_id"), shares: r.get("shares") }
    }
}
struct Number { value: i64 }
impl Number { fn from_row(r: &tokio_postgres::Row) -> Self { Self { value: r.get("value") } } }
struct OptionalNumber { value: Option<i64> }
impl OptionalNumber { fn from_row(r: &tokio_postgres::Row) -> Self { Self { value: r.get("value") } } }
struct InitializationResult { safe: bool, value: i64 }
impl InitializationResult {
    fn from_row(r: &tokio_postgres::Row) -> Self { Self { safe: r.get("safe"), value: r.get("value") } }
}

pub(crate) async fn operation_id(ex: &(impl Executor + ?Sized)) -> Result<i64, db::Error> {
    Ok(Number::from_row(&ex.query_one("SELECT nextval('game.corp_governance_operation_seq') AS value", &[]).await?).value)
}
pub(crate) async fn initialize(ex: &(impl Executor + ?Sized), corp: i64, initial: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("WITH claimed AS (UPDATE game.corporations SET issued_shares = $2 WHERE corporation_id = $1 AND issued_shares IS NULL AND NOT EXISTS (SELECT 1 FROM game.corp_shares s WHERE s.corporation_id = $1) RETURNING corporation_id) INSERT INTO game.corp_shares (corporation_id, shareholder_id, shares) SELECT corporation_id, corporation_id, $2 FROM claimed", &[&corp, &initial]).await? != 0)
}
pub(crate) async fn initialize_existing(ex: &(impl Executor + ?Sized), initial: i64) -> Result<Option<u64>, db::Error> {
    let row = ex.query_one("WITH locked AS MATERIALIZED (SELECT corporation_id FROM game.corporations WHERE issued_shares IS NULL ORDER BY corporation_id FOR UPDATE), safety AS MATERIALIZED (SELECT NOT EXISTS (SELECT 1 FROM locked l JOIN game.corp_shares s ON s.corporation_id = l.corporation_id) AS safe), claimed AS (UPDATE game.corporations c SET issued_shares = $1 FROM locked l, safety WHERE safety.safe AND c.corporation_id = l.corporation_id AND c.issued_shares IS NULL RETURNING c.corporation_id), inserted AS (INSERT INTO game.corp_shares (corporation_id, shareholder_id, shares) SELECT corporation_id, corporation_id, $1 FROM claimed ORDER BY corporation_id RETURNING corporation_id) SELECT safety.safe, (SELECT count(*) FROM inserted) AS value FROM safety", &[&initial]).await?;
    let result = InitializationResult::from_row(&row);
    Ok(if result.safe { Some(result.value as u64) } else { None })
}
pub(crate) async fn issued_total(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<i64>, db::Error> {
    Ok(ex.query_opt("SELECT issued_shares AS value FROM game.corporations WHERE corporation_id = $1", &[&corp]).await?.as_ref().and_then(|r| OptionalNumber::from_row(r).value))
}
pub(crate) async fn lock_issuer(ex: &(impl Executor + ?Sized), corp: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT corporation_id FROM game.corporations WHERE corporation_id = $1 AND NOT deleted FOR UPDATE", &[&corp]).await?.is_some())
}
/// Atomically grow the issued total and the issuer's treasury holding in one statement.
/// Callers hold the issuer lock. False means the issuer is missing, its issuance was never
/// initialized, the amount is not positive, or either resulting balance exceeds bigint range.
pub(crate) async fn issue(ex: &(impl Executor + ?Sized), corp: i64, quantity: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("WITH issuer AS (UPDATE game.corporations c SET issued_shares = (c.issued_shares::numeric + $2::numeric)::bigint
            WHERE c.corporation_id = $1 AND NOT c.deleted AND c.issued_shares IS NOT NULL AND $2::bigint > 0
                AND c.issued_shares::numeric + $2::numeric <= 9223372036854775807::numeric
                AND COALESCE((SELECT s.shares FROM game.corp_shares s WHERE s.corporation_id = c.corporation_id AND s.shareholder_id = c.corporation_id), 0)::numeric + $2::numeric <= 9223372036854775807::numeric
            RETURNING c.corporation_id)
        INSERT INTO game.corp_shares (corporation_id, shareholder_id, shares)
        SELECT i.corporation_id, i.corporation_id, (COALESCE((SELECT s.shares FROM game.corp_shares s WHERE s.corporation_id = i.corporation_id AND s.shareholder_id = i.corporation_id), 0)::numeric + $2::numeric)::bigint
        FROM issuer i
        ON CONFLICT (corporation_id, shareholder_id) DO UPDATE SET shares = EXCLUDED.shares", &[&corp, &quantity]).await? == 1)
}
pub(crate) async fn valid_holder(ex: &(impl Executor + ?Sized), holder: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 WHERE EXISTS (SELECT 1 FROM game.characters WHERE character_id = $1 AND NOT deleted) OR EXISTS (SELECT 1 FROM game.corporations WHERE corporation_id = $1 AND NOT deleted)", &[&holder]).await?.is_some())
}
pub(crate) async fn holdings(ex: &(impl Executor + ?Sized), holder: i64) -> Result<Vec<Holding>, db::Error> {
    Ok(ex.query("SELECT s.corporation_id, s.shareholder_id, c.corporation_id AS shareholder_corporation_id, s.shares FROM game.corp_shares s LEFT JOIN game.characters c ON c.character_id = s.shareholder_id AND NOT c.deleted WHERE s.shareholder_id = $1 AND s.shares > 0 ORDER BY s.corporation_id", &[&holder]).await?.iter().map(Holding::from_row).collect())
}
pub(crate) async fn shareholders(ex: &(impl Executor + ?Sized), issuer: i64) -> Result<Vec<Holding>, db::Error> {
    Ok(ex.query("SELECT s.corporation_id, s.shareholder_id, c.corporation_id AS shareholder_corporation_id, s.shares FROM game.corp_shares s LEFT JOIN game.characters c ON c.character_id = s.shareholder_id AND NOT c.deleted WHERE s.corporation_id = $1 AND s.shares > 0 ORDER BY s.shareholder_id", &[&issuer]).await?.iter().map(Holding::from_row).collect())
}
pub(crate) async fn transfer_holdings(ex: &(impl Executor + ?Sized), issuer: i64, holders: &[i64]) -> Result<Vec<Holding>, db::Error> {
    Ok(ex.query("SELECT s.corporation_id, s.shareholder_id, c.corporation_id AS shareholder_corporation_id, s.shares FROM game.corp_shares s LEFT JOIN game.characters c ON c.character_id = s.shareholder_id AND NOT c.deleted WHERE s.corporation_id = $1 AND s.shareholder_id = ANY($2) AND s.shares > 0 ORDER BY s.shareholder_id", &[&issuer, &holders]).await?.iter().map(Holding::from_row).collect())
}
pub(crate) async fn balance(ex: &(impl Executor + ?Sized), issuer: i64, holder: i64) -> Result<i64, db::Error> {
    Ok(ex.query_opt("SELECT shares AS value FROM game.corp_shares WHERE corporation_id = $1 AND shareholder_id = $2", &[&issuer, &holder]).await?.as_ref().map(Number::from_row).map_or(0, |r| r.value))
}
pub(crate) async fn receipt_exists(ex: &(impl Executor + ?Sized), op: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.corp_share_transfers WHERE operation_id=$1", &[&op]).await?.is_some())
}
pub(crate) async fn transfer_receipt(ex: &(impl Executor + ?Sized), op: i64, issuer: i64, from: i64, to: i64, quantity: i64, actor: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.corp_share_transfers (operation_id, corporation_id, from_id, to_id, quantity, actor_id, created) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (operation_id) DO NOTHING", &[&op, &issuer, &from, &to, &quantity, &actor, &now]).await? != 0)
}
pub(crate) async fn transfer(ex: &(impl Executor + ?Sized), issuer: i64, from: i64, to: i64, quantity: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("WITH target_character AS MATERIALIZED (SELECT character_id FROM game.characters WHERE character_id=$3 AND NOT deleted FOR UPDATE), target_corporation AS MATERIALIZED (SELECT corporation_id FROM game.corporations WHERE corporation_id=$3 AND NOT deleted FOR UPDATE), balances AS MATERIALIZED (SELECT COALESCE((SELECT shares FROM game.corp_shares WHERE corporation_id=$1 AND shareholder_id=$2),0)::numeric AS source, COALESCE((SELECT shares FROM game.corp_shares WHERE corporation_id=$1 AND shareholder_id=$3),0)::numeric AS target), changes AS (SELECT v.holder, CASE WHEN v.shares BETWEEN 0 AND 9223372036854775807::numeric THEN v.shares::bigint END AS shares FROM balances b CROSS JOIN LATERAL (VALUES ($2, b.source - $4::bigint), ($3, b.target + $4::bigint)) v(holder, shares) WHERE $4::bigint > 0 AND $2 <> $3 AND b.source >= $4::bigint AND b.target + $4::bigint <= 9223372036854775807::numeric AND (EXISTS (SELECT 1 FROM target_character) OR EXISTS (SELECT 1 FROM target_corporation))) INSERT INTO game.corp_shares (corporation_id, shareholder_id, shares) SELECT $1, holder, shares FROM changes ORDER BY holder ON CONFLICT (corporation_id, shareholder_id) DO UPDATE SET shares = EXCLUDED.shares", &[&issuer, &from, &to, &quantity]).await? == 2)
}
pub(crate) async fn observers(ex: &(impl Executor + ?Sized), holder: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("SELECT character_id AS value FROM game.characters WHERE NOT deleted AND (character_id=$1 OR corporation_id IN (SELECT corporation_id FROM game.corporations WHERE corporation_id=$1 AND NOT deleted)) ORDER BY character_id", &[&holder]).await?.iter().map(|r| Number::from_row(r).value).collect())
}
/// Average is over active issuer members' personal holdings, including members with zero shares.
pub(crate) async fn above_average_members(ex: &(impl Executor + ?Sized), issuer: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("WITH members AS (SELECT c.character_id, COALESCE(s.shares,0) AS shares FROM game.characters c LEFT JOIN game.corp_shares s ON s.corporation_id=$1 AND s.shareholder_id=c.character_id WHERE c.corporation_id=$1 AND NOT c.deleted) SELECT character_id AS value FROM members WHERE shares > (SELECT avg(shares) FROM members) ORDER BY character_id", &[&issuer]).await?.iter().map(|r| Number::from_row(r).value).collect())
}
