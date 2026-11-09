//! Existing Entosis source generations and immutable invalidation times; no organization cache.
use crate::db::{self,executor::Executor};
#[derive(Clone,Debug,PartialEq,Eq)]
pub(crate) struct SourceVersion { pub character_id:i64, pub revision:i64 }
impl SourceVersion { fn from_row(r:&tokio_postgres::Row)->Self { Self { character_id:r.get("character_id"),revision:r.get("revision") } } }
#[derive(Clone,Debug,PartialEq,Eq)]
pub(crate) struct SourceChange { pub character_id:i64, pub revision:i64, pub effective_at:i64 }
impl SourceChange { fn from_row(r:&tokio_postgres::Row)->Self { Self { character_id:r.get("character_id"),revision:r.get("revision"),effective_at:r.get("effective_at") } } }
/// Called only after canonical admission; lock lifetime requires a transaction.
pub(crate) async fn ensure(ex:&(impl Executor+?Sized), character:i64)->Result<SourceVersion,db::Error> {
    ex.execute("INSERT INTO game.sov_link_sources (character_id) VALUES ($1) ON CONFLICT DO NOTHING",&[&character]).await?;
    Ok(SourceVersion::from_row(&ex.query_one("SELECT character_id,revision FROM game.sov_link_sources WHERE character_id=$1 FOR UPDATE",&[&character]).await?))
}
pub(crate) async fn versions(ex:&(impl Executor+?Sized), characters:&[i64])->Result<Vec<SourceVersion>,db::Error> {
    Ok(ex.query("SELECT character_id,revision FROM game.sov_link_sources WHERE character_id=ANY($1) ORDER BY character_id",&[&characters]).await?.iter().map(SourceVersion::from_row).collect())
}
/// Enumerate only sources that have actually been activated, in lock order.
pub(crate) async fn all_character_ids(ex:&(impl Executor+?Sized))->Result<Vec<i64>,db::Error> {
    Ok(ex.query("SELECT character_id FROM game.sov_link_sources ORDER BY character_id",&[]).await?.iter().map(|r|r.get("character_id")).collect())
}
/// Locks existing sources in character order; update and history append are one statement.
pub(crate) async fn invalidate(ex:&(impl Executor+?Sized), characters:&[i64], effective_at:i64)->Result<Vec<SourceChange>,db::Error> {
    Ok(ex.query("WITH locked AS MATERIALIZED (SELECT character_id FROM game.sov_link_sources WHERE character_id=ANY($1) ORDER BY character_id FOR UPDATE), updated AS (UPDATE game.sov_link_sources s SET revision=s.revision+1 FROM locked WHERE s.character_id=locked.character_id RETURNING s.character_id,s.revision), inserted AS (INSERT INTO game.sov_link_source_changes (character_id,revision,effective_at) SELECT character_id,revision,$2 FROM updated RETURNING character_id,revision,effective_at) SELECT character_id,revision,effective_at FROM inserted ORDER BY character_id",&[&characters,&effective_at]).await?.iter().map(SourceChange::from_row).collect())
}
/// Earliest committed cutoff after the frozen generation, with the highest invalidation watermark.
pub(crate) async fn first_change_after(ex:&(impl Executor+?Sized), character:i64, revision:i64)->Result<Option<SourceChange>,db::Error> {
    Ok(ex.query_opt("SELECT character_id,MAX(revision) AS revision,MIN(effective_at) AS effective_at FROM game.sov_link_source_changes WHERE character_id=$1 AND revision>$2 GROUP BY character_id",&[&character,&revision]).await?.map(|r|SourceChange::from_row(&r)))
}
