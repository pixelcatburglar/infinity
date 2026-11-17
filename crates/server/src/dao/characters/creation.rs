//! Character name lookup and creation in `game.characters`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Name validation, starting location, skill grants, transactions, refusals, notifications and World are caller concerns.

use crate::db::{self, executor::Executor};

pub(crate) async fn name_taken(ex: &(impl Executor + ?Sized), name: &str) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.characters WHERE lower(name) = lower($1) AND NOT deleted", &[&name]).await?.is_some())
}
pub(crate) struct NewCharacter<'a> {
    pub user_id: i64, pub name: &'a str, pub race_id: i32, pub bloodline_id: i32, pub gender_id: i32,
    pub ancestry_id: Option<i32>, pub school_id: Option<i32>, pub corporation_id: i64, pub station_id: i64,
    pub solar_system_id: i64, pub constellation_id: i64, pub region_id: i64, pub appearance: &'a [u8], pub portrait: &'a [u8], pub balance: f64,
}
pub(crate) async fn create(ex: &(impl Executor + ?Sized), c: &NewCharacter<'_>) -> Result<i64, db::Error> {
    let row = ex.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, ancestry_id, school_id,
                         corporation_id, station_id, solar_system_id, constellation_id, region_id, balance, appearance, portrait, home_station_id)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $15::float8::numeric, $13, $14, $9) RETURNING character_id",
        &[&c.user_id, &c.name, &c.race_id, &c.bloodline_id, &c.gender_id, &c.ancestry_id, &c.school_id, &c.corporation_id, &c.station_id,
          &c.solar_system_id, &c.constellation_id, &c.region_id, &c.appearance, &c.portrait, &c.balance]).await?;
    Ok(row.get::<_, i64>("character_id"))
}
