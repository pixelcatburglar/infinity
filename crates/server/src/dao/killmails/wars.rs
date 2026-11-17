//! Immutable war-side indexes for `game.killmail_wars` and `game.killmails`.
//! Serves the client `eve/client/script/ui/shared/neocom/war/warReport.py`.
//! Classification and transactions belong to callers; historical membership is not inferred.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
use super::Killmail;

#[derive(Clone, Debug)]
pub(crate) struct Projection {
    pub kill_id: i64,
    pub war_id: i64,
    pub attacker_side: i64,
    pub defender_side: i64,
}
impl Projection {
    fn from_row(r: &Row) -> Self {
        Self { kill_id: r.get("kill_id"), war_id: r.get("war_id"), attacker_side: r.get("attacker_side"), defender_side: r.get("defender_side") }
    }
}
pub(crate) struct IndexedKillmail { pub projection: Projection, pub killmail: Killmail }

pub(crate) async fn insert(ex: &(impl Executor + ?Sized), projection: &Projection) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.killmail_wars (kill_id,war_id,attacker_side,defender_side) SELECT $1,$2,$3,$4 FROM game.wars w WHERE w.war_id=$2 AND (($3=w.declared_by AND $4=w.against) OR ($3=w.against AND $4=w.declared_by)) ON CONFLICT DO NOTHING", &[&projection.kill_id, &projection.war_id, &projection.attacker_side, &projection.defender_side]).await? != 0)
}
pub(crate) async fn by_kills(ex: &(impl Executor + ?Sized), kill_ids: &[i64]) -> Result<Vec<Projection>, db::Error> {
    Ok(ex.query("SELECT * FROM game.killmail_wars WHERE kill_id=ANY($1) ORDER BY kill_id,war_id", &[&kill_ids]).await?.iter().map(Projection::from_row).collect())
}
pub(crate) async fn killmails(ex: &(impl Executor + ?Sized), war_id: i64) -> Result<Vec<IndexedKillmail>, db::Error> {
    let rows = ex.query("SELECT k.*, p.war_id, p.attacker_side, p.defender_side FROM game.killmail_wars p JOIN game.killmails k ON k.kill_id=p.kill_id WHERE p.war_id=$1 ORDER BY k.kill_id DESC", &[&war_id]).await?;
    Ok(rows.iter().map(|r| IndexedKillmail { projection: Projection::from_row(r), killmail: Killmail::from_row(r) }).collect())
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
