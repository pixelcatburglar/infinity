//! Corporation member audit production and `corpmgr.AuditMember` query.
//! Mirrors `corp_ui_member_auditing.py` using durable corporation event and role history tables.
//! History predating deployment is not reconstructed.
use infinity_core::{consts::ROLE_DIRECTOR, messages};
use crate::{client::Client, dao::corporations::audit as dao, db::executor::Executor, services::{db_err, keyval, notify_error, Call, CallError, CallResult}};
use marshal::Value;

/// `corp_ui_member_auditing.py:223-236` event identifiers.
pub(super) const CREATED: i32 = 12;
pub(super) const DISSOLVED: i32 = 13;
pub(super) const LEFT: i32 = 14;
pub(super) const APPLIED: i32 = 15;
pub(super) const BECAME_CEO: i32 = 16;
pub(super) const CEASED_CEO: i32 = 17;
pub(super) const JOINED: i32 = 44;
/// `appConst.corpRoleAuditor`.
const ROLE_AUDITOR: i64 = 4096;
/// Server audit query policy: maximum entries per source per page.
const MAX_PAGE: i64 = 100;

pub(super) async fn event(ex: &(impl Executor + ?Sized), character: i64, issuer: i64, corporation: i64, kind: i32, time: i64) -> Result<(), CallError> {
    dao::event(ex, character, issuer, corporation, kind, time).await.map_err(db_err)
}
pub(crate) async fn member_history(c: &Client, call: &Call<'_>) -> CallResult {
    let invalid = || notify_error(messages::CANNOT_MODIFY_MEMBER_ROLES);
    if call.args.len() != 4 { return Err(invalid()); }
    let [Some(target), Some(from), Some(to), Some(limit)] = [0, 1, 2, 3].map(|i| call.args[i].as_int()) else { return Err(invalid()); };
    if target <= 0 || from < 0 || from >= to || !(1..=MAX_PAGE).contains(&limit) { return Err(invalid()); }
    let actor = c.session.int("charid").unwrap_or(0);
    let corp = super::permissions::load(&c.db, actor).await?.ok_or_else(invalid)?.corporation;
    c.db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            if !crate::dao::corporations::registry::lock_admission(tx, &[], &[corp], &[actor]).await.map_err(db_err)? { return Err(invalid()); }
            let authority = super::permissions::load(tx, actor).await?.ok_or_else(invalid)?;
            if !authority.live || authority.corporation != corp || authority.roles[0] & (ROLE_AUDITOR | ROLE_DIRECTOR) == 0 || !dao::target_visible(tx, corp, target).await.map_err(db_err)? { return Err(invalid()); }
            let events = dao::events(tx, corp, target, from, to, limit).await.map_err(db_err)?;
            let roles = dao::roles(tx, corp, target, from, to, limit).await.map_err(db_err)?;
            Ok(Value::tuple([
                Value::list(events.into_iter().map(|r| keyval([( "eventDateTime", Value::Int(r.time)), ("corporationID", Value::Int(r.corporation)), ("eventTypeID", Value::Int(i64::from(r.kind)))]))),
                Value::list(roles.into_iter().map(|r| keyval([( "changeTime", Value::Int(r.time)), ("corporationID", Value::Int(r.corporation)), ("charID", Value::Int(r.character)), ("issuerID", Value::Int(r.issuer)), ("oldRoles", Value::Int(r.old)), ("newRoles", Value::Int(r.new)), ("grantable", Value::Bool(r.grantable))]))),
            ]))
        }),
        |value, ()| async move { Ok(value) },
    ).await
}

#[cfg(test)]
mod tests;
