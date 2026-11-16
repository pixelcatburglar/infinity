//! Ship location, station relocation and home station persistence from
//! game.characters. Ship services retain client behavior, transactions and
//! notifications.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
struct HomeStation { station_id: Option<i64> }
impl HomeStation { fn from_row(r: &Row) -> Self { Self { station_id: r.get("station_id") } } }
pub(crate) struct Location { pub(crate) corporation: i64, pub(crate) alliance: Option<i64>, pub(crate) station: Option<i64>, pub(crate) system: Option<i64>, pub(crate) ship: Option<i64> }
impl Location {
    fn from_row(row: &Row) -> Self { Self { corporation: row.get("corporation_id"), alliance: row.get("alliance_id"), station: row.get("station_id"), system: row.get("solar_system_id"), ship: row.get("ship_id") } }
}
pub(crate) async fn current(ex: &(impl Executor + ?Sized), me: i64) -> Result<Option<Location>, db::Error> {
    Ok(ex.query_opt("SELECT ch.corporation_id,ch.station_id,ch.solar_system_id,ch.ship_id,(SELECT alliance_id FROM game.corporations co WHERE co.corporation_id=ch.corporation_id AND NOT co.deleted) AS alliance_id FROM game.characters ch WHERE ch.character_id=$1 AND NOT ch.deleted", &[&me]).await?.as_ref().map(Location::from_row))
}
pub(crate) async fn undock(ex:&(impl Executor+?Sized), me:i64, system:i64)->Result<u64,db::Error> { ex.execute("UPDATE game.characters SET station_id = NULL, solar_system_id = $2 WHERE character_id = $1", &[&me,&system]).await }
/// Lock the character's location row FOR UPDATE, matching the exact source
/// tuple; `user` additionally binds the account when present.
pub(crate) async fn lock_location(ex:&(impl Executor+?Sized), character:i64, user:Option<i64>, ship:i64, station:Option<i64>, system:i64)->Result<bool,db::Error> { Ok(ex.query_opt("SELECT character_id FROM game.characters WHERE character_id = $1 AND ($2::bigint IS NULL OR user_id = $2) AND ship_id = $3 AND NOT deleted AND station_id IS NOT DISTINCT FROM $4 AND solar_system_id = $5 FOR UPDATE", &[&character,&user,&ship,&station,&system]).await?.is_some()) }
/// Write the four location columns while re-validating the active ship and
/// source tuple under the lock; false means the row no longer matches.
pub(crate) async fn relocate(ex:&(impl Executor+?Sized), character:i64, ship:i64, source_station:Option<i64>, source_system:i64, station:Option<i64>, system:i64, constellation:i64, region:i64)->Result<bool,db::Error> { Ok(ex.execute("UPDATE game.characters SET station_id = $5, solar_system_id = $6, constellation_id = $7, region_id = $8 WHERE character_id = $1 AND ship_id = $2 AND NOT deleted AND station_id IS NOT DISTINCT FROM $3 AND solar_system_id = $4", &[&character,&ship,&source_station,&source_system,&station,&system,&constellation,&region]).await? == 1) }
pub(crate) async fn location(ex:&(impl Executor+?Sized), me:i64, system:i64, constellation:i64, region:i64)->Result<u64,db::Error> { ex.execute("UPDATE game.characters SET solar_system_id = $2, constellation_id = $3, region_id = $4 WHERE character_id = $1", &[&me,&system,&constellation,&region]).await }
pub(crate) async fn home_station(ex:&(impl Executor+?Sized), me:i64)->Result<Option<i64>,db::Error> { Ok(HomeStation::from_row(&ex.query_one("SELECT COALESCE(home_station_id, station_id) AS station_id FROM game.characters WHERE character_id = $1", &[&me]).await?).station_id) }
/// Write the five location columns while re-validating the character, its
/// active ship and the previous station under the character lock; false means
/// the row no longer matches.
pub(crate) async fn set_home(ex:&(impl Executor+?Sized), me:i64, expected_ship:i64, expected_station:Option<i64>, capsule:i64, station:i64, system:i64, constellation:i64, region:i64)->Result<bool,db::Error> { Ok(ex.execute("UPDATE game.characters SET ship_id = $3, station_id = $4, solar_system_id = $5, constellation_id = $6, region_id = $7 WHERE character_id = $1 AND ship_id = $2 AND NOT deleted AND station_id IS NOT DISTINCT FROM $8", &[&me,&expected_ship,&capsule,&station,&system,&constellation,&region,&expected_station]).await? == 1) }
