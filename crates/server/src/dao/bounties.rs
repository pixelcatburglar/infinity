//! Player bounty persistence for the bounty service. The client mirrors
//! `eve/client/script/ui/shared/bountySvc.py`; data comes from `game.bounties`,
//! `game.bounty_contributions`, `game.bounty_payouts` and owner tables.
//! Transactions, notifications, NPC bounties and GM operations are not handled here.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) async fn members(ex: &(impl Executor + ?Sized), target_id: i64, kind: i16) -> Result<Vec<i64>, db::Error> {
    let rows = ex.query("SELECT c.character_id AS id FROM game.characters c LEFT JOIN game.corporations corp ON corp.corporation_id = c.corporation_id AND NOT corp.deleted WHERE NOT c.deleted AND (($2::smallint = 1 AND c.character_id = $1) OR ($2::smallint = 2 AND c.corporation_id = $1) OR ($2::smallint = 3 AND corp.alliance_id = $1)) ORDER BY c.character_id", &[&target_id, &kind]).await?;
    Ok(rows.iter().map(|r| r.get::<_, i64>("id")).collect())
}

pub(crate) struct Bounty {
    pub target_id: i64,
    pub amount: f64,
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
}
impl Bounty {
    fn from_row(r: &Row) -> Self { Self { target_id: r.get("target_id"), amount: r.get("amount"), corporation_id: r.get("corporation_id"), alliance_id: r.get("alliance_id") } }
}
const BOUNTY_JOINS: &str = " LEFT JOIN game.characters c ON c.character_id = x.target_id AND NOT c.deleted LEFT JOIN game.corporations cc ON cc.corporation_id = c.corporation_id AND NOT cc.deleted LEFT JOIN game.corporations co ON co.corporation_id = x.target_id AND NOT co.deleted LEFT JOIN game.alliances a ON a.alliance_id = x.target_id AND NOT a.deleted ";
const BOUNTY_COLUMNS: &str = "x.target_id, COALESCE(x.amount, 0)::float8 AS amount, COALESCE(c.corporation_id, co.corporation_id) AS corporation_id, COALESCE(cc.alliance_id, co.alliance_id, a.alliance_id) AS alliance_id";

