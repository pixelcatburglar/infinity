//! Coherent market book snapshots and revision updates in `game.market_book_versions`.
//! Wire rowsets, cache policy and scheduling belong to callers.
use crate::db::{self, executor::Executor};
use super::orders::{MarketScope,Order,COLUMNS,DEADLINE};

pub(crate) struct BookSnapshot {
    pub rows:Vec<Order>, pub revision:i64, pub changed_at:i64,
    pub next_visible_expiry:Option<i64>, pub latest_crossed_expiry:Option<i64>,
}
// Every caller supplies a data-modifying CTE named changed with these book columns.
pub(super) fn bump_cte(now:&str)->String {
    format!("bumped AS (INSERT INTO game.market_book_versions AS v (market_scope,type_id,region_key,revision,changed_at) SELECT DISTINCT market_scope,type_id,CASE WHEN market_scope=1 THEN 0 ELSE region_id END,1,{now}::bigint FROM changed ON CONFLICT (market_scope,type_id,region_key) DO UPDATE SET revision=v.revision+1,changed_at=EXCLUDED.changed_at RETURNING revision)")
}
pub(crate) async fn snapshot(ex:&(impl Executor+?Sized), scope:MarketScope, type_id:i32, region:i64, now:i64, day:i64)->Result<BookSnapshot,db::Error> {
    query_snapshot(ex,scope,type_id,region,now,day,false).await
}
pub(crate) async fn best_ask_snapshot(ex:&(impl Executor+?Sized), scope:MarketScope, type_id:i32, region:i64, now:i64, day:i64)->Result<BookSnapshot,db::Error> {
    query_snapshot(ex,scope,type_id,region,now,day,true).await
}
async fn query_snapshot(ex:&(impl Executor+?Sized), scope:MarketScope, type_id:i32, region:i64, now:i64, day:i64, best:bool)->Result<BookSnapshot,db::Error> {
    if scope != MarketScope::for_type(type_id) { return Err(db::Error::Progress(anyhow::anyhow!("market scope does not match type"))); }
    let scope=scope.code();
    let sql=format!("WITH book AS (SELECT o.*, CASE WHEN o.char_id IS NULL THEN NULL ELSE {DEADLINE} END AS deadline FROM game.market_orders o WHERE o.market_scope=$1::smallint AND o.type_id=$2::integer AND ($1::smallint=1 OR o.region_id=$3::bigint)), meta AS (SELECT min(deadline) FILTER (WHERE deadline>$4::bigint AND vol_remaining>0) AS future, max(deadline) FILTER (WHERE deadline<=$4::bigint AND vol_remaining>0) AS crossed FROM book), version AS (SELECT revision,changed_at FROM game.market_book_versions WHERE market_scope=$1::smallint AND type_id=$2::integer AND region_key=CASE WHEN $1::smallint=1 THEN 0 ELSE $3::bigint END), visible AS (SELECT * FROM book o WHERE o.vol_remaining>0 AND (o.char_id IS NULL OR o.deadline>$4::bigint) AND (NOT $6::boolean OR NOT o.bid) ORDER BY o.bid DESC,CASE WHEN o.bid THEN o.price END DESC,CASE WHEN NOT o.bid THEN o.price END ASC,o.issued NULLS FIRST,o.order_id LIMIT CASE WHEN $6::boolean THEN 1 ELSE NULL END) SELECT {COLUMNS}, COALESCE(v.revision,0)::bigint AS revision, COALESCE(v.changed_at,0)::bigint AS changed_at, CASE WHEN m.future IS NOT NULL THEN LEAST(m.future,9223372036854775807::numeric)::bigint END AS next_visible_expiry, CASE WHEN m.crossed IS NOT NULL THEN GREATEST(m.crossed,(-9223372036854775807::numeric-1))::bigint END AS latest_crossed_expiry FROM meta m LEFT JOIN version v ON true LEFT JOIN visible o ON true ORDER BY o.bid DESC,CASE WHEN o.bid THEN o.price END DESC,CASE WHEN NOT o.bid THEN o.price END ASC,o.issued NULLS FIRST,o.order_id");
    let result=ex.query(&sql,&[&scope,&type_id,&region,&now,&day,&best]).await?;
    let first=&result[0];
    let mut rows=Vec::new();
    for row in &result { if row.try_get::<_,Option<i64>>("order_id")?.is_some() { rows.push(Order::from_row(row)?); } }
    Ok(BookSnapshot { rows, revision:first.try_get("revision")?, changed_at:first.try_get("changed_at")?, next_visible_expiry:first.try_get("next_visible_expiry")?, latest_crossed_expiry:first.try_get("latest_crossed_expiry")? })
}
