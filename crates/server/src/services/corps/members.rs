//! Client member-query grammar and validated typed DAO operands.
use infinity_core::messages;
use crate::{dao::corporations::members::{self, Clause, SearchValue}, db::Db, services::{db_err, notify_error, Call, CallError, CallResult}};
use marshal::Value;

fn invalid() -> CallError { notify_error(messages::CANNOT_MODIFY_MEMBER_ROLES) }
fn integer(value: &Value) -> Option<i64> { if let Value::Int(value) = value { Some(*value) } else { None } }
fn boolean(value: Option<&Value>) -> Result<bool, CallError> {
    match value { None | Some(Value::Bool(false) | Value::Int(0)) => Ok(false), Some(Value::Bool(true) | Value::Int(1)) => Ok(true), _ => Err(invalid()) }
}
async fn authorize(ex: &(impl crate::db::executor::Executor + ?Sized), actor: i64, corp: i64, detailed: bool) -> Result<(), CallError> {
    if !crate::dao::corporations::registry::lock_admission(ex, &[], &[corp], &[actor]).await.map_err(db_err)? { return Err(invalid()); }
    let authority = super::permissions::load(ex, actor).await?.ok_or_else(invalid)?;
    if authority.corporation != corp || (super::is_player_corp(corp) && !authority.live)
        || (detailed && authority.roles[0] & infinity_core::consts::ROLE_DIRECTOR == 0) { return Err(invalid()); }
    Ok(())
}

pub(super) async fn rows(db: &Db, actor: i64, corp: i64, filter: crate::dao::corporations::registry::MemberFilter, ids: &[i64], detailed: bool) -> Result<Vec<crate::dao::corporations::registry::Member>, CallError> {
    let ids = ids.to_vec();
    db.transaction(
        |tx, _: &mut ()| {
            let ids = ids.clone();
            Box::pin(async move {
                authorize(tx, actor, corp, detailed).await?;
                crate::dao::corporations::registry::members(tx, corp, filter, &ids, infinity_core::consts::FILETIME_SECOND, infinity_core::consts::FILETIME_UNIX_EPOCH).await.map_err(db_err)
            })
        },
        |value, ()| async move { Ok(value) },
    ).await
}

pub(super) async fn page(db: &Db, actor: i64, corp: i64, page: i64, per_page: i64) -> Result<(Vec<crate::dao::corporations::registry::Member>, i64), CallError> {
    db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            authorize(tx, actor, corp, false).await?;
            crate::dao::corporations::registry::member_page(tx, corp, page, per_page, infinity_core::consts::FILETIME_SECOND, infinity_core::consts::FILETIME_UNIX_EPOCH).await.map_err(db_err)
        }),
        |value, ()| async move { Ok(value) },
    ).await
}


pub(super) fn parse(value: Option<&Value>) -> Result<Vec<Clause>, CallError> {
    let rows = value.and_then(Value::items).ok_or_else(invalid)?;
    if rows.len() > 65_532 { return Err(invalid()); }
    let mut clauses = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let fields = row.items().ok_or_else(invalid)?;
        let offset = usize::from(index != 0);
        if fields.len() != 3 + offset { return Err(invalid()); }
        let join = if offset == 0 { 0 } else { integer(&fields[0]).filter(|value| *value == 1 || *value == 2).ok_or_else(invalid)? };
        let name = fields[offset].as_text().ok_or_else(invalid)?;
        let property = match name.as_ref() {
            "roles" => "roles", "rolesAtHQ" => "rolesAtHQ", "rolesAtBase" => "rolesAtBase", "rolesAtOther" => "rolesAtOther",
            "grantableRoles" => "grantableRoles", "grantableRolesAtHQ" => "grantableRolesAtHQ", "grantableRolesAtBase" => "grantableRolesAtBase", "grantableRolesAtOther" => "grantableRolesAtOther",
            "baseID" => "baseID", "startDateTime" => "startDateTime", "characterID" => "characterID", "titleMask" => "titleMask", _ => return Err(invalid()),
        };
        let operator = integer(&fields[offset + 1]).ok_or_else(invalid)?;
        let valid = match property { "characterID" => (9..=13).contains(&operator), "baseID" => operator == 1 || operator == 6,
            "startDateTime" => (1..=6).contains(&operator), _ => operator == 7 || operator == 8 };
        if !valid { return Err(invalid()); }
        let value = if property == "characterID" { SearchValue::Text(fields[offset + 2].as_text().ok_or_else(invalid)?.into_owned()) }
            else { SearchValue::Number(integer(&fields[offset + 2]).ok_or_else(invalid)?) };
        clauses.push(Clause { join, property, operator, value });
    }
    Ok(clauses)
}

pub(super) async fn query(db: &Db, sde: &crate::sde::Sde, actor: i64, corp: i64, call: &Call<'_>) -> CallResult {
    if call.args.len() > 3 { return Err(invalid()); }
    let clauses = parse(call.args.first())?;
    let implied = boolean(call.args.get(1))?;
    let titles = boolean(call.args.get(2))?;
    let valid = sde.corporation_roles.scope_masks();
    for clause in &clauses {
        let scope = match clause.property {
            "roles" | "grantableRoles" => Some(0), "rolesAtHQ" | "grantableRolesAtHQ" => Some(1),
            "rolesAtBase" | "grantableRolesAtBase" => Some(2), "rolesAtOther" | "grantableRolesAtOther" => Some(3), _ => None,
        };
        if let (Some(scope), SearchValue::Number(mask)) = (scope, &clause.value) {
            if *mask & !valid[scope] != 0 { return Err(invalid()); }
        }
    }
    db.transaction(
        |tx, _: &mut ()| {
            let clauses = clauses.clone();
            Box::pin(async move {
                authorize(tx, actor, corp, false).await?;
                Ok(Value::list(members::query(tx, corp, &clauses, implied, titles, valid).await.map_err(db_err)?.into_iter().map(Value::Int)))
            })
        },
        |value, ()| async move { Ok(value) },
    ).await
}