/// Returns one row per input occurrence, including unknown owners.
pub(crate) async fn get(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<Bounty>, db::Error> {
    let sql = format!("SELECT {BOUNTY_COLUMNS} FROM (SELECT ids.target_id, b.amount, ids.ordinality FROM unnest($1::bigint[]) WITH ORDINALITY ids(target_id, ordinality) LEFT JOIN game.bounties b ON b.target_id = ids.target_id) x {BOUNTY_JOINS} ORDER BY x.ordinality");
    Ok(ex.query(&sql, &[&ids]).await?.iter().map(Bounty::from_row).collect())
}

pub(crate) struct Pool {
    pub target_id: i64,
    pub cents: i64,
}
impl Pool {
    fn from_row(r: &Row) -> Self { Self { target_id: r.get("target_id"), cents: r.get("cents") } }
}
/// Exact positive pools, in primary-key order for payout calculations.
pub(crate) async fn pools(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<Pool>, db::Error> {
    let rows = ex.query("SELECT target_id, (amount * 100)::bigint AS cents FROM game.bounties WHERE target_id = ANY($1) AND amount > 0 ORDER BY target_id", &[&ids]).await?;
    Ok(rows.iter().map(Pool::from_row).collect())
}

/// Compute the payout cap in numeric arithmetic; ISK has 100 cents per unit.
pub(crate) async fn payout_limit(ex: &(impl Executor + ?Sized), available_cents: i64, isk_lost: f64, fraction: f64) -> Result<i64, db::Error> {
    let row = ex.query_one("SELECT LEAST($1::bigint::numeric, FLOOR(GREATEST($2::float8::numeric, 0) * $3::float8::numeric * 100))::bigint AS cents", &[&available_cents, &isk_lost, &fraction]).await?;
    Ok(row.get::<_, i64>("cents"))
}

pub(crate) struct Contribution {
    pub contribution_id: i64,
    pub target_id: i64,
    pub amount: f64,
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
}
impl Contribution {
    fn from_row(r: &Row) -> Self { Self { contribution_id: r.get("contribution_id"), target_id: r.get("target_id"), amount: r.get("amount"), corporation_id: r.get("corporation_id"), alliance_id: r.get("alliance_id") } }
}
pub(crate) async fn contributions(ex: &(impl Executor + ?Sized), placer_id: i64) -> Result<Vec<Contribution>, db::Error> {
    let rows = ex.query("SELECT x.contribution_id, x.target_id, x.amount::float8 AS amount, COALESCE(c.corporation_id, co.corporation_id) AS corporation_id, COALESCE(cc.alliance_id, co.alliance_id, a.alliance_id) AS alliance_id FROM game.bounty_contributions x LEFT JOIN game.characters c ON c.character_id = x.target_id AND NOT c.deleted LEFT JOIN game.corporations cc ON cc.corporation_id = c.corporation_id AND NOT cc.deleted LEFT JOIN game.corporations co ON co.corporation_id = x.target_id AND NOT co.deleted LEFT JOIN game.alliances a ON a.alliance_id = x.target_id AND NOT a.deleted WHERE x.placer_id = $1 ORDER BY x.contribution_id", &[&placer_id]).await?;
    Ok(rows.iter().map(Contribution::from_row).collect())
}
pub(crate) async fn top(ex: &(impl Executor + ?Sized), kind: i16, offset: i64, limit: i64) -> Result<Vec<Bounty>, db::Error> {
    let sql = format!("SELECT {BOUNTY_COLUMNS} FROM game.bounties x {BOUNTY_JOINS} WHERE x.kind = $1 AND x.amount > 0 ORDER BY x.amount DESC, x.target_id LIMIT $3 OFFSET $2");
    Ok(ex.query(&sql, &[&kind, &offset, &limit]).await?.iter().map(Bounty::from_row).collect())
}
pub(crate) struct RankedBounty { pub rank: i64, pub bounty: Bounty }
impl RankedBounty { fn from_row(r: &Row) -> Self { Self { rank: r.get("rank"), bounty: Bounty::from_row(r) } } }
pub(crate) async fn search(ex: &(impl Executor + ?Sized), kind: i16, target_id: i64, context: i64) -> Result<Vec<RankedBounty>, db::Error> {
    let sql = format!("WITH ranked AS (SELECT target_id, amount, row_number() OVER (ORDER BY amount DESC, target_id) - 1 AS rank FROM game.bounties WHERE kind = $1 AND amount > 0), selected AS (SELECT rank FROM ranked WHERE target_id = $2) SELECT {BOUNTY_COLUMNS}, x.rank FROM ranked x {BOUNTY_JOINS} CROSS JOIN selected s WHERE x.rank BETWEEN s.rank - $3 AND s.rank + $3 ORDER BY x.rank");
    Ok(ex.query(&sql, &[&kind, &target_id, &context]).await?.iter().map(RankedBounty::from_row).collect())
}

pub(crate) struct Hunter {
    pub bounty_hunter_id: i64,
    pub row_number: i64,
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
    pub bounties_claimed: f64,
    pub number_of_kills: i64,
}
impl Hunter { fn from_row(r: &Row) -> Self { Self { bounty_hunter_id: r.get("bounty_hunter_id"), row_number: r.get("row_number"), corporation_id: r.get("corporation_id"), alliance_id: r.get("alliance_id"), bounties_claimed: r.get("bounties_claimed"), number_of_kills: r.get("number_of_kills") } } }
pub(crate) async fn hunters(ex: &(impl Executor + ?Sized), kind: i16, offset: i64, limit: i64) -> Result<Vec<Hunter>, db::Error> {
    let rows = ex.query("WITH totals AS (SELECT CASE $1::smallint WHEN 1 THEN p.hunter_id WHEN 2 THEN c.corporation_id WHEN 3 THEN co.alliance_id END AS bounty_hunter_id, sum(p.amount) AS amount, count(*) AS number_of_kills FROM game.bounty_payouts p JOIN game.characters c ON c.character_id = p.hunter_id LEFT JOIN game.corporations co ON co.corporation_id = c.corporation_id WHERE p.amount > 0 GROUP BY 1), ranked AS (SELECT *, row_number() OVER (ORDER BY amount DESC, bounty_hunter_id) AS row_number FROM totals WHERE bounty_hunter_id IS NOT NULL) SELECT r.bounty_hunter_id, r.row_number, COALESCE(c.corporation_id, co.corporation_id) AS corporation_id, COALESCE(cc.alliance_id, co.alliance_id, a.alliance_id) AS alliance_id, r.amount::float8 AS bounties_claimed, r.number_of_kills FROM ranked r LEFT JOIN game.characters c ON c.character_id = r.bounty_hunter_id LEFT JOIN game.corporations cc ON cc.corporation_id = c.corporation_id LEFT JOIN game.corporations co ON co.corporation_id = r.bounty_hunter_id LEFT JOIN game.alliances a ON a.alliance_id = r.bounty_hunter_id ORDER BY r.row_number LIMIT $3 OFFSET $2", &[&kind, &offset, &limit]).await?;
    Ok(rows.iter().map(Hunter::from_row).collect())
}

/// Returns false on a cap or kind mismatch; caller rolls back its wallet debit.
pub(crate) async fn add(ex: &(impl Executor + ?Sized), target_id: i64, kind: i16, amount: f64, max: f64) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.bounties (target_id, kind, amount) SELECT $1, $2, $3::float8::numeric WHERE $3::float8 > 0 AND $3::float8 <= $4::float8 ON CONFLICT (target_id) DO UPDATE SET amount = game.bounties.amount + EXCLUDED.amount WHERE game.bounties.kind = EXCLUDED.kind AND game.bounties.amount + EXCLUDED.amount <= $4::float8::numeric", &[&target_id, &kind, &amount, &max]).await? == 1)
}
pub(crate) async fn contribute(ex: &(impl Executor + ?Sized), target_id: i64, placer_id: i64, amount: f64, now: i64) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.bounty_contributions (target_id, placer_id, amount, placed_at) VALUES ($1, $2, $3::float8::numeric, $4)", &[&target_id, &placer_id, &amount, &now]).await?;
    Ok(())
}
/// Call in ascending target_id order; false requires rolling back the transaction.
pub(crate) async fn debit(ex: &(impl Executor + ?Sized), target_id: i64, cents: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.bounties SET amount = amount - $2::bigint::numeric / 100 WHERE target_id = $1 AND $2::bigint >= 0 AND amount >= $2::bigint::numeric / 100", &[&target_id, &cents]).await? == 1)
}
/// Claim the killmail id in the same transaction as all debits and credit.
pub(crate) async fn record_payout(ex: &(impl Executor + ?Sized), killmail_id: i64, hunter_id: i64, cents: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.bounty_payouts (killmail_id, hunter_id, amount, paid_at) VALUES ($1, $2, $3::bigint::numeric / 100, $4) ON CONFLICT (killmail_id) DO NOTHING", &[&killmail_id, &hunter_id, &cents, &now]).await? == 1)
}
pub(crate) async fn placers(ex: &(impl Executor + ?Sized), target_ids: &[i64]) -> Result<Vec<i64>, db::Error> {
    let rows = ex.query("SELECT DISTINCT placer_id AS id FROM game.bounty_contributions WHERE target_id = ANY($1) ORDER BY id", &[&target_ids]).await?;
    Ok(rows.iter().map(|r| r.get::<_, i64>("id")).collect())
}
