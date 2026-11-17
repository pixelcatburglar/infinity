//! Durable mutual-war lifecycle and invitations in `game.wars` and `game.war_invites`.
//! Mirrors `eve/client/script/ui/shared/neocom/war/warHistoryCont.py` and mutual invitations.
//! Callers own authority, transactions, jobs and notifications; allies and fees are unsupported.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State { Pending, Active, Cooldown, Ended }

#[derive(Clone, Debug)]
pub(crate) struct War {
    pub war_id: i64,
    pub declared_by: i64,
    pub against: i64,
    pub declared_at: i64,
    pub starts_at: i64,
    pub state: State,
    pub retracted_at: Option<i64>,
    pub retracted_by: Option<i64>,
    pub finishes_at: Option<i64>,
}

impl War {
    fn from_row(r: &Row) -> Self {
        let state = match r.get::<_, &str>("state") {
            "pending" => State::Pending,
            "active" => State::Active,
            "cooldown" => State::Cooldown,
            "ended" => State::Ended,
            other => unreachable!("invalid war state: {other}"),
        };
        Self { war_id: r.get("war_id"), declared_by: r.get("declared_by"), against: r.get("against"),
            declared_at: r.get("declared_at"), starts_at: r.get("starts_at"), state,
            retracted_at: r.get("retracted_at"), retracted_by: r.get("retracted_by"), finishes_at: r.get("finishes_at") }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Invite { pub from_owner: i64, pub to_owner: i64, pub sent_at: i64, pub expires_at: i64 }
impl Invite {
    fn from_row(r: &Row) -> Self {
        Self { from_owner: r.get("from_owner"), to_owner: r.get("to_owner"), sent_at: r.get("sent_at"), expires_at: r.get("expires_at") }
    }
}

/// Call inside a transaction before reading or mutating either owner's lifecycle.
pub(crate) async fn lock_owners(ex: &(impl Executor + ?Sized), owners: &[i64]) -> Result<(), db::Error> {
    let mut owners = owners.to_vec();
    owners.sort_unstable();
    owners.dedup();
    for owner in owners {
        ex.execute("INSERT INTO game.war_invite_settings (owner_id) VALUES ($1) ON CONFLICT DO NOTHING", &[&owner]).await?;
        ex.query_one("SELECT owner_id FROM game.war_invite_settings WHERE owner_id=$1 FOR UPDATE", &[&owner]).await?;
    }
    Ok(())
}

pub(crate) async fn get(ex: &(impl Executor + ?Sized), war_id: i64) -> Result<Option<War>, db::Error> {
    Ok(ex.query_opt("SELECT * FROM game.wars WHERE war_id=$1", &[&war_id]).await?.as_ref().map(War::from_row))
}
pub(crate) async fn by_owner(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<War>, db::Error> {
    Ok(ex.query("SELECT * FROM game.wars WHERE declared_by=$1 OR against=$1 ORDER BY war_id DESC", &[&owner]).await?.iter().map(War::from_row).collect())
}
pub(crate) async fn by_owners(ex: &(impl Executor + ?Sized), owners: &[i64]) -> Result<Vec<War>, db::Error> {
    Ok(ex.query("SELECT * FROM game.wars WHERE declared_by=ANY($1) OR against=ANY($1) ORDER BY war_id DESC", &[&owners]).await?.iter().map(War::from_row).collect())
}
pub(crate) async fn by_ids(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<War>, db::Error> {
    Ok(ex.query("SELECT * FROM game.wars WHERE war_id=ANY($1) ORDER BY war_id", &[&ids]).await?.iter().map(War::from_row).collect())
}
pub(crate) async fn top(ex: &(impl Executor + ?Sized), before: Option<i64>, limit: i64) -> Result<Vec<War>, db::Error> {
    Ok(ex.query("SELECT * FROM game.wars WHERE ($1::bigint IS NULL OR war_id < $1) ORDER BY war_id DESC LIMIT $2", &[&before, &limit]).await?.iter().map(War::from_row).collect())
}
pub(crate) async fn ongoing_pair(ex: &(impl Executor + ?Sized), a: i64, b: i64, now: i64) -> Result<Option<War>, db::Error> {
    Ok(ex.query_opt("SELECT * FROM game.wars WHERE LEAST(declared_by,against)=LEAST($1::bigint,$2::bigint) AND GREATEST(declared_by,against)=GREATEST($1::bigint,$2::bigint) AND state <> 'ended' AND (finishes_at IS NULL OR $3 < finishes_at)", &[&a, &b, &now]).await?.as_ref().map(War::from_row))
}
/// End overdue cooldowns under owner locks. Callers must notify for every returned change.
pub(crate) async fn settle_due_pair(ex: &(impl Executor + ?Sized), a: i64, b: i64, now: i64) -> Result<Vec<War>, db::Error> {
    Ok(ex.query("UPDATE game.wars SET state='ended' WHERE LEAST(declared_by,against)=LEAST($1::bigint,$2::bigint) AND GREATEST(declared_by,against)=GREATEST($1::bigint,$2::bigint) AND state='cooldown' AND finishes_at <= $3 RETURNING *", &[&a, &b, &now]).await?.iter().map(War::from_row).collect())
}
pub(crate) async fn create(ex: &(impl Executor + ?Sized), from: i64, to: i64, now: i64, starts_at: i64) -> Result<Option<War>, db::Error> {
    Ok(ex.query_opt("INSERT INTO game.wars (declared_by,against,declared_at,starts_at,state) SELECT $1,$2,$3,$4,'pending' WHERE $1::bigint <> $2 AND $4::bigint > $3 ON CONFLICT DO NOTHING RETURNING *", &[&from, &to, &now, &starts_at]).await?.as_ref().map(War::from_row))
}
pub(crate) async fn activate(ex: &(impl Executor + ?Sized), id: i64, expected_start: i64, now: i64) -> Result<Option<War>, db::Error> {
    Ok(ex.query_opt("UPDATE game.wars SET state='active' WHERE war_id=$1 AND state='pending' AND starts_at=$2 AND starts_at <= $3 RETURNING *", &[&id, &expected_start, &now]).await?.as_ref().map(War::from_row))
}
pub(crate) async fn retract(ex: &(impl Executor + ?Sized), id: i64, actor_owner: i64, now: i64, finishes: i64) -> Result<Option<War>, db::Error> {
    Ok(ex.query_opt("UPDATE game.wars SET state='cooldown', retracted_at=$3, retracted_by=$2, finishes_at=$4 WHERE war_id=$1 AND state IN ('pending','active') AND $2 IN (declared_by,against) AND $4::bigint > $3 RETURNING *", &[&id, &actor_owner, &now, &finishes]).await?.as_ref().map(War::from_row))
}
pub(crate) async fn finish(ex: &(impl Executor + ?Sized), id: i64, expected_finish: i64, now: i64) -> Result<Option<War>, db::Error> {
    Ok(ex.query_opt("UPDATE game.wars SET state='ended' WHERE war_id=$1 AND state='cooldown' AND finishes_at=$2 AND finishes_at <= $3 RETURNING *", &[&id, &expected_finish, &now]).await?.as_ref().map(War::from_row))
}
pub(crate) async fn active_by_owners(ex: &(impl Executor + ?Sized), owner_ids: &[i64], now: i64) -> Result<Vec<War>, db::Error> {
    Ok(ex.query("SELECT * FROM game.wars WHERE declared_by=ANY($1) AND against=ANY($1) AND state <> 'ended' AND starts_at < $2 AND (finishes_at IS NULL OR $2 < finishes_at) ORDER BY war_id", &[&owner_ids, &now]).await?.iter().map(War::from_row).collect())
}
pub(crate) async fn invite(ex: &(impl Executor + ?Sized), from: i64, to: i64, now: i64) -> Result<Option<Invite>, db::Error> {
    Ok(ex.query_opt("SELECT * FROM game.war_invites WHERE from_owner=$1 AND to_owner=$2 AND $3 < expires_at", &[&from, &to, &now]).await?.as_ref().map(Invite::from_row))
}
pub(crate) async fn invitations(ex: &(impl Executor + ?Sized), owner: i64, now: i64) -> Result<Vec<Invite>, db::Error> {
    Ok(ex.query("SELECT * FROM game.war_invites WHERE (from_owner=$1 OR to_owner=$1) AND $2 < expires_at ORDER BY sent_at,from_owner,to_owner", &[&owner, &now]).await?.iter().map(Invite::from_row).collect())
}
pub(crate) async fn insert_invite(ex: &(impl Executor + ?Sized), invite: &Invite) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.war_invites (from_owner,to_owner,sent_at,expires_at) VALUES ($1,$2,$3,$4) ON CONFLICT (from_owner,to_owner) DO UPDATE SET sent_at=EXCLUDED.sent_at, expires_at=EXCLUDED.expires_at WHERE game.war_invites.expires_at <= EXCLUDED.sent_at", &[&invite.from_owner, &invite.to_owner, &invite.sent_at, &invite.expires_at]).await? != 0)
}
pub(crate) async fn remove_invite(ex: &(impl Executor + ?Sized), from: i64, to: i64, expected_sent: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("DELETE FROM game.war_invites WHERE from_owner=$1 AND to_owner=$2 AND sent_at=$3", &[&from, &to, &expected_sent]).await? != 0)
}
pub(crate) async fn expire_invite(ex: &(impl Executor + ?Sized), from: i64, to: i64, expected_sent: i64, expected_expiry: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("DELETE FROM game.war_invites WHERE from_owner=$1 AND to_owner=$2 AND sent_at=$3 AND expires_at=$4 AND expires_at <= $5", &[&from, &to, &expected_sent, &expected_expiry, &now]).await? != 0)
}
pub(crate) async fn invites_blocked(ex: &(impl Executor + ?Sized), owner: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT blocked FROM game.war_invite_settings WHERE owner_id=$1", &[&owner]).await?.map(|r| r.get("blocked")).unwrap_or(false))
}
pub(crate) async fn set_invites_blocked(ex: &(impl Executor + ?Sized), owner: i64, blocked: bool) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.war_invite_settings SET blocked=$2 WHERE owner_id=$1 AND blocked IS DISTINCT FROM $2", &[&owner, &blocked]).await? != 0)
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
