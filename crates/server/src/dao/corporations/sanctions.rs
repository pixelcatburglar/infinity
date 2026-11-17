//! Sanctioned corporation vote action persistence for `corp_ui_sanctionableactions.py`: one
//! decided action per case, joined to the immutable case and option rows for its type and
//! parameters.
//! Execution policy, transactions and notifications are not handled here.
use super::votes::Deadline;
use crate::db::{self, executor::Executor};

pub(crate) struct Action { pub(crate) case_id: i64, pub(crate) option_id: i32, pub(crate) expires: i64, pub(crate) acted_upon: bool, pub(crate) in_effect: bool, pub(crate) time_acted_upon: Option<i64>, pub(crate) time_rescinded: Option<i64>, pub(crate) expired_at: Option<i64> }
impl Action { fn from_row(r: &tokio_postgres::Row) -> Self { Self { case_id: r.get("case_id"), option_id: r.get("option_id"), expires: r.get("expires"), acted_upon: r.get("acted_upon"), in_effect: r.get("in_effect"), time_acted_upon: r.get("time_acted_upon"), time_rescinded: r.get("time_rescinded"), expired_at: r.get("expired_at") } } }
/// An action row joined with its case's vote type and winning option parameters.
pub(crate) struct ActionRow { pub(crate) action: Action, pub(crate) vote_type: i64, pub(crate) parameter: Option<i64>, pub(crate) parameter1: Option<i64>, pub(crate) parameter2: Option<i64> }
impl ActionRow { fn from_row(r: &tokio_postgres::Row) -> Self { Self { action: Action::from_row(r), vote_type: r.get("vote_type"), parameter: r.get("parameter"), parameter1: r.get("parameter1"), parameter2: r.get("parameter2") } } }

const ACTION_COLUMNS: &str = "case_id,option_id,expires,acted_upon,in_effect,time_acted_upon,time_rescinded,expired_at";

pub(crate) async fn create(ex: &(impl Executor + ?Sized), action: &Action) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.corp_sanctioned_actions (case_id,option_id,expires,acted_upon,in_effect,time_acted_upon,time_rescinded,expired_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (case_id) DO NOTHING", &[&action.case_id,&action.option_id,&action.expires,&action.acted_upon,&action.in_effect,&action.time_acted_upon,&action.time_rescinded,&action.expired_at]).await? != 0)
}
pub(crate) async fn get(ex: &(impl Executor + ?Sized), case: i64) -> Result<Option<Action>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("SELECT {ACTION_COLUMNS} FROM game.corp_sanctioned_actions WHERE case_id=$1"));
    Ok(ex.query_opt(SQL.as_str(), &[&case]).await?.as_ref().map(Action::from_row))
}
pub(crate) async fn lock(ex: &(impl Executor + ?Sized), case: i64) -> Result<Option<Action>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("SELECT {ACTION_COLUMNS} FROM game.corp_sanctioned_actions WHERE case_id=$1 FOR UPDATE"));
    Ok(ex.query_opt(SQL.as_str(), &[&case]).await?.as_ref().map(Action::from_row))
}
pub(crate) async fn list(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Vec<ActionRow>, db::Error> {
    Ok(ex.query("SELECT a.case_id,a.option_id,a.expires,a.acted_upon,a.in_effect,a.time_acted_upon,a.time_rescinded,a.expired_at,c.vote_type,o.parameter,o.parameter1,o.parameter2 FROM game.corp_sanctioned_actions a JOIN game.corp_vote_cases c ON c.id=a.case_id JOIN game.corp_vote_options o ON o.case_id=a.case_id AND o.option_id=a.option_id WHERE c.corporation_id=$1 ORDER BY a.case_id", &[&corp]).await?.iter().map(ActionRow::from_row).collect())
}
/// Marks an unacted, unexpired action executed while it is still inside its window.
pub(crate) async fn mark_acted(ex: &(impl Executor + ?Sized), case: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.corp_sanctioned_actions SET acted_upon=true,in_effect=true,time_acted_upon=$2 WHERE case_id=$1 AND NOT acted_upon AND expired_at IS NULL AND expires>$2", &[&case,&now]).await? != 0)
}
/// Records that an unacted action passed its execution window.
pub(crate) async fn mark_expired(ex: &(impl Executor + ?Sized), case: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.corp_sanctioned_actions SET expired_at=$2 WHERE case_id=$1 AND NOT acted_upon AND expires<=$2 AND expired_at IS NULL", &[&case,&now]).await? != 0)
}
/// Takes an enacted action back out of effect, keeping the row as history.
pub(crate) async fn rescind(ex: &(impl Executor + ?Sized), case: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.corp_sanctioned_actions SET in_effect=false,time_rescinded=$2 WHERE case_id=$1 AND acted_upon AND in_effect AND time_rescinded IS NULL", &[&case,&now]).await? != 0)
}
/// Every unacted, unexpired action and its execution deadline, for scheduler seeding.
pub(crate) async fn pending_deadlines(ex: &(impl Executor + ?Sized)) -> Result<Vec<Deadline>, db::Error> {
    Ok(ex.query("SELECT case_id,expires AS due_at FROM game.corp_sanctioned_actions WHERE NOT acted_upon AND expired_at IS NULL ORDER BY case_id", &[]).await?.iter().map(|r| Deadline { case_id: r.get("case_id"), due_at: r.get("due_at") }).collect())
}
