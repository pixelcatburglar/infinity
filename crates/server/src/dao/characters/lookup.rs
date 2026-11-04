//! Active character identity lookup for GM commands and `lookupSvc`.
//! The slash and owner search services supply the client wire contracts.
//! Reads `game.characters`; deleted characters are excluded.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

struct CharacterId { id: i64 }
impl CharacterId { fn from_row(row: &Row) -> Self { Self { id: row.get("character_id") } } }

pub(crate) async fn find(ex: &(impl Executor + ?Sized), text: &str, id: Option<i64>) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT character_id FROM game.characters WHERE NOT deleted AND (character_id = $2 OR ($2::bigint IS NULL AND lower(name) = lower($1))) ORDER BY character_id LIMIT 1", &[&text, &id]).await?.as_ref().map(|row| CharacterId::from_row(row).id))
}

pub(crate) struct Candidate { pub id: i64, pub name: String }
impl Candidate {
    fn from_row(row: &Row) -> Self { Self { id: row.get("character_id"), name: row.get("name") } }
}

pub(crate) async fn search(ex: &(impl Executor + ?Sized), text: &str, exact: bool) -> Result<Vec<Candidate>, Error> {
    Ok(ex.query("SELECT character_id, name FROM game.characters WHERE NOT deleted AND CASE WHEN $2 THEN lower(name) = lower($1) ELSE position(lower($1) in lower(name)) > 0 END ORDER BY name, character_id", &[&text, &exact]).await?.iter().map(Candidate::from_row).collect())
}
