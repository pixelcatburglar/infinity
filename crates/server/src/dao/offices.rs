//! Office persistence for officeManager (eve/client/script/ui/shared/neocom/corporation).
//! Uses game.offices and game.items. Rental policy, static station data, notifications
//! and impounding are not implemented here.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
struct OfficeId { value: i64 }
impl OfficeId { fn from_row(r: &Row) -> Self { Self { value: r.get("office_id") } } }
struct CorporationId { value: i64 }
impl CorporationId { fn from_row(r: &Row) -> Self { Self { value: r.get("corporation_id") } } }
struct Count { value: i64 }
impl Count { fn from_row(r: &Row) -> Self { Self { value: r.get("count") } } }
pub(crate) struct Office { pub office_id: i64, pub station_id: i64, pub rented_until: i64 }
impl Office { fn from_row(r: &Row) -> Self { Self { office_id: r.get("office_id"), station_id: r.get("station_id"), rented_until: r.get("rented_until") } } }
pub(crate) async fn at(ex: &(impl Executor + ?Sized), corp: i64, station: i64) -> Result<Option<i64>, Error> { Ok(ex.query_opt("SELECT office_id FROM game.offices WHERE corporation_id = $1 AND station_id = $2", &[&corp, &station]).await?.as_ref().map(|r| OfficeId::from_row(r).value)) }
pub(crate) async fn owner(ex: &(impl Executor + ?Sized), item: i64) -> Result<Option<i64>, Error> { Ok(ex.query_opt("SELECT corporation_id FROM game.offices WHERE office_id = $1", &[&item]).await?.as_ref().map(|r| CorporationId::from_row(r).value)) }
pub(crate) async fn station(ex: &(impl Executor + ?Sized), office: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT station_id FROM game.offices WHERE office_id=$1", &[&office]).await?.map(|row| row.get("station_id")))
}
pub(crate) async fn extend(ex: &(impl Executor + ?Sized), office: i64, now: i64, period: i64) -> Result<u64, Error> { ex.execute("UPDATE game.offices SET rented_until = GREATEST(rented_until, $2) + $3 WHERE office_id = $1", &[&office, &now, &period]).await }
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), office: i64, corp: i64, station: i64, until: i64) -> Result<u64, Error> { ex.execute("INSERT INTO game.offices (office_id, corporation_id, station_id, rented_until) VALUES ($1, $2, $3, $4)", &[&office, &corp, &station, &until]).await }
pub(crate) async fn list(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Vec<Office>, Error> { Ok(ex.query("SELECT o.office_id, o.station_id, o.rented_until FROM game.offices o WHERE o.corporation_id = $1 ORDER BY o.office_id", &[&corp]).await?.iter().map(Office::from_row).collect()) }
pub(crate) async fn corporations(ex: &(impl Executor + ?Sized), station: i64) -> Result<Vec<i64>, Error> { Ok(ex.query("SELECT corporation_id FROM game.offices WHERE station_id = $1", &[&station]).await?.iter().map(|r| CorporationId::from_row(r).value).collect()) }
pub(crate) async fn count(ex: &(impl Executor + ?Sized), station: i64) -> Result<i64, Error> { Ok(Count::from_row(&ex.query_one("SELECT count(*) FROM game.offices WHERE station_id = $1", &[&station]).await?).value) }
pub(crate) async fn has_items(ex: &(impl Executor + ?Sized), office: i64) -> Result<bool, Error> { Ok(ex.query_opt("SELECT 1 FROM game.items WHERE location_id = $1 LIMIT 1", &[&office]).await?.is_some()) }
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), office: i64) -> Result<u64, Error> { ex.execute("DELETE FROM game.offices WHERE office_id = $1", &[&office]).await }
