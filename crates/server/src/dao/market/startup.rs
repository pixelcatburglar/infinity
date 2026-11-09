//! Startup market seeding and retail price persistence using `game.market_orders`,
//! `game.retail_prices`, `sde_native.schools`, `sde.sta_stations`,
//! `sde.inv_type_materials` and `sde.inv_types`; the seed query is `sql/market_seed.sql`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Startup orchestration, transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
const MARKET_SEED: &str = include_str!("../../../sql/market_seed.sql");
struct Seeded { seeded: bool }
impl Seeded { fn from_row(r: &Row) -> Self { Self { seeded: r.get("seeded") } } }
pub(crate) async fn retail_prices(ex: &(impl Executor + ?Sized), ids: &[i32], avg: &[f64], adj: &[f64]) -> Result<u64, db::Error> {
    ex.execute("INSERT INTO game.retail_prices (type_id, average, adjusted)
             SELECT * FROM unnest($1::int[], $2::float8[], $3::float8[])
             ON CONFLICT (type_id) DO UPDATE SET average = EXCLUDED.average, adjusted = EXCLUDED.adjusted", &[&ids, &avg, &adj]).await
}
pub(crate) async fn seeded(ex: &(impl Executor + ?Sized)) -> Result<bool, db::Error> {
    Ok(Seeded::from_row(&ex.query_one("SELECT EXISTS (SELECT 1 FROM game.market_orders WHERE char_id IS NULL) AS seeded", &[]).await?).seeded)
}
pub(crate) async fn seed(ex: &(impl Executor + ?Sized)) -> Result<u64, db::Error> {
    let now=crate::client::filetime_now();
    let today=now / infinity_core::consts::FILETIME_DAY * infinity_core::consts::FILETIME_DAY;
    let seed=MARKET_SEED.trim_end().strip_suffix("SELECT * FROM changed").ok_or_else(||db::Error::Progress(anyhow::anyhow!("invalid market seed query")))?;
    let sql=format!("{seed}, {} SELECT count(*)::bigint AS count FROM changed",super::versions::bump_cte("$3"));
    let count:i64=ex.query_one(&sql,&[&infinity_core::consts::TYPE_PLEX,&today,&now]).await?.try_get("count")?;
    Ok(count as u64)
}
pub(crate) async fn bids_seeded(ex: &(impl Executor + ?Sized)) -> Result<bool, db::Error> {
    Ok(Seeded::from_row(&ex.query_one("SELECT EXISTS (SELECT 1 FROM game.market_orders WHERE char_id IS NULL AND bid) AS seeded", &[]).await?).seeded)
}
pub(crate) async fn seed_bids(ex: &(impl Executor + ?Sized)) -> Result<u64, db::Error> {
    let now=crate::client::filetime_now();
    let today=now / infinity_core::consts::FILETIME_DAY * infinity_core::consts::FILETIME_DAY;
    let sql=format!("WITH changed AS (INSERT INTO game.market_orders (type_id, station_id, solar_system_id, constellation_id, region_id,
                                                     price, vol_entered, vol_remaining, bid, range, duration, issued)
                     SELECT type_id, station_id, solar_system_id, constellation_id, region_id,
                            GREATEST(round((price::numeric * $1::text::numeric), 2), 0.01), 1000000, 1000000, true, 32767, 365, $3::bigint
                     FROM game.market_orders WHERE char_id IS NULL AND NOT bid AND type_id <> $2::integer RETURNING market_scope,type_id,region_id), {} SELECT count(*)::bigint AS count FROM changed",super::versions::bump_cte("$4"));
    let count:i64=ex.query_one(&sql,&[&crate::config::game().market.npc_buy_ratio.to_string(),&infinity_core::consts::TYPE_PLEX,&today,&now]).await?.try_get("count")?;
    Ok(count as u64)
}
pub(crate) async fn reprice(ex: &(impl Executor + ?Sized)) -> Result<u64, db::Error> {
    let now=crate::client::filetime_now();
    let sql=format!("WITH changed AS (UPDATE game.market_orders o
                 SET price = GREATEST(round((r.average * CASE WHEN o.bid THEN $1::float8 ELSE 1 END)::numeric, 2), 0.01)
                 FROM game.retail_prices r
                 WHERE o.char_id IS NULL AND o.type_id = r.type_id AND o.type_id <> $2::integer AND r.average > 0
                   AND o.price <> GREATEST(round((r.average * CASE WHEN o.bid THEN $1::float8 ELSE 1 END)::numeric, 2), 0.01)
                 RETURNING o.market_scope,o.type_id,o.region_id), {} SELECT count(*)::bigint AS count FROM changed",super::versions::bump_cte("$3"));
    let count:i64=ex.query_one(&sql,&[&crate::config::game().market.npc_buy_ratio,&infinity_core::consts::TYPE_PLEX,&now]).await?.try_get("count")?;
    Ok(count as u64)
}
