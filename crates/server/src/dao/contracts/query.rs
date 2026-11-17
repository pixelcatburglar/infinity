//! Contract list queries for contractProxy; reads game contracts and contract items.
//! Static filtering and pagination of search candidates remain in the service.
use super::{Contract, CONTRACT_SELECT};
use crate::db::{executor::Executor, Error};
use tokio_postgres::types::ToSql;

fn arg<T: ToSql + Sync + Send + 'static>(params: &mut Vec<Box<dyn ToSql + Sync + Send>>, v: T) -> String {
    params.push(Box::new(v));
    format!("${}", params.len())
}
async fn run(ex: &(impl Executor + ?Sized), sql: &str, params: &[Box<dyn ToSql + Sync + Send>], order: &str) -> Result<Vec<Contract>, Error> {
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect();
    Ok(ex.query(&format!("{CONTRACT_SELECT} WHERE {sql} {order}"), &refs).await?.iter().map(Contract::from_row).collect())
}

pub(crate) struct Search {
    pub now: i64, pub assignee: i64, pub kind: i64, pub issuer: Option<i64>,
    pub bounds: [Option<f64>; 8],
    pub description: Option<String>, pub exclude_multiple: bool, pub exclude_trade: bool, pub exclude_no_buyout: bool,
    pub sort: i64, pub descending: bool,
    pub outstanding: i32, pub all: i64, pub combined: i64, pub exchange: i32, pub auction: i32,
}
pub(crate) async fn search(ex: &(impl Executor + ?Sized), f: Search) -> Result<(Vec<Contract>, std::time::Instant), Error> {
    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = vec![];
    let mut sql = format!("c.status = {} AND c.date_expired > {}", f.outstanding, arg(&mut params, f.now));
    sql += &format!(" AND c.assignee_id = {}", arg(&mut params, f.assignee));
    if f.kind == f.combined { sql += &format!(" AND c.type IN ({}, {})", f.exchange, f.auction); }
    else if f.kind != f.all { sql += &format!(" AND c.type = {}", arg(&mut params, f.kind as i32)); }
    if let Some(issuer) = f.issuer { sql += &format!(" AND (CASE WHEN c.for_corp THEN c.issuer_corp_id ELSE c.issuer_id END) = {}", arg(&mut params, issuer)); }
    for (v, (column, op)) in f.bounds.into_iter().zip([("price", ">="), ("price", "<="), ("reward", ">="), ("reward", "<="), ("collateral", ">="), ("collateral", "<="), ("volume", ">="), ("volume", "<=")]) {
        if let Some(v) = v { sql += &format!(" AND c.{column} {op} {}", arg(&mut params, v)); }
    }
    if let Some(text) = f.description { sql += &format!(" AND (c.title ILIKE {p} OR c.description ILIKE {p})", p = arg(&mut params, format!("%{}%", text.trim()))); }
    if f.exclude_multiple { sql += " AND (SELECT count(*) FROM game.contract_items ci WHERE ci.contract_id = c.contract_id AND ci.in_crate) <= 1"; }
    if f.exclude_trade { sql += " AND NOT EXISTS (SELECT 1 FROM game.contract_items ci WHERE ci.contract_id = c.contract_id AND NOT ci.in_crate)"; }
    if f.exclude_no_buyout { sql += &format!(" AND (c.type <> {} OR c.collateral > 0)", f.auction); }
    let column = match f.sort { 1 => "c.price", 2 => "c.date_expired", 3 => "c.start_station_id", 8 => "c.reward", 9 => "c.collateral", 10 => "c.volume", _ => "c.contract_id" };
    let dir = if f.descending { "DESC" } else { "ASC" };
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect();
    let started = std::time::Instant::now();
    let order = format!("ORDER BY {column} {dir}, c.contract_id");
    let found = ex.query(&format!("{CONTRACT_SELECT} WHERE {sql} {order}"), &refs).await?.iter().map(Contract::from_row).collect();
    Ok((found, started))
}

