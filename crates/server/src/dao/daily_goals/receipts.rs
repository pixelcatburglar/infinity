//! Durable daily payment and QA audit receipts; no money movement or policy.
//! Client replay is resolved by the caller before applying current-day limits.
use crate::db::{self, executor::Executor};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaymentRow { pub goal_id: String, pub character_id: i64, pub day_start: i64, pub ordinal: i16, pub issuer_id: i64, pub amount: i64, pub paid_at: i64 }
impl PaymentRow {
    fn from_row(r: &tokio_postgres::Row) -> Result<Self, db::Error> { Ok(Self { goal_id:r.try_get("goal_id")?, character_id:r.try_get("character_id")?, day_start:r.try_get("day_start")?, ordinal:r.try_get("ordinal")?, issuer_id:r.try_get("issuer_id")?, amount:r.try_get("amount")?, paid_at:r.try_get("paid_at")? }) }
}
pub(crate) async fn payment(ex: &(impl Executor + ?Sized), owner: i64, goal: &str) -> Result<Option<PaymentRow>, db::Error> {
    ex.query_opt("SELECT goal_id::text AS goal_id,character_id,day_start,ordinal,issuer_id,amount,paid_at FROM game.daily_goal_payments WHERE character_id=$1 AND goal_id=$2::text::uuid", &[&owner,&goal]).await?.as_ref().map(PaymentRow::from_row).transpose()
}
pub(crate) async fn payment_count(ex: &(impl Executor + ?Sized), owner: i64, day: i64) -> Result<i64, db::Error> {
    Ok(ex.query_one("SELECT count(*)::bigint AS count FROM game.daily_goal_payments WHERE character_id=$1 AND day_start=$2", &[&owner,&day]).await?.try_get("count")?)
}
pub(crate) async fn insert_payment(ex: &(impl Executor + ?Sized), p: &PaymentRow) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.daily_goal_payments(goal_id,character_id,day_start,ordinal,issuer_id,amount,paid_at) SELECT goal_id,$2,$3,$4,$5,$6,$7 FROM game.daily_goal_instances WHERE goal_id=$1::text::uuid AND character_id=$2 ON CONFLICT DO NOTHING", &[&p.goal_id,&p.character_id,&p.day_start,&p.ordinal,&p.issuer_id,&p.amount,&p.paid_at]).await? == 1)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuditRow { pub audit_id: String, pub actor_id: i64, pub subject_id: i64, pub goal_id: String, pub operation: String, pub payload: String, pub before_state: String, pub after_state: String, pub recorded_at: i64 }
impl AuditRow {
    fn from_row(r: &tokio_postgres::Row) -> Result<Self, db::Error> { Ok(Self { audit_id:r.try_get("audit_id")?, actor_id:r.try_get("actor_id")?, subject_id:r.try_get("subject_id")?, goal_id:r.try_get("goal_id")?, operation:r.try_get("operation")?, payload:r.try_get("payload")?, before_state:r.try_get("before_state")?, after_state:r.try_get("after_state")?, recorded_at:r.try_get("recorded_at")? }) }
}
pub(crate) async fn audit(ex: &(impl Executor + ?Sized), id: &str) -> Result<Option<AuditRow>, db::Error> {
    ex.query_opt("SELECT audit_id::text AS audit_id,actor_id,subject_id,goal_id::text AS goal_id,operation,payload::text AS payload,before_state::text AS before_state,after_state::text AS after_state,recorded_at FROM game.daily_goal_admin_audit WHERE audit_id=$1::text::uuid", &[&id]).await?.as_ref().map(AuditRow::from_row).transpose()
}
pub(crate) async fn insert_audit(ex: &(impl Executor + ?Sized), a: &AuditRow) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.daily_goal_admin_audit(audit_id,actor_id,subject_id,goal_id,operation,payload,before_state,after_state,recorded_at) SELECT $1::text::uuid,$2,$3,goal_id,$5,$6::text::jsonb,$7::text::jsonb,$8::text::jsonb,$9 FROM game.daily_goal_instances WHERE goal_id=$4::text::uuid AND character_id=$3 ON CONFLICT DO NOTHING", &[&a.audit_id,&a.actor_id,&a.subject_id,&a.goal_id,&a.operation,&a.payload,&a.before_state,&a.after_state,&a.recorded_at]).await? == 1)
}
