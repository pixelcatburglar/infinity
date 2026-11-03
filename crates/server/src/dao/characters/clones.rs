//! Jump clone and authoritative character persistence in game tables.
//! Client policy, transactions and notifications belong to jumpCloneSvc callers.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct CloneRow { pub clone_id: i64, pub owner_id: i64, pub location_id: i64, pub name: String }
impl CloneRow { fn from_row(r: &Row) -> Self { Self { clone_id: r.get("clone_id"), owner_id: r.get("owner_id"), location_id: r.get("location_id"), name: r.get("name") } } }
pub(crate) struct CharacterState { pub ship_id: Option<i64>, pub station_id: Option<i64>, pub last_clone_jump: Option<i64> }
impl CharacterState { fn from_row(r: &Row) -> Self { Self { ship_id: r.get("ship_id"), station_id: r.get("station_id"), last_clone_jump: r.get("last_clone_jump") } } }
/// Caller holds the transaction for all subsequent clone and item locks.
pub(crate) async fn lock_character(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Option<CharacterState>, db::Error> {
    Ok(ex.query_opt("SELECT ship_id, station_id, last_clone_jump FROM game.characters WHERE character_id=$1 FOR UPDATE", &[&owner]).await?.as_ref().map(CharacterState::from_row))
}
pub(crate) async fn clones(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<CloneRow>, db::Error> {
    Ok(ex.query("SELECT clone_id, owner_id, location_id, name FROM game.jump_clones WHERE owner_id=$1 ORDER BY clone_id", &[&owner]).await?.iter().map(CloneRow::from_row).collect())
}
pub(crate) async fn lock_clones(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<CloneRow>, db::Error> {
    Ok(ex.query("SELECT clone_id, owner_id, location_id, name FROM game.jump_clones WHERE owner_id=$1 ORDER BY clone_id FOR UPDATE", &[&owner]).await?.iter().map(CloneRow::from_row).collect())
}
pub(crate) async fn create(ex: &(impl Executor + ?Sized), owner: i64, location: i64) -> Result<CloneRow, db::Error> {
    Ok(CloneRow::from_row(&ex.query_one("INSERT INTO game.jump_clones (owner_id, location_id) VALUES ($1,$2) RETURNING clone_id, owner_id, location_id, name", &[&owner, &location]).await?))
}
pub(crate) async fn destroy(ex: &(impl Executor + ?Sized), clone_id: i64, owner: i64) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.jump_clones WHERE clone_id=$1 AND owner_id=$2", &[&clone_id, &owner]).await
}
pub(crate) async fn rename(ex: &(impl Executor + ?Sized), clone_id: i64, owner: i64, name: &str) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.jump_clones SET name=$3 WHERE clone_id=$1 AND owner_id=$2", &[&clone_id, &owner, &name]).await
}
pub(crate) async fn jump(ex: &(impl Executor + ?Sized), owner: i64, capsule: i64, station: i64, system: i64, constellation: i64, region: i64, expected_ship: i64, expected_station: i64, now: Option<i64>) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.characters SET ship_id=$2, station_id=$3, solar_system_id=$4, constellation_id=$5, region_id=$6, last_clone_jump=COALESCE($7,last_clone_jump) WHERE character_id=$1 AND ship_id=$8 AND station_id=$9", &[&owner, &capsule, &station, &system, &constellation, &region, &now, &expected_ship, &expected_station]).await
}
pub(crate) async fn reset_last_jump(ex: &(impl Executor + ?Sized), owner: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.characters SET last_clone_jump=NULL WHERE character_id=$1", &[&owner]).await
}
pub(crate) async fn defer_location(ex: &(impl Executor + ?Sized)) -> Result<(), db::Error> {
    ex.execute("SET CONSTRAINTS game.jump_clones_owner_location_key DEFERRED", &[]).await?; Ok(())
}
pub(crate) async fn restore_location(ex: &(impl Executor + ?Sized)) -> Result<(), db::Error> {
    ex.execute("SET CONSTRAINTS game.jump_clones_owner_location_key IMMEDIATE", &[]).await?; Ok(())
}
