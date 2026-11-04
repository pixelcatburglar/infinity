//! Active character names for an account from `game.characters`, ordered by character ID.
//! This DAO has no direct client mirror or wire contract; the gateway supplies its response contract.
//! Transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
pub(crate) struct CharacterName { pub character_id: i64, pub name: String }
impl CharacterName { fn from_row(r: &Row) -> Self { Self { character_id: r.get("character_id"), name: r.get("name") } } }
struct Ownership { owned: bool }
impl Ownership { fn from_row(r: &Row) -> Self { Self { owned: r.get("owned") } } }
pub(crate) async fn names(ex: &(impl Executor + ?Sized), user: i64) -> Result<Vec<CharacterName>, db::Error> {
    Ok(ex.query("SELECT character_id, name FROM game.characters WHERE user_id = $1 AND NOT deleted ORDER BY character_id", &[&user]).await?.iter().map(CharacterName::from_row).collect())
}
pub(crate) async fn owned(ex: &(impl Executor + ?Sized), user: i64, character: i64) -> Result<bool, db::Error> {
    Ok(Ownership::from_row(&ex.query_one("SELECT EXISTS (SELECT 1 FROM game.characters WHERE user_id = $1 AND character_id = $2 AND NOT deleted) AS owned", &[&user, &character]).await?).owned)
}
