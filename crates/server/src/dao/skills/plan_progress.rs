//! Viewer-specific milestone snapshots in `game.skill_plan_milestone_progress`.
//! Progress is derived from trained skills and queue, not a client wire field.
//! Reconciliation and locking belong to the caller.
use crate::db::{self, executor::Executor};

/// Server recovery page size bounds startup memory and transaction work.
const RECOVERY_PAGE_SIZE: i64 = 100;
struct CharacterId { character_id: i64 }
impl CharacterId {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { character_id: row.get("character_id") } }
}
pub(crate) async fn characters_after(ex: &(impl Executor + ?Sized), after: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("SELECT DISTINCT character_id FROM game.skill_plan_milestone_progress WHERE character_id > $1 ORDER BY character_id LIMIT $2", &[&after, &RECOVERY_PAGE_SIZE]).await?.iter().map(CharacterId::from_row).map(|row| row.character_id).collect())
}
pub(crate) struct Snapshot { pub milestone_id: Vec<u8>, pub required_sp: i64, pub trained_sp: i64, pub queued_sp: i64, pub completed: bool, pub valid: bool }
pub(crate) async fn upsert(ex: &(impl Executor + ?Sized), character_id: i64, snapshots: &[Snapshot], evaluated_at: i64) -> Result<(), db::Error> {
    let ids: Vec<&[u8]> = snapshots.iter().map(|s| s.milestone_id.as_slice()).collect();
    let required: Vec<i64> = snapshots.iter().map(|s| s.required_sp).collect();
    let trained: Vec<i64> = snapshots.iter().map(|s| s.trained_sp).collect();
    let queued: Vec<i64> = snapshots.iter().map(|s| s.queued_sp).collect();
    let completed: Vec<bool> = snapshots.iter().map(|s| s.completed).collect();
    let valid: Vec<bool> = snapshots.iter().map(|s| s.valid).collect();
    ex.execute("INSERT INTO game.skill_plan_milestone_progress(character_id, milestone_id, required_sp, trained_sp, queued_sp, completed, valid, evaluated_at) SELECT $1, x.id, x.required, x.trained, x.queued, x.completed, x.valid, $8 FROM unnest($2::bytea[], $3::bigint[], $4::bigint[], $5::bigint[], $6::boolean[], $7::boolean[]) AS x(id, required, trained, queued, completed, valid) ON CONFLICT (character_id, milestone_id) DO UPDATE SET required_sp = EXCLUDED.required_sp, trained_sp = EXCLUDED.trained_sp, queued_sp = EXCLUDED.queued_sp, completed = EXCLUDED.completed, valid = EXCLUDED.valid, evaluated_at = EXCLUDED.evaluated_at WHERE (game.skill_plan_milestone_progress.required_sp, game.skill_plan_milestone_progress.trained_sp, game.skill_plan_milestone_progress.queued_sp, game.skill_plan_milestone_progress.completed, game.skill_plan_milestone_progress.valid) IS DISTINCT FROM (EXCLUDED.required_sp, EXCLUDED.trained_sp, EXCLUDED.queued_sp, EXCLUDED.completed, EXCLUDED.valid)", &[&character_id, &ids, &required, &trained, &queued, &completed, &valid, &evaluated_at]).await?;
    Ok(())
}
