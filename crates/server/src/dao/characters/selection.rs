//! Character selection, ship association, logout evidence and deletion persistence in `game.characters`,
//! `game.items`, `game.corporations` and, through the shared role expression, `game.corp_titles`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Session policy, transactions, refusals, notifications and World are caller concerns.
//! Stored ISK is exact numeric; the selection balance is a lossy clamped client projection.

use crate::isk::{IskAmount, personal_client_cap};
use crate::dao::wallets::WalletError;
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

use super::roles::{self, Authority};

pub(crate) struct CharacterRow {
    pub character_id: i64, pub name: String, pub corporation_id: i64, pub station_id: Option<i64>,
    pub solar_system_id: Option<i64>, pub balance: f64, pub skill_points: i64, pub logoff_date: i64,
    pub gender_id: i32, pub bloodline_id: i32, pub race_id: i32, pub ship_type_id: Option<i32>, pub delete_prepare_at: Option<i64>,
}
impl CharacterRow {
    fn from_row(r: &Row) -> Result<Self, WalletError> { Ok(Self {
        character_id: r.get("character_id"), name: r.get("name"), corporation_id: r.get("corporation_id"), station_id: r.get("station_id"),
        solar_system_id: r.get("solar_system_id"), balance: IskAmount::parse_decimal(r.get("balance"))?.client_isk(personal_client_cap()), skill_points: r.get("skill_points"), logoff_date: r.get("logoff_date"),
        gender_id: r.get("gender_id"), bloodline_id: r.get("bloodline_id"), race_id: r.get("race_id"), ship_type_id: r.get("ship_type_id"), delete_prepare_at: r.get("delete_prepare_at"),
    }) }
}
pub(crate) async fn characters(ex: &(impl Executor + ?Sized), user_id: i64, second: i64, epoch: i64) -> Result<Vec<CharacterRow>, WalletError> {
    let rows = ex.query("SELECT ch.character_id, ch.name, ch.corporation_id, ch.station_id, ch.solar_system_id, ch.balance::text AS balance, ch.skill_points,
                    (extract(epoch FROM ch.logoff_at) * $2::bigint)::bigint + $3::bigint AS logoff_date,
                    ch.gender_id, ch.bloodline_id, ch.race_id, ship.type_id AS ship_type_id, ch.delete_prepare_at
             FROM game.characters ch LEFT JOIN game.items ship ON ship.item_id = ch.ship_id
             WHERE ch.user_id = $1 AND NOT ch.deleted ORDER BY ch.character_id", &[&user_id, &second, &epoch]).await?;
    rows.iter().map(CharacterRow::from_row).collect()
}
pub(crate) struct SelectedCharacter {
    pub corporation_id: i64, pub station_id: Option<i64>, pub solar_system_id: i64, pub constellation_id: i64, pub region_id: i64,
    pub gender_id: i32, pub bloodline_id: i32, pub race_id: i32, pub name: String, pub roles: i64, pub alliance_id: Option<i64>,
    pub authority: Authority,
}
impl SelectedCharacter {
    fn from_row(r: &Row) -> Self { Self { corporation_id: r.get("corporation_id"), station_id: r.get("station_id"), solar_system_id: r.get("solar_system_id"),
        constellation_id: r.get("constellation_id"), region_id: r.get("region_id"), gender_id: r.get("gender_id"), bloodline_id: r.get("bloodline_id"),
        race_id: r.get("race_id"), name: r.get("name"), roles: r.get("roles"), alliance_id: r.get("alliance_id"), authority: Authority::from_row(r) } }
}
pub(crate) async fn selected(ex: &(impl Executor + ?Sized), character_id: i64, user_id: i64) -> Result<Option<SelectedCharacter>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let authority = roles::projection("ch.character_id=$1");
        format!("WITH authority AS ({authority})
        SELECT authority.*, ch.station_id, ch.solar_system_id, ch.constellation_id, ch.region_id, ch.gender_id, ch.bloodline_id, ch.race_id, ch.name,
               (SELECT alliance_id FROM game.corporations co WHERE co.corporation_id=ch.corporation_id) AS alliance_id
        FROM game.characters ch JOIN authority ON authority.character_id=ch.character_id
        WHERE ch.character_id=$1 AND ch.user_id=$2 AND NOT ch.deleted")
    });
    Ok(ex.query_opt(SQL.as_str(), &[&character_id, &user_id]).await?.as_ref().map(SelectedCharacter::from_row))
}
pub(crate) struct SpaceShip { pub item_id: i64, pub x: Option<f64>, pub y: Option<f64>, pub z: Option<f64> }
impl SpaceShip { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id"), x: r.get("x"), y: r.get("y"), z: r.get("z") } } }
pub(crate) async fn space_ship(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<Option<SpaceShip>, db::Error> {
    Ok(ex.query_opt("SELECT i.item_id, i.x, i.y, i.z FROM game.characters ch JOIN game.items i ON i.item_id = ch.ship_id
             WHERE ch.character_id = $1 AND i.location_id = ch.solar_system_id AND i.x IS NOT NULL", &[&character_id]).await?.as_ref().map(SpaceShip::from_row))
}
/// Lock the character row FOR UPDATE to serialize per-character mutations.
pub(crate) async fn lock_character(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<(), db::Error> {
    ex.query_one("SELECT character_id FROM game.characters WHERE character_id = $1 FOR UPDATE", &[&character_id]).await?;
    Ok(())
}
struct ShipId { ship_id: i64 }
impl ShipId { fn from_row(r: &Row) -> Self { Self { ship_id: r.get("ship_id") } } }
pub(crate) async fn ship_id(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<Option<i64>, db::Error> {
    Ok(ex.query_opt("SELECT ship_id FROM game.characters ch JOIN game.items i ON i.item_id = ch.ship_id WHERE ch.character_id = $1", &[&character_id]).await?.as_ref().map(|r| ShipId::from_row(r).ship_id))
}
pub(crate) async fn set_ship(ex: &(impl Executor + ?Sized), character_id: i64, ship_id: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.characters SET ship_id = $2 WHERE character_id = $1", &[&character_id, &ship_id]).await
}
pub(crate) async fn logon(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.characters SET observed_logon_at = now() WHERE character_id = $1", &[&character_id]).await
}
pub(crate) async fn logoff(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.characters SET logoff_at = now(), observed_logoff_at = now() WHERE character_id = $1", &[&character_id]).await
}

/// Exact durable logout timestamps for ambiguous checkpoint reconciliation.
pub(crate) async fn logout_timestamps(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<(String, Option<String>), db::Error> {
    let row = ex.query_one("SELECT logoff_at::text AS logoff_at, observed_logoff_at::text AS observed_logoff_at FROM game.characters WHERE character_id = $1", &[&character_id]).await?;
    Ok((row.get("logoff_at"), row.get("observed_logoff_at")))
}
pub(crate) async fn owns(ex: &(impl Executor + ?Sized), character_id: i64, user_id: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.characters WHERE character_id = $1 AND user_id = $2 AND NOT deleted", &[&character_id, &user_id]).await?.is_some())
}
struct Count { count: i64 }
impl Count { fn from_row(r: &Row) -> Self { Self { count: r.get("count") } } }
pub(crate) async fn count(ex: &(impl Executor + ?Sized), user_id: i64) -> Result<i64, db::Error> {
    Ok(Count::from_row(&ex.query_one("SELECT count(*) FROM game.characters WHERE user_id = $1 AND NOT deleted", &[&user_id]).await?).count)
}
pub(crate) async fn prepare_delete(ex: &(impl Executor + ?Sized), character_id: i64, when: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.characters SET delete_prepare_at = $2 WHERE character_id = $1", &[&character_id, &when]).await
}
pub(crate) async fn cancel_delete(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.characters SET delete_prepare_at = NULL WHERE character_id = $1", &[&character_id]).await
}
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), character_id: i64, now: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.characters SET deleted = true WHERE character_id = $1 AND delete_prepare_at IS NOT NULL AND delete_prepare_at <= $2", &[&character_id, &now]).await
}