pub(crate) struct Owner {
    pub me: i64, pub issued_by: Option<bool>, pub status: i32, pub kind: Option<i64>, pub before: Option<i64>, pub num: i64,
    pub outstanding: i32, pub in_progress: i32, pub finished: i32, pub all: i64, pub combined: i64, pub exchange: i32, pub auction: i32,
}
pub(crate) async fn owner(ex: &(impl Executor + ?Sized), f: Owner) -> Result<Vec<Contract>, Error> {
    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = vec![];
    let p_me = arg(&mut params, f.me);
    let mut sql = match f.issued_by { Some(false) => format!("(CASE WHEN c.for_corp THEN c.issuer_corp_id ELSE c.issuer_id END) = {p_me}"), Some(true) => format!("(c.acceptor_id = {p_me} OR c.assignee_id = {p_me})"), None => format!("((CASE WHEN c.for_corp THEN c.issuer_corp_id ELSE c.issuer_id END) = {p_me} OR c.acceptor_id = {p_me} OR c.assignee_id = {p_me})") };
    sql += &if f.status == f.outstanding { format!(" AND c.status = {}", f.outstanding) } else if f.status == f.in_progress { format!(" AND c.status = {}", f.in_progress) } else if f.status == f.finished { format!(" AND c.status NOT IN ({}, {})", f.outstanding, f.in_progress) } else { format!(" AND c.status = {}", arg(&mut params, f.status)) };
    match f.kind { None | Some(-1) => {}, Some(t) if t == f.all => {}, Some(t) if t == f.combined => sql += &format!(" AND c.type IN ({}, {})", f.exchange, f.auction), Some(t) => sql += &format!(" AND c.type = {}", arg(&mut params, t as i32)) }
    if let Some(before) = f.before { sql += &format!(" AND c.contract_id < {}", arg(&mut params, before)); }
    run(&ex.uncached(), &sql, &params, &format!("ORDER BY c.contract_id DESC LIMIT {}", f.num)).await
}

pub(crate) enum Filter { Id(i64), Issuer(i64, i32), Package(i64, i32), Attention(i64, i64, i32, i32, i32, i64), Bidding(i64, i32), InProgress(i64, i32), Assigned(i64, i64, i32, i64) }
pub(crate) async fn list(ex: &(impl Executor + ?Sized), f: Filter) -> Result<Vec<Contract>, Error> {
    let mut p: Vec<Box<dyn ToSql + Sync + Send>> = vec![];
    let sql = match f {
        Filter::Id(id) => { arg(&mut p, id); "c.contract_id = $1" },
        Filter::Issuer(me, status) => { arg(&mut p, me); arg(&mut p, status); "(CASE WHEN c.for_corp THEN c.issuer_corp_id ELSE c.issuer_id END) = $1 AND c.status = $2" },
        Filter::Package(id, status) => { arg(&mut p, id); arg(&mut p, status); "c.package_id = $1 AND c.status = $2" },
        Filter::Attention(me, now, outstanding, rejected, progress, day) => { arg(&mut p, me); arg(&mut p, outstanding); arg(&mut p, now); arg(&mut p, rejected); arg(&mut p, progress); arg(&mut p, day); "((CASE WHEN c.for_corp THEN c.issuer_corp_id ELSE c.issuer_id END) = $1 AND ((c.status = $2 AND c.date_expired <= $3) OR c.status = $4))\n         OR (((CASE WHEN c.for_corp THEN c.issuer_corp_id ELSE c.issuer_id END) = $1 OR c.acceptor_id = $1) AND c.status = $5 AND c.date_accepted + c.num_days::bigint * $6 <= $3)" },
        Filter::Bidding(me, status) => { arg(&mut p, me); arg(&mut p, status); "c.status = $2 AND EXISTS (SELECT 1 FROM game.contract_bids b WHERE b.contract_id = c.contract_id AND b.bidder_id = $1)" },
        Filter::InProgress(me, status) => { arg(&mut p, me); arg(&mut p, status); "c.acceptor_id = $1 AND c.status = $2" },
        Filter::Assigned(me, corp, status, now) => { arg(&mut p, me); arg(&mut p, corp); arg(&mut p, status); arg(&mut p, now); "c.assignee_id IN ($1, $2) AND c.status = $3 AND c.date_expired > $4" },
    };
    run(ex, sql, &p, "ORDER BY c.contract_id DESC LIMIT 1000").await
}
