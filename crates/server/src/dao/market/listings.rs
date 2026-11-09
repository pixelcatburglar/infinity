//! Market book, issuer history, fills and aggregates for `marketProxy`
//! (`eve/client/script/ui/services/marketsvc.py`). Reads use `game.market_orders`,
//! `game.market_order_history`, `game.market_transactions` and `game.wallet_accounts`.
//! PLEX books are global; ordinary books retain their area. Authorization, coherent
//! cached book snapshots, wire projection, mutations and notifications are caller concerns.
use infinity_core::consts::TYPE_PLEX;
use crate::{db::{self, executor::Executor}, isk::IskAmount};
use super::orders::MarketScope;
use tokio_postgres::Row;

fn amount(r: &Row, column: &str) -> Result<IskAmount, db::Error> {
    IskAmount::parse_decimal(r.try_get(column)?).map_err(|e| db::Error::Progress(e.into()))
}

pub(crate) struct BookOrder { pub price: IskAmount, pub vol_remaining: i64, pub range: i64, pub order_id: i64, pub min_volume: i64 }
impl BookOrder { fn from_row(r: &Row) -> Result<Self, db::Error> { Ok(Self { price:amount(r,"price")?, vol_remaining:r.try_get("vol_remaining")?, range:r.try_get("range")?, order_id:r.try_get("order_id")?, min_volume:r.try_get("min_volume")? }) } }
const ORDER_COLUMNS: &str = "o.price::text AS price, o.vol_remaining::bigint AS vol_remaining, o.range::bigint AS range, o.order_id, o.min_volume::bigint AS min_volume";
pub(crate) async fn orders(ex: &(impl Executor + ?Sized), now: i64, day: i64, type_id: i32, area: Area, id: i64, side: Option<bool>) -> Result<Vec<BookOrder>, db::Error> {
    let column = area.column();
    let scope = MarketScope::for_type(type_id).code();
    ex.query(&format!("SELECT {ORDER_COLUMNS} FROM game.market_orders o WHERE o.type_id = $1::integer AND o.market_scope = $6::smallint AND ($6::smallint = 1 OR o.{column} = $2::bigint) AND o.vol_remaining > 0 AND ($3::boolean IS NULL OR o.bid = $3::boolean) AND (o.char_id IS NULL OR COALESCE(o.expires_at::numeric, o.issued::numeric + o.duration::numeric * $5::bigint::numeric) > $4::bigint) ORDER BY o.price, o.issued NULLS FIRST, o.order_id"), &[&type_id, &id, &side, &now, &day, &scope]).await?.iter().map(BookOrder::from_row).collect()
}
pub(crate) enum Area { Station, System, Region }
impl Area {
    fn column(&self) -> &'static str {
        match self { Self::Station => "station_id", Self::System => "solar_system_id", Self::Region => "region_id" }
    }
}
pub(crate) struct BestAsk { pub type_id: i64, pub price: IskAmount, pub vol_remaining: i64, pub station_id: i64 }
impl BestAsk { fn from_row(r: &Row) -> Result<Self, db::Error> { Ok(Self { type_id:r.try_get("type_id")?, price:amount(r,"price")?, vol_remaining:r.try_get("vol_remaining")?, station_id:r.try_get("station_id")? }) } }
/// `type_filter` limits dedicated PLEX best reads without a second query path.
pub(crate) async fn best_asks(ex: &(impl Executor + ?Sized), now: i64, day: i64, area: Area, id: i64, type_filter: Option<i32>) -> Result<Vec<BestAsk>, db::Error> {
    let column = area.column();
    let regional = MarketScope::Regional.code();
    let global = MarketScope::GlobalPlex.code();
    ex.query(&format!("SELECT DISTINCT ON (o.type_id) o.type_id::bigint AS type_id, o.price::text AS price, o.vol_remaining::bigint AS vol_remaining, o.station_id
                 FROM game.market_orders o WHERE ((o.type_id = $4::integer AND o.market_scope = $6::smallint) OR (o.type_id <> $4::integer AND o.market_scope = $5::smallint AND o.{column} = $1::bigint)) AND ($7::integer IS NULL OR o.type_id = $7::integer) AND NOT o.bid AND o.vol_remaining > 0 AND (o.char_id IS NULL OR COALESCE(o.expires_at::numeric, o.issued::numeric + o.duration::numeric * $3::bigint::numeric) > $2::bigint) ORDER BY o.type_id, o.price, o.issued NULLS FIRST, o.order_id"), &[&id, &now, &day, &TYPE_PLEX, &regional, &global, &type_filter]).await?.iter().map(BestAsk::from_row).collect()
}
pub(crate) struct History { pub order_id:i64, pub type_id:i64, pub char_id:i64, pub corporation_id:Option<i64>, pub wallet_key:i32, pub region_id:i64, pub station_id:i64, pub range:i64, pub bid:bool, pub price:IskAmount, pub vol_entered:i64, pub vol_remaining:i64, pub issued:i64, pub min_volume:i64, pub duration:i64, pub state:i64, pub closed:i64 }
impl History { fn from_row(r:&Row)->Result<Self,db::Error> { Ok(Self { order_id:r.try_get("order_id")?,type_id:r.try_get("type_id")?,char_id:r.try_get("char_id")?,corporation_id:r.try_get("corporation_id")?,wallet_key:r.try_get("wallet_key")?,region_id:r.try_get("region_id")?,station_id:r.try_get("station_id")?,range:r.try_get("range")?,bid:r.try_get("bid")?,price:amount(r,"price")?,vol_entered:r.try_get("vol_entered")?,vol_remaining:r.try_get("vol_remaining")?,issued:r.try_get("issued")?,min_volume:r.try_get("min_volume")?,duration:r.try_get("duration")?,state:r.try_get("state")?,closed:r.try_get("closed")? }) } }
pub(crate) async fn history(ex:&(impl Executor + ?Sized), me:i64, since:i64)->Result<Vec<History>,db::Error> {
    ex.query("SELECT order_id, type_id::bigint AS type_id, char_id, corporation_id, wallet_key, region_id, station_id, range::bigint AS range, bid, price::text AS price, vol_entered::bigint AS vol_entered, vol_remaining::bigint AS vol_remaining,
                    issued, min_volume::bigint AS min_volume, duration::bigint AS duration, state::bigint AS state, closed
             FROM game.market_order_history WHERE char_id = $1::bigint AND closed >= $2::bigint ORDER BY closed DESC, order_id DESC LIMIT 1000", &[&me,&since]).await?.iter().map(History::from_row).collect()
}
pub(crate) struct Transaction { pub transaction_id:i64, pub date:i64, pub type_id:i64, pub quantity:i64, pub price:IskAmount, pub station_id:i64, pub buyer_id:i64, pub seller_id:i64, pub buyer_account_id:Option<i64>, pub seller_account_id:Option<i64>, pub account_id:Option<i64> }
impl Transaction { fn from_row(r:&Row)->Result<Self,db::Error> { Ok(Self { transaction_id:r.try_get("transaction_id")?,date:r.try_get("date")?,type_id:r.try_get("type_id")?,quantity:r.try_get("quantity")?,price:amount(r,"price")?,station_id:r.try_get("station_id")?,buyer_id:r.try_get("buyer_id")?,seller_id:r.try_get("seller_id")?,buyer_account_id:r.try_get("buyer_account_id")?,seller_account_id:r.try_get("seller_account_id")?,account_id:r.try_get("account_id")? }) } }
pub(crate) async fn transactions(ex:&(impl Executor + ?Sized),me:i64,from:i64)->Result<Vec<Transaction>,db::Error> {
    ex.query("SELECT t.transaction_id, t.date, t.type_id::bigint AS type_id, t.quantity::bigint AS quantity, t.price::text AS price, t.station_id, t.buyer_id, t.seller_id, t.buyer_account_id, t.seller_account_id,
                 (SELECT account_id FROM game.wallet_accounts WHERE owner_id = $1::bigint AND account_key = 1000) AS account_id
             FROM game.market_transactions t WHERE (t.buyer_id = $1::bigint OR t.seller_id = $1::bigint) AND t.date >= $2::bigint ORDER BY t.date DESC, t.transaction_id DESC LIMIT 2500", &[&me,&from]).await?.iter().map(Transaction::from_row).collect()
}
pub(crate) async fn corporation_transactions(ex:&(impl Executor + ?Sized), corporation_id:i64, keys:&[i32], from:i64)->Result<Vec<Transaction>,db::Error> {
    ex.query("SELECT * FROM (
             SELECT t.transaction_id, t.date, t.type_id::bigint AS type_id, t.quantity::bigint AS quantity, t.price::text AS price, t.station_id, t.buyer_id, t.seller_id, t.buyer_account_id, t.seller_account_id, a.account_id
             FROM game.wallet_accounts a JOIN game.market_transactions t ON t.buyer_account_id = a.account_id
             WHERE a.owner_id = $1::bigint AND a.account_key = ANY($2::integer[]) AND t.date >= $3::bigint
             UNION ALL
             SELECT t.transaction_id, t.date, t.type_id::bigint AS type_id, t.quantity::bigint AS quantity, t.price::text AS price, t.station_id, t.buyer_id, t.seller_id, t.buyer_account_id, t.seller_account_id, a.account_id
             FROM game.wallet_accounts a JOIN game.market_transactions t ON t.seller_account_id = a.account_id
             WHERE a.owner_id = $1::bigint AND a.account_key = ANY($2::integer[]) AND t.date >= $3::bigint
         ) sides ORDER BY date DESC, transaction_id DESC LIMIT 2500", &[&corporation_id,&keys,&from]).await?.iter().map(Transaction::from_row).collect()
}
pub(crate) struct Escrow { pub isk:String, pub items:String }
impl Escrow { fn from_row(r:&Row)->Result<Self,db::Error> { Ok(Self { isk:r.try_get("isk")?, items:r.try_get("items")? }) } }
pub(crate) async fn escrow(ex:&(impl Executor + ?Sized),me:i64)->Result<Escrow,db::Error> {
    Escrow::from_row(&ex.query_one("SELECT COALESCE(sum(escrow) FILTER (WHERE bid), 0)::text AS isk, COALESCE(sum(price * vol_remaining) FILTER (WHERE NOT bid), 0)::text AS items
             FROM game.market_orders WHERE char_id = $1::bigint AND corporation_id IS NULL", &[&me]).await?)
}
pub(crate) struct PriceHistory { pub type_id:i32, pub day:i64, pub low:IskAmount, pub high:IskAmount, pub average:String, pub volume:i128, pub count:i64 }
impl PriceHistory { fn from_row(r:&Row)->Result<Self,db::Error> { Ok(Self { type_id:r.try_get("type_id")?,day:r.try_get("day")?,low:amount(r,"low")?,high:amount(r,"high")?,average:r.try_get("average")?,volume:r.try_get::<_, &str>("volume")?.parse::<i128>().map_err(|e|db::Error::Progress(e.into()))?,count:r.try_get("count")? }) } }
pub(crate) async fn price_history(ex:&(impl Executor + ?Sized),region:i64,type_ids:&[i32],day:i64,since:i64)->Result<Vec<PriceHistory>,db::Error> {
    ex.query("SELECT type_id, (floor(date::numeric / $3::bigint::numeric) * $3::bigint::numeric)::bigint AS day, min(price)::text AS low, max(price)::text AS high, (sum(price * quantity) / sum(quantity))::text AS average, sum(quantity)::text AS volume, count(*) AS count
             FROM game.market_transactions WHERE type_id = ANY($2::integer[]) AND date >= $4::bigint AND (type_id = $5::integer OR region_id = $1::bigint)
             GROUP BY 1, 2 ORDER BY 1, 2", &[&region,&type_ids,&day,&since,&TYPE_PLEX]).await?.iter().map(PriceHistory::from_row).collect()
}
