//! Persistent Alpha/Omega authority rows and guarded audit writes.
//! Mirrors `omega/client/clone_grade_svc.py`; state is held in `game.account_clone_grades`.
//! Services own transactions, deadlines and notifications. Missing rows are errors rather than defaults.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

#[derive(Clone, Debug)]
pub(crate) struct CloneGrade {
    pub user_id: i64,
    pub grade: i16,
    pub omega_until: Option<i64>,
    pub grace_until: Option<i64>,
    pub revision: i64,
}
impl CloneGrade {
    fn from_row(row: &Row) -> Self { Self { user_id: row.get("user_id"), grade: row.get("grade"), omega_until: row.get("omega_until"), grace_until: row.get("grace_until"), revision: row.get("revision") } }
}

pub(crate) async fn by_user(ex: &(impl Executor + ?Sized), user_id: i64) -> Result<CloneGrade, db::Error> {
    Ok(CloneGrade::from_row(&ex.query_one("SELECT user_id, grade, omega_until, grace_until, revision FROM game.account_clone_grades WHERE user_id = $1", &[&user_id]).await?))
}

const LOCK_SELECT: &str = "SELECT user_id, grade, omega_until, grace_until, revision FROM game.account_clone_grades WHERE user_id = $1 FOR UPDATE";

pub(crate) async fn lock(ex: &(impl Executor + ?Sized), user_id: i64) -> Result<CloneGrade, db::Error> {
    Ok(CloneGrade::from_row(&ex.query_one(LOCK_SELECT, &[&user_id]).await?))
}

pub(crate) async fn lock_opt(ex: &(impl Executor + ?Sized), user_id: i64) -> Result<Option<CloneGrade>, db::Error> {
    Ok(ex.query_opt(LOCK_SELECT, &[&user_id]).await?.as_ref().map(CloneGrade::from_row))
}

pub(crate) async fn finite(ex: &(impl Executor + ?Sized)) -> Result<Vec<CloneGrade>, db::Error> {
    Ok(ex.query("SELECT user_id, grade, omega_until, grace_until, revision FROM game.account_clone_grades WHERE grade = 1 AND omega_until IS NOT NULL ORDER BY user_id", &[]).await?.iter().map(CloneGrade::from_row).collect())
}

pub(crate) async fn set_grade(ex: &(impl Executor + ?Sized), user_id: i64, grade: i16, omega_until: Option<i64>, grace_until: Option<i64>, revision: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.account_clone_grades SET grade = $2, omega_until = $3, grace_until = $4, revision = revision + 1 WHERE user_id = $1 AND revision = $5 AND revision < 9223372036854775807", &[&user_id, &grade, &omega_until, &grace_until, &revision]).await? == 1)
}

pub(crate) async fn audit(ex: &(impl Executor + ?Sized), user_id: i64, actor: Option<i64>, before: &CloneGrade, after_grade: i16, after_omega: Option<i64>, after_grace: Option<i64>, reason: &str) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.account_clone_grade_audit (user_id, actor_user_id, before_grade, after_grade, before_omega_until, after_omega_until, before_grace_until, after_grace_until, reason) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)", &[&user_id, &actor, &before.grade, &after_grade, &before.omega_until, &after_omega, &before.grace_until, &after_grace, &reason]).await?;
    Ok(())
}
