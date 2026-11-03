//! Market book and history queries for marketProxy
//! (`eve/client/script/ui/services/marketsvc.py`), using game.market_orders,
//! game.market_order_history and game.market_transactions. Wire formatting,
//! matching, notifications and transaction control are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct BookOrder { pub price: f64, pub vol_remaining: i64, pub type_id: i64, pub range: i64, pub order_id: i64, pub vol_entered: i64, pub min_volume: i64, pub bid: bool, pub issued: i64, pub duration: i64, pub station_id: i64, pub region_id: i64, pub solar_system_id: i64, pub constellation_id: i64 }
impl BookOrder { fn from_row(r: &Row) -> Self { Self { price:r.get("price"), vol_remaining:r.get("vol_remaining"), type_id:r.get("type_id"), range:r.get("range"), order_id:r.get("order_id"), vol_entered:r.get("vol_entered"), min_volume:r.get("min_volume"), bid:r.get("bid"), issued:r.get("issued"), duration:r.get("duration"), station_id:r.get("station_id"), region_id:r.get("region_id"), solar_system_id:r.get("solar_system_id"), constellation_id:r.get("constellation_id") } } }
const ORDER_COLUMNS: &str = "price, vol_remaining::bigint, type_id::bigint, range::bigint, order_id, vol_entered::bigint,
    min_volume::bigint, bid, COALESCE(issued, $1) AS issued, duration::bigint, station_id, region_id, solar_system_id, constellation_id";
pub(crate) async fn orders(ex: &(impl Executor + ?Sized), today: i64, type_id: i32, area: Area, id: i64, side: Option<bool>) -> Result<Vec<BookOrder>, db::Error> {
    let column = area.column();
    Ok(ex.query(&format!("SELECT {ORDER_COLUMNS} FROM game.market_orders WHERE type_id = $2 AND {column} = $3 AND vol_remaining > 0 AND ($4::boolean IS NULL OR bid = $4) ORDER BY price"), &[&today, &type_id, &id, &side]).await?.iter().map(BookOrder::from_row).collect())
}
pub(crate) enum Area { Station, System, Region }
impl Area {
    fn column(&self) -> &'static str {
        match self { Self::Station => "station_id", Self::System => "solar_system_id", Self::Region => "region_id" }
    }
}
pub(crate) struct BestAsk { pub type_id: i64, pub price: f64, pub vol_remaining: i64, pub station_id: i64 }
impl BestAsk { fn from_row(r: &Row) -> Self { Self { type_id:r.get("type_id"), price:r.get("price"), vol_remaining:r.get("vol_remaining"), station_id:r.get("station_id") } } }
pub(crate) async fn best_asks(ex: &(impl Executor + ?Sized), area: Area, id: i64) -> Result<Vec<BestAsk>, db::Error> {
    let column = area.column();
    Ok(ex.query(&format!("SELECT DISTINCT ON (type_id) type_id::bigint, price, vol_remaining::bigint, station_id
                 FROM game.market_orders WHERE {column} = $1 AND NOT bid AND vol_remaining > 0 ORDER BY type_id, price"), &[&id]).await?.iter().map(BestAsk::from_row).collect())
}
pub(crate) struct History { pub order_id:i64, pub type_id:i64, pub region_id:i64, pub station_id:i64, pub range:i64, pub bid:bool, pub price:f64, pub vol_entered:i64, pub vol_remaining:i64, pub issued:i64, pub min_volume:i64, pub duration:i64, pub state:i64, pub closed:i64 }
impl History { fn from_row(r:&Row)->Self { Self { order_id:r.get("order_id"),type_id:r.get("type_id"),region_id:r.get("region_id"),station_id:r.get("station_id"),range:r.get("range"),bid:r.get("bid"),price:r.get("price"),vol_entered:r.get("vol_entered"),vol_remaining:r.get("vol_remaining"),issued:r.get("issued"),min_volume:r.get("min_volume"),duration:r.get("duration"),state:r.get("state"),closed:r.get("closed") } } }
pub(crate) async fn history(ex:&(impl Executor + ?Sized), me:i64, since:i64)->Result<Vec<History>,db::Error> {
    Ok(ex.query("SELECT order_id, type_id::bigint, region_id, station_id, range::bigint, bid, price, vol_entered::bigint, vol_remaining::bigint,
                    issued, min_volume::bigint, duration::bigint, state::bigint, closed
             FROM game.market_order_history WHERE char_id = $1 AND closed >= $2 ORDER BY closed DESC LIMIT 1000", &[&me,&since]).await?.iter().map(History::from_row).collect())
}
pub(crate) struct Transaction { pub transaction_id:i64, pub date:i64, pub type_id:i64, pub quantity:i64, pub price:f64, pub station_id:i64, pub buyer_id:i64, pub seller_id:i64 }
impl Transaction { fn from_row(r:&Row)->Self { Self { transaction_id:r.get("transaction_id"),date:r.get("date"),type_id:r.get("type_id"),quantity:r.get("quantity"),price:r.get("price"),station_id:r.get("station_id"),buyer_id:r.get("buyer_id"),seller_id:r.get("seller_id") } } }
pub(crate) async fn transactions(ex:&(impl Executor + ?Sized),me:i64,from:i64)->Result<Vec<Transaction>,db::Error> {
    Ok(ex.query("SELECT transaction_id, date, type_id::bigint, quantity::bigint, price, station_id, buyer_id, seller_id
             FROM game.market_transactions WHERE (buyer_id = $1 OR seller_id = $1) AND date >= $2 ORDER BY date DESC LIMIT 2500", &[&me,&from]).await?.iter().map(Transaction::from_row).collect())
}
pub(crate) struct Escrow { pub isk:f64, pub items:f64 }
impl Escrow { fn from_row(r:&Row)->Self { Self { isk:r.get("isk"), items:r.get("items") } } }
pub(crate) async fn escrow(ex:&(impl Executor + ?Sized),me:i64)->Result<Escrow,db::Error> {
    Ok(Escrow::from_row(&ex.query_one("SELECT COALESCE(sum(escrow) FILTER (WHERE bid), 0)::float8 AS isk, COALESCE(sum(price * vol_remaining) FILTER (WHERE NOT bid), 0)::float8 AS items
             FROM game.market_orders WHERE char_id = $1", &[&me]).await?))
}
pub(crate) struct PriceHistory { pub type_id:i32, pub day:i64, pub low:f64, pub high:f64, pub average:f64, pub volume:i64, pub count:i64 }
impl PriceHistory { fn from_row(r:&Row)->Self { Self { type_id:r.get("type_id"),day:r.get("day"),low:r.get("low"),high:r.get("high"),average:r.get("average"),volume:r.get("volume"),count:r.get("count") } } }
pub(crate) async fn price_history(ex:&(impl Executor + ?Sized),region:i64,type_ids:&[i32],day:i64,since:i64)->Result<Vec<PriceHistory>,db::Error> {
    Ok(ex.query("SELECT type_id, date / $3 * $3 AS day, min(price) AS low, max(price) AS high, sum(price * quantity) / sum(quantity) AS average, sum(quantity)::bigint AS volume, count(*) AS count
             FROM game.market_transactions WHERE region_id = $1 AND type_id = ANY($2) AND date >= $4
             GROUP BY 1, 2 ORDER BY 1, 2", &[&region,&type_ids.to_vec(),&day,&since]).await?.iter().map(PriceHistory::from_row).collect())
}
