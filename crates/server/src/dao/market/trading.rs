//! Exact order mutations, terminal history and fills in the market tables.
//! Matching, settlement, authorization and notifications belong to callers.
use crate::{db::{self,executor::Executor},isk::IskAmount};
use super::orders::{CloseReason,MarketScope,Order};

pub(crate) async fn order_count(ex:&(impl Executor+?Sized),char_id:i64)->Result<i64,db::Error> {
    Ok(ex.query_one("SELECT count(*) AS count FROM game.market_orders WHERE char_id=$1::bigint",&[&char_id]).await?.try_get("count")?)
}
pub(crate) struct NewTransaction {
    pub type_id:i32, pub quantity:i64, pub price:IskAmount,
    pub buyer:i64, pub seller:i64, pub station:i64, pub region:i64, pub date:i64,
    pub buyer_account_id:Option<i64>, pub seller_account_id:Option<i64>, pub order_id:Option<i64>,
    pub scope:MarketScope, pub buy_order_id:Option<i64>, pub sell_order_id:Option<i64>,
    pub buyer_vault_user_id:Option<i64>, pub seller_vault_user_id:Option<i64>,
}
pub(crate) async fn record(ex:&(impl Executor+?Sized),t:&NewTransaction)->Result<i64,db::Error> {
    let price=t.price.decimal(); let scope=t.scope.code();
    Ok(ex.query_one("INSERT INTO game.market_transactions (type_id,quantity,price,buyer_id,seller_id,station_id,region_id,date,buyer_account_id,seller_account_id,order_id,market_scope,buy_order_id,sell_order_id,buyer_vault_user_id,seller_vault_user_id) SELECT $1::integer,$2::bigint,$3::text::numeric,$4::bigint,$5::bigint,$6::bigint,$7::bigint,$8::bigint,$9::bigint,$10::bigint,$11::bigint,$12::smallint,$13::bigint,$14::bigint,$15::bigint,$16::bigint WHERE (($1::integer=$17::integer AND $12::smallint=1) OR ($1::integer<>$17::integer AND $12::smallint=0)) RETURNING transaction_id",&[&t.type_id,&t.quantity,&price,&t.buyer,&t.seller,&t.station,&t.region,&t.date,&t.buyer_account_id,&t.seller_account_id,&t.order_id,&scope,&t.buy_order_id,&t.sell_order_id,&t.buyer_vault_user_id,&t.seller_vault_user_id,&infinity_core::consts::TYPE_PLEX]).await?.try_get("transaction_id")?)
}
/// A successful close owns the deleted order's refund; zero means the guard lost.
pub(crate) async fn close(ex:&(impl Executor+?Sized),order:&Order,remaining:i64,escrow:IskAmount,plex_escrow:i64,state:i32,reason:CloseReason,now:i64)->Result<u64,db::Error> {
    let old_escrow=order.escrow.decimal(); let old_price=order.price.decimal(); let escrow=escrow.decimal(); let scope=order.scope.code(); let reason=reason.as_str();
    let sql=format!("WITH changed AS (DELETE FROM game.market_orders WHERE order_id=$1::bigint AND vol_remaining=$2::bigint AND escrow=$3::text::numeric AND price=$4::text::numeric AND issued IS NOT DISTINCT FROM $5::bigint AND market_scope=$6::smallint AND plex_escrow=$7::bigint AND $8::bigint>=0 AND $8::bigint<=vol_remaining AND $9::text::numeric>=0 AND $10::bigint>=0 RETURNING *), archived AS (INSERT INTO game.market_order_history (order_id,type_id,char_id,station_id,region_id,price,vol_entered,vol_remaining,min_volume,bid,range,duration,issued,state,closed,corporation_id,wallet_key,source_location_id,source_flag_id,market_scope,vault_user_id,plex_escrow,created_at,expires_at,escrow,close_reason) SELECT order_id,type_id,char_id,station_id,region_id,price,vol_entered,$8::bigint,min_volume,bid,range,duration,COALESCE(issued,$13::bigint),$11::integer,$13::bigint,corporation_id,wallet_key,source_location_id,source_flag_id,market_scope,vault_user_id,$10::bigint,created_at,expires_at,$9::text::numeric,$12::text FROM changed RETURNING order_id), {} SELECT count(*)::bigint AS count FROM archived",super::versions::bump_cte("$13"));
    let count:i64=ex.query_one(&sql,&[&order.id,&order.vol_remaining,&old_escrow,&old_price,&order.issued,&scope,&order.plex_escrow,&remaining,&escrow,&plex_escrow,&state,&reason,&now]).await?.try_get("count")?; Ok(count as u64)
}
/// Archive an accepted immediate order with its preallocated identity and final facts.
pub(crate) async fn archive_immediate(ex:&(impl Executor+?Sized),order:&Order,state:i32,reason:CloseReason,now:i64)->Result<i64,db::Error> {
    let range=i32::try_from(order.range).map_err(|e|db::Error::Progress(e.into()))?; let duration=i32::try_from(order.duration).map_err(|e|db::Error::Progress(e.into()))?;
    let price=order.price.decimal(); let escrow=order.escrow.decimal(); let scope=order.scope.code(); let reason=reason.as_str();
    Ok(ex.query_one("INSERT INTO game.market_order_history (order_id,type_id,char_id,station_id,region_id,price,vol_entered,vol_remaining,min_volume,bid,range,duration,issued,state,closed,corporation_id,wallet_key,source_location_id,source_flag_id,market_scope,vault_user_id,plex_escrow,created_at,expires_at,escrow,close_reason) SELECT $1::bigint,$2::integer,$3::bigint,$4::bigint,$5::bigint,$6::text::numeric,$7::bigint,$8::bigint,$9::bigint,$10::boolean,$11::integer,$12::integer,COALESCE($13::bigint,$15::bigint),$14::integer,$15::bigint,$16::bigint,$17::integer,$18::bigint,$19::integer,$20::smallint,$21::bigint,$22::bigint,$23::bigint,$24::bigint,$25::text::numeric,$26::text WHERE $1::bigint<>0 AND (($2::integer=$27::integer AND $20::smallint=1) OR ($2::integer<>$27::integer AND $20::smallint=0)) RETURNING order_id",&[&order.id,&order.type_id,&order.char_id,&order.at.id,&order.at.region,&price,&order.vol_entered,&order.vol_remaining,&order.min_volume,&order.bid,&range,&duration,&order.issued,&state,&now,&order.corporation_id,&order.wallet_key,&order.source_location_id,&order.source_flag_id,&scope,&order.vault_user_id,&order.plex_escrow,&order.created_at,&order.expires_at,&escrow,&reason,&infinity_core::consts::TYPE_PLEX]).await?.try_get("order_id")?)
}
pub(crate) async fn consume(ex:&(impl Executor+?Sized),order:&Order,remaining:i64,escrow:IskAmount,plex_escrow:i64)->Result<u64,db::Error> {
    let old_escrow=order.escrow.decimal(); let old_price=order.price.decimal(); let escrow=escrow.decimal(); let scope=order.scope.code(); let now=crate::client::filetime_now();
    let sql=format!("WITH changed AS (UPDATE game.market_orders SET vol_remaining=$2::bigint,escrow=$3::text::numeric,plex_escrow=$4::bigint WHERE order_id=$1::bigint AND vol_remaining=$5::bigint AND escrow=$6::text::numeric AND price=$7::text::numeric AND issued IS NOT DISTINCT FROM $8::bigint AND market_scope=$9::smallint AND plex_escrow=$10::bigint AND $2::bigint>0 AND $2::bigint<vol_remaining AND $3::text::numeric>=0 AND $4::bigint>=0 RETURNING market_scope,type_id,region_id), {} SELECT count(*)::bigint AS count FROM changed",super::versions::bump_cte("$11"));
    let count:i64=ex.query_one(&sql,&[&order.id,&remaining,&escrow,&plex_escrow,&order.vol_remaining,&old_escrow,&old_price,&order.issued,&scope,&order.plex_escrow,&now]).await?.try_get("count")?; Ok(count as u64)
}
pub(crate) async fn reprice(ex:&(impl Executor+?Sized),order:&Order,price:IskAmount,escrow:IskAmount,issued:i64)->Result<u64,db::Error> {
    let old_escrow=order.escrow.decimal(); let old_price=order.price.decimal(); let price=price.decimal(); let escrow=escrow.decimal(); let scope=order.scope.code(); let now=crate::client::filetime_now();
    let sql=format!("WITH changed AS (UPDATE game.market_orders SET price=$2::text::numeric,escrow=$3::text::numeric,issued=$4::bigint,expires_at=($4::bigint::numeric+duration::numeric*$12::bigint::numeric)::bigint WHERE order_id=$1::bigint AND vol_remaining=$5::bigint AND escrow=$6::text::numeric AND price=$7::text::numeric AND issued IS NOT DISTINCT FROM $8::bigint AND market_scope=$9::smallint AND plex_escrow=$10::bigint AND price<>$2::text::numeric AND $2::text::numeric>0 AND $3::text::numeric>=0 AND ($4::bigint::numeric+duration::numeric*$12::bigint::numeric) BETWEEN $13::bigint::numeric AND $14::bigint::numeric RETURNING market_scope,type_id,region_id), {} SELECT count(*)::bigint AS count FROM changed",super::versions::bump_cte("$11"));
    let count:i64=ex.query_one(&sql,&[&order.id,&price,&escrow,&issued,&order.vol_remaining,&old_escrow,&old_price,&order.issued,&scope,&order.plex_escrow,&now,&infinity_core::consts::FILETIME_DAY,&i64::MIN,&i64::MAX]).await?.try_get("count")?; Ok(count as u64)
}
