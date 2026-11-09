//! Ship location and home station persistence from game.characters.
//! Ship services retain client behavior, transactions and notifications.
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
pub(crate) async fn dock(ex:&(impl Executor+?Sized), me:i64, station:i64, system:i64)->Result<u64,db::Error> { ex.execute("UPDATE game.characters SET station_id = $2, solar_system_id = $3 WHERE character_id = $1", &[&me,&station,&system]).await }
pub(crate) async fn location(ex:&(impl Executor+?Sized), me:i64, system:i64, constellation:i64, region:i64)->Result<u64,db::Error> { ex.execute("UPDATE game.characters SET solar_system_id = $2, constellation_id = $3, region_id = $4 WHERE character_id = $1", &[&me,&system,&constellation,&region]).await }
pub(crate) async fn home_station(ex:&(impl Executor+?Sized), me:i64)->Result<Option<i64>,db::Error> { Ok(HomeStation::from_row(&ex.query_one("SELECT COALESCE(home_station_id, station_id) AS station_id FROM game.characters WHERE character_id = $1", &[&me]).await?).station_id) }
pub(crate) async fn set_home(ex:&(impl Executor+?Sized), me:i64, capsule:i64, station:i64, system:i64, constellation:i64, region:i64)->Result<u64,db::Error> { ex.execute("UPDATE game.characters SET ship_id = $2, station_id = $3, solar_system_id = $4, constellation_id = $5, region_id = $6 WHERE character_id = $1", &[&me,&capsule,&station,&system,&constellation,&region]).await }
