//! Character identity, paperdolls, notes and cfg lookups for `charMgr`,
//! `paperDollServer` and `eve/common/script/sys/eveCfg.py`.
//! Uses `game.characters`, `game.char_notes`, `game.items`, `game.corporations`
//! and `game.alliances`. Static enrichment and wire values remain with services;
//! transactions and notifications are not handled here.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct PublicInfo {
    pub name: String,
    pub corporation_id: i64,
    pub race_id: i32,
    pub bloodline_id: i32,
    pub ancestry_id: Option<i32>,
    pub school_id: Option<i32>,
    pub gender_id: i32,
    pub created: i64,
}
impl PublicInfo {
    fn from_row(r: &Row) -> Self {
        Self {
            name: r.get("name"),
            corporation_id: r.get("corporation_id"),
            race_id: r.get("race_id"),
            bloodline_id: r.get("bloodline_id"),
            ancestry_id: r.get("ancestry_id"),
            school_id: r.get("school_id"),
            gender_id: r.get("gender_id"),
            created: r.get("created"),
        }
    }
}
pub(crate) async fn public_info(ex: &(impl Executor + ?Sized), id: i64, second: i64, epoch: i64) -> Result<Option<PublicInfo>, Error> {
    let row = ex.query_opt(
        "SELECT name, corporation_id, race_id, bloodline_id, ancestry_id, school_id, gender_id,\n                    (extract(epoch FROM created_at) * $2::bigint)::bigint + $3::bigint AS created\n             FROM game.characters WHERE character_id = $1",
        &[&id, &second, &epoch],
    ).await?;
    Ok(row.as_ref().map(PublicInfo::from_row))
}
#[derive(Clone, Copy)]
pub(crate) enum DollColumn {
    Appearance,
    Portrait,
}
impl DollColumn {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Portrait => "portrait",
        }
    }
}
struct DollBlob {
    blob: Option<Vec<u8>>,
}
impl DollBlob {
    fn from_row(r: &Row, column: DollColumn) -> Self {
        Self { blob: r.get(column.name()) }
    }
}
pub(crate) async fn doll_blob(ex: &impl Executor, id: i64, column: DollColumn) -> Result<Option<Vec<u8>>, Error> {
    let selected = column;
    let column = column.name();
    let row = ex.query_opt(&format!("SELECT {column} FROM game.characters WHERE character_id = $1"), &[&id]).await?;
    Ok(row.as_ref().and_then(|r| DollBlob::from_row(r, selected).blob))
}
pub(crate) async fn store_doll(ex: &impl Executor, id: i64, user: i64, appearance: &[u8], portrait: &[u8]) -> Result<u64, Error> {
    ex.execute(
        "UPDATE game.characters SET appearance = $3, portrait = $4 WHERE character_id = $1 AND user_id = $2",
        &[&id, &user, &appearance, &portrait],
    ).await
}
struct HomeStationId { home_station_id: Option<i64> }
impl HomeStationId {
    fn from_row(r: &Row) -> Self { Self { home_station_id: r.get("home_station_id") } }
}
pub(crate) async fn home_station_id(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<i64>, Error> {
    let row = ex.query_opt("SELECT home_station_id FROM game.characters WHERE character_id = $1", &[&id]).await?;
    Ok(row.as_ref().and_then(|r| HomeStationId::from_row(r).home_station_id))
}
struct Note { note: String }
impl Note {
    fn from_row(r: &Row) -> Self { Self { note: r.get("note") } }
}
pub(crate) async fn note(ex: &impl Executor, char_id: i64, item: i64) -> Result<Option<String>, Error> {
    let row = ex.query_opt("SELECT note FROM game.char_notes WHERE char_id = $1 AND item_id = $2", &[&char_id, &item]).await?;
    Ok(row.as_ref().map(|r| Note::from_row(r).note))
}
pub(crate) async fn delete_note(ex: &impl Executor, char_id: i64, item: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.char_notes WHERE char_id = $1 AND item_id = $2", &[&char_id, &item]).await
}
pub(crate) async fn set_note(ex: &impl Executor, char_id: i64, item: i64, note: &str) -> Result<u64, Error> {
    ex.execute(
        "INSERT INTO game.char_notes (char_id, item_id, note) VALUES ($1, $2, $3)\n                         ON CONFLICT (char_id, item_id) DO UPDATE SET note = $3",
        &[&char_id, &item, &note],
    ).await
}

pub(crate) struct Owner {
    pub id: i64,
    pub name: Option<String>,
    pub type_id: i64,
    pub gender: Option<i32>,
    pub name_id: Option<i64>,
}
impl Owner {
    fn from_row(r: &Row) -> Self {
        Self {
            id: r.get("character_id"),
            name: r.get("name"),
            type_id: r.get("type_id"),
            gender: r.get("gender_id"),
            name_id: r.get("name_id"),
        }
    }
}
pub(crate) async fn owners(ex: &(impl Executor + ?Sized), ids: &[i64], character: i64, corporation: i64, alliance: i64) -> Result<Vec<Owner>, Error> {
    let rows = ex.query(
        "SELECT character_id, name, $2::bigint AS type_id, gender_id, NULL::bigint AS name_id FROM game.characters WHERE character_id = ANY($1::bigint[])\n             UNION ALL SELECT corporation_id, name, $3::bigint, 0, NULL FROM game.corporations WHERE corporation_id = ANY($1::bigint[])\n             UNION ALL SELECT alliance_id, name, $4::bigint, 0, NULL FROM game.alliances WHERE alliance_id = ANY($1::bigint[])",
        &[&ids, &character, &corporation, &alliance],
    ).await?;
    Ok(rows.iter().map(Owner::from_row).collect())
}
pub(crate) struct Location {
    pub id: i64,
    pub name: Option<String>,
    pub type_id: i32,
}
impl Location {
    fn from_row(r: &Row) -> Self {
        Self {
            id: r.get("item_id"),
            name: r.get("name"),
            type_id: r.get("type_id"),
        }
    }
}
pub(crate) async fn locations(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<Location>, Error> {
    let rows = ex.query(
        "SELECT item_id, name, type_id FROM game.items WHERE item_id = ANY($1::bigint[])",
        &[&ids],
    ).await?;
    Ok(rows.iter().map(Location::from_row).collect())
}
pub(crate) struct Ticker {
    pub id: i64,
    pub ticker: Option<String>,
}
impl Ticker {
    fn from_row(r: &Row) -> Self {
        Self { id: r.get("corporation_id"), ticker: r.get("ticker") }
    }
}
pub(crate) async fn tickers(ex: &impl Executor, ids: &[i64]) -> Result<Vec<Ticker>, Error> {
    let rows = ex.query(
        "SELECT corporation_id, ticker FROM game.corporations WHERE corporation_id = ANY($1::bigint[])",
        &[&ids],
    ).await?;
    Ok(rows.iter().map(Ticker::from_row).collect())
}
pub(crate) struct Alliance {
    pub id: i64,
    pub name: String,
}
impl Alliance {
    fn from_row(r: &Row) -> Self {
        Self { id: r.get("alliance_id"), name: r.get("short_name") }
    }
}
pub(crate) async fn alliances(ex: &impl Executor, ids: &[i64]) -> Result<Vec<Alliance>, Error> {
    let rows = ex.query("SELECT alliance_id, short_name FROM game.alliances WHERE alliance_id = ANY($1::bigint[])", &[&ids]).await?;
    Ok(rows.iter().map(Alliance::from_row).collect())
}
