//! Corporation membership and role audit persistence.
//! `corp_ui_member_auditing.py` reads events and role changes from these tables.
//! Only committed production events are recorded; no historical backfill is synthesized.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct Event { pub(crate) time: i64, pub(crate) corporation: i64, pub(crate) kind: i32 }
impl Event { fn from_row(r: &Row) -> Self { Self { time: r.get("event_time"), corporation: r.get("corporation_id"), kind: r.get("event_type_id") } } }
pub(crate) struct RoleChange { pub(crate) time: i64, pub(crate) corporation: i64, pub(crate) character: i64, pub(crate) issuer: i64, pub(crate) old: i64, pub(crate) new: i64, pub(crate) grantable: bool }
impl RoleChange { fn from_row(r: &Row) -> Self { Self { time: r.get("change_time"), corporation: r.get("corporation_id"), character: r.get("character_id"), issuer: r.get("issuer_id"), old: r.get("old_roles"), new: r.get("new_roles"), grantable: r.get("grantable") } } }
pub(crate) async fn event(ex: &(impl Executor + ?Sized), character: i64, issuer: i64, corporation: i64, kind: i32, time: i64) -> Result<(), Error> {
    ex.execute("INSERT INTO game.corp_member_events(character_id,issuer_id,corporation_id,event_type_id,event_time) VALUES ($1,$2,$3,$4,$5)", &[&character,&issuer,&corporation,&kind,&time]).await?; Ok(())
}
pub(crate) async fn role(ex: &(impl Executor + ?Sized), character: i64, corporation: i64, issuer: i64, time: i64, old: i64, new: i64, grantable: bool, scope: i16) -> Result<(), Error> {
    if old != new { ex.execute("INSERT INTO game.corp_role_history(character_id,corporation_id,issuer_id,change_time,old_roles,new_roles,grantable,scope) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)", &[&character,&corporation,&issuer,&time,&old,&new,&grantable,&scope]).await?; } Ok(())
}
pub(crate) async fn events(ex: &(impl Executor + ?Sized), corp: i64, character: i64, from: i64, to: i64, limit: i64) -> Result<Vec<Event>, Error> {
    Ok(ex.query("SELECT event_time,corporation_id,event_type_id FROM game.corp_member_events WHERE corporation_id=$1 AND character_id=$2 AND event_time >= $3 AND event_time < $4 ORDER BY event_time DESC,event_id DESC LIMIT $5", &[&corp,&character,&from,&to,&limit]).await?.iter().map(Event::from_row).collect())
}
pub(crate) async fn roles(ex: &(impl Executor + ?Sized), corp: i64, character: i64, from: i64, to: i64, limit: i64) -> Result<Vec<RoleChange>, Error> {
    Ok(ex.query("SELECT change_time,corporation_id,character_id,issuer_id,old_roles,new_roles,grantable FROM game.corp_role_history WHERE corporation_id=$1 AND character_id=$2 AND change_time >= $3 AND change_time < $4 ORDER BY change_time DESC,history_id DESC LIMIT $5", &[&corp,&character,&from,&to,&limit]).await?.iter().map(RoleChange::from_row).collect())
}
pub(crate) async fn target_visible(ex: &(impl Executor + ?Sized), corp: i64, character: i64) -> Result<bool, Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.characters WHERE corporation_id=$1 AND character_id=$2 UNION ALL SELECT 1 FROM game.employment_history WHERE corporation_id=$1 AND char_id=$2 UNION ALL SELECT 1 FROM game.corp_member_events WHERE corporation_id=$1 AND character_id=$2 UNION ALL SELECT 1 FROM game.corp_role_history WHERE corporation_id=$1 AND character_id=$2 LIMIT 1", &[&corp,&character]).await?.is_some())
}
