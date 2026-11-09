//! Exact market orders in `game.market_orders`; matching and authorization belong to callers.
use crate::{db::{self, executor::Executor}, isk::IskAmount};
use tokio_postgres::Row;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarketScope { Regional, GlobalPlex }
impl MarketScope {
    pub(crate) fn code(self) -> i16 { match self { Self::Regional => 0, Self::GlobalPlex => 1 } }
    pub(crate) fn for_type(type_id: i32) -> Self { if type_id == infinity_core::consts::TYPE_PLEX { Self::GlobalPlex } else { Self::Regional } }
    fn from_code(code: i16) -> Result<Self, db::Error> { match code { 0 => Ok(Self::Regional), 1 => Ok(Self::GlobalPlex), _ => Err(db::Error::Progress(anyhow::anyhow!("invalid market scope {code}"))) } }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseReason { Filled, Expired, Cancelled }
impl CloseReason {
    pub(crate) fn as_str(self) -> &'static str { match self { Self::Filled => "filled", Self::Expired => "expired", Self::Cancelled => "cancelled" } }
}
#[derive(Clone, Copy)]
pub(crate) struct Station { pub id: i64, pub system: i64, pub constellation: i64, pub region: i64, pub owner: i64 }
#[derive(Clone)]
pub(crate) struct Order {
    pub id:i64, pub type_id:i32, pub char_id:Option<i64>, pub at:Station,
    pub corporation_id:Option<i64>, pub wallet_key:i32, pub source_location_id:Option<i64>, pub source_flag_id:Option<i32>,
    pub price:IskAmount, pub vol_entered:i64, pub vol_remaining:i64, pub min_volume:i64,
    pub bid:bool, pub range:i64, pub duration:i64, pub issued:Option<i64>, pub escrow:IskAmount,
    pub scope:MarketScope, pub vault_user_id:Option<i64>, pub plex_escrow:i64, pub created_at:Option<i64>, pub expires_at:Option<i64>,
}
impl Order {
    pub(super) fn from_row(r:&Row) -> Result<Self,db::Error> {
        Ok(Self { id:r.try_get("order_id")?, type_id:r.try_get("type_id")?, char_id:r.try_get("char_id")?,
            corporation_id:r.try_get("corporation_id")?, wallet_key:r.try_get("wallet_key")?, source_location_id:r.try_get("source_location_id")?, source_flag_id:r.try_get("source_flag_id")?,
            at:Station { id:r.try_get("station_id")?, system:r.try_get("solar_system_id")?, constellation:r.try_get("constellation_id")?, region:r.try_get("region_id")?, owner:0 },
            price:IskAmount::parse_decimal(r.try_get("price")?).map_err(|e|db::Error::Progress(e.into()))?, vol_entered:r.try_get("vol_entered")?, vol_remaining:r.try_get("vol_remaining")?, min_volume:r.try_get("min_volume")?, bid:r.try_get("bid")?, range:r.try_get("range")?, duration:r.try_get("duration")?, issued:r.try_get("issued")?,
            escrow:IskAmount::parse_decimal(r.try_get("escrow")?).map_err(|e|db::Error::Progress(e.into()))?, scope:MarketScope::from_code(r.try_get("market_scope")?)?, vault_user_id:r.try_get("vault_user_id")?, plex_escrow:r.try_get("plex_escrow")?, created_at:r.try_get("created_at")?, expires_at:r.try_get("expires_at")? })
    }
}
pub(super) const COLUMNS:&str = "o.order_id, o.type_id, o.char_id, o.corporation_id, o.wallet_key, o.source_location_id, o.source_flag_id, o.station_id, o.solar_system_id, o.constellation_id, o.region_id, o.price::text AS price, o.vol_entered, o.vol_remaining, o.min_volume, o.bid, o.range::bigint AS range, o.duration::bigint AS duration, o.issued, o.escrow::text AS escrow, o.market_scope, o.vault_user_id, o.plex_escrow, o.created_at, o.expires_at";
// Numeric arithmetic prevents overflow when evaluating legacy deadlines.
pub(super) const DEADLINE:&str = "COALESCE(o.expires_at::numeric, o.issued::numeric + o.duration::numeric * $5::bigint::numeric)";
pub(crate) async fn my_order(ex:&(impl Executor+?Sized), char_id:i64, order_id:i64)->Result<Option<Order>,db::Error> {
    ex.query_opt(&format!("SELECT {COLUMNS} FROM game.market_orders o WHERE o.order_id=$1::bigint AND o.char_id=$2::bigint"), &[&order_id,&char_id]).await?.as_ref().map(Order::from_row).transpose()
}
pub(crate) async fn my_orders(ex:&(impl Executor+?Sized), char_id:i64)->Result<Vec<Order>,db::Error> {
    ex.query(&format!("SELECT {COLUMNS} FROM game.market_orders o WHERE o.char_id=$1::bigint ORDER BY o.order_id"), &[&char_id]).await?.iter().map(Order::from_row).collect()
}
pub(crate) async fn corporation_orders(ex:&(impl Executor+?Sized), corporation_id:i64, keys:&[i32])->Result<Vec<Order>,db::Error> {
    ex.query(&format!("SELECT {COLUMNS} FROM game.market_orders o WHERE o.corporation_id=$1::bigint AND o.wallet_key=ANY($2::integer[]) ORDER BY o.order_id"), &[&corporation_id,&keys]).await?.iter().map(Order::from_row).collect()
}
pub(crate) async fn has_open_owner_orders(ex:&(impl Executor+?Sized), owner:i64)->Result<bool,db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.market_orders WHERE corporation_id = $1::bigint UNION ALL SELECT 1 FROM game.market_orders WHERE corporation_id IS NULL AND char_id = $1::bigint LIMIT 1", &[&owner]).await?.is_some())
}
pub(crate) async fn expired(ex:&(impl Executor+?Sized), day:i64, now:i64)->Result<Vec<Order>,db::Error> {
    ex.query(&format!("SELECT {COLUMNS} FROM game.market_orders o WHERE o.char_id IS NOT NULL AND COALESCE(o.expires_at::numeric,o.issued::numeric+o.duration::numeric*$1::bigint::numeric)<=$2::bigint ORDER BY o.order_id"), &[&day,&now]).await?.iter().map(Order::from_row).collect()
}
async fn matching(ex:&(impl Executor+?Sized), now:i64, day:i64, type_id:i32, region:i64, price:IskAmount, bid:bool)->Result<Vec<Order>,db::Error> {
    let scope=MarketScope::for_type(type_id).code(); let price=price.decimal();
    let comparison=if bid { ">=" } else { "<=" }; let direction=if bid { "DESC" } else { "ASC" };
    ex.query(&format!("SELECT {COLUMNS} FROM game.market_orders o WHERE o.type_id=$2::integer AND o.market_scope=$6::smallint AND ($6::smallint=1 OR o.region_id=$3::bigint) AND o.bid=$7::boolean AND o.vol_remaining>0 AND o.price {comparison} $4::text::numeric AND (o.char_id IS NULL OR {DEADLINE}>$1::bigint) ORDER BY o.price {direction},o.issued NULLS FIRST,o.order_id"), &[&now,&type_id,&region,&price,&day,&scope,&bid]).await?.iter().map(Order::from_row).collect()
}
pub(crate) async fn bids(ex:&(impl Executor+?Sized), now:i64, day:i64, type_id:i32, region:i64, floor:IskAmount)->Result<Vec<Order>,db::Error> { matching(ex,now,day,type_id,region,floor,true).await }
pub(crate) async fn asks(ex:&(impl Executor+?Sized), now:i64, day:i64, type_id:i32, region:i64, cap:IskAmount)->Result<Vec<Order>,db::Error> { matching(ex,now,day,type_id,region,cap,false).await }
pub(crate) async fn next_order_id(ex:&(impl Executor+?Sized))->Result<i64,db::Error> { Ok(ex.query_one("SELECT nextval(pg_get_serial_sequence('game.market_orders','order_id')) AS order_id", &[]).await?.try_get("order_id")?) }
pub(crate) async fn insert(ex:&(impl Executor+?Sized), o:&Order)->Result<i64,db::Error> {
    let range=i32::try_from(o.range).map_err(|e|db::Error::Progress(e.into()))?; let duration=i32::try_from(o.duration).map_err(|e|db::Error::Progress(e.into()))?;
    let price=o.price.decimal(); let escrow=o.escrow.decimal(); let scope=o.scope.code(); let now=crate::client::filetime_now();
    let sql=format!("WITH changed AS (INSERT INTO game.market_orders (order_id,type_id,char_id,station_id,solar_system_id,constellation_id,region_id,price,vol_entered,vol_remaining,min_volume,bid,range,duration,issued,escrow,corporation_id,wallet_key,source_location_id,source_flag_id,market_scope,vault_user_id,plex_escrow,created_at,expires_at) SELECT $1::bigint,$2::integer,$3::bigint,$4::bigint,$5::bigint,$6::bigint,$7::bigint,$8::text::numeric,$9::bigint,$10::bigint,$11::bigint,$12::boolean,$13::integer,$14::integer,$15::bigint,$16::text::numeric,$17::bigint,$18::integer,$19::bigint,$20::integer,$21::smallint,$22::bigint,$23::bigint,$24::bigint,$25::bigint WHERE $1::bigint<>0 AND (($2::integer=$27::integer AND $21::smallint=1) OR ($2::integer<>$27::integer AND $21::smallint=0)) RETURNING order_id,market_scope,type_id,region_id), {} SELECT order_id FROM changed",super::versions::bump_cte("$26"));
    Ok(ex.query_one(&sql,&[&o.id,&o.type_id,&o.char_id,&o.at.id,&o.at.system,&o.at.constellation,&o.at.region,&price,&o.vol_entered,&o.vol_remaining,&o.min_volume,&o.bid,&range,&duration,&o.issued,&escrow,&o.corporation_id,&o.wallet_key,&o.source_location_id,&o.source_flag_id,&scope,&o.vault_user_id,&o.plex_escrow,&o.created_at,&o.expires_at,&now,&infinity_core::consts::TYPE_PLEX]).await?.try_get("order_id")?)
}
