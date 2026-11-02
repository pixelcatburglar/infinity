//! `mailingListsMgr`, mirroring mailingListsSvc.py and its membership, settings and welcome views.
//! List state is persisted by the mailing-list service and DAO; paid lists are unsupported.

use super::mailing_lists::{self, MemberAction};
use super::{text_argument, ACCESS_ALLOWED, ACCESS_BLOCKED, MAX_MEMBER_BATCH, ROLE_MUTED, ROLE_OPERATOR, ROLE_OWNER};
use super::super::{keyval, notify_error, Call, CallError, CallResult};
use crate::{client::Client, dao::mailing_lists::List, messages};
use marshal::Value;
use std::collections::BTreeMap;

fn invalid() -> CallError { notify_error(messages::mail::INVALID_ARGUMENT) }
fn integer(v: &Value) -> Result<i64, CallError> {
    match v { Value::Int(n) => Ok(*n), _ => Err(invalid()) }
}
fn id(v: &Value) -> Result<i64, CallError> {
    let n = integer(v)?;
    if n > 0 { Ok(n) } else { Err(invalid()) }
}
fn access(v: &Value) -> Result<i16, CallError> {
    match integer(v)? {
        n if n == i64::from(ACCESS_BLOCKED) => Ok(ACCESS_BLOCKED),
        n if n == i64::from(ACCESS_ALLOWED) => Ok(ACCESS_ALLOWED),
        _ => Err(invalid()),
    }
}
fn fee(v: &Value) -> Result<i64, CallError> {
    let n = integer(v)?;
    if n == 0 { Ok(n) } else { Err(notify_error(messages::mail::FEES_UNSUPPORTED)) }
}
fn batch(v: &Value) -> Result<Vec<i64>, CallError> {
    let values = match v { Value::List(v) | Value::Tuple(v) => v, _ => return Err(invalid()) };
    if values.len() > MAX_MEMBER_BATCH { return Err(invalid()); }
    let mut ids = values.iter().map(id).collect::<Result<Vec<_>, _>>()?;
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}
fn access_entries(v: &Value) -> Result<Vec<(i64, i16)>, CallError> {
    let Value::Dict(values) = v else { return Err(invalid()); };
    if values.len() > MAX_MEMBER_BATCH { return Err(invalid()); }
    let mut entries = BTreeMap::new();
    for (key, value) in values {
        let (key, value) = (id(key)?, access(value)?);
        if entries.insert(key, value).is_some_and(|old| old != value) { return Err(invalid()); }
    }
    Ok(entries.into_iter().collect())
}
fn view(list: &List, role: i16, me: i64) -> Value {
    let owner = list.owner_id == me;
    keyval([
        ("id", Value::Int(list.list_id)),
        ("name", Value::unicode(list.name_key.as_str())),
        ("displayName", Value::unicode(list.display_name.as_str())),
        ("isMuted", Value::Bool(!owner && role == ROLE_MUTED)),
        ("isOperator", Value::Bool(!owner && role == ROLE_OPERATOR)),
        ("isOwner", Value::Bool(owner)),
    ])
}

// mailingListsSvc.py Initialize, GetDisplayName and JoinMailingList consume these KeyVal fields.
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let count = match call.method {
        "GetJoinedLists" => 0,
        "GetInfo" | "Join" | "Leave" | "Delete" | "GetMembers" | "GetSettings" | "GetWelcomeMail" | "ClearWelcomeMail" => 1,
        "KickMembers" | "SetMembersMuted" | "SetMembersOperator" | "SetMembersClear" | "SetEntitiesAccess" | "ClearEntityAccess" => 2,
        "SaveWelcomeMail" | "SendWelcomeMail" => 3,
        "Create" | "SetDefaultAccess" => 4,
        _ => return None,
    };
    Some(handle(c, call, count).await)
}
async fn handle(c: &Client, call: &Call<'_>, count: usize) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    if me <= 0 { return Err(notify_error(messages::mail::SELECT_CHARACTER)); }
    if call.args.len() != count || !(matches!(call.kwargs, Value::None) || matches!(call.kwargs, Value::Dict(v) if v.is_empty())) {
        return Err(invalid());
    }
    let a = call.args;
    match call.method {
        // eve/client/script/ui/services/mail/mailingListsSvc.py:CreateMailingList consumes the list ID.
        "Create" => Ok(Value::Int(mailing_lists::create(c, text_argument(&a[0])?, access(&a[1])?, access(&a[2])?, fee(&a[3])?).await?)),
        // mailingListsSvc.py:JoinMailingList consumes id, name, displayName and the three role flags.
        "Join" => { let (list, role) = mailing_lists::join(c, text_argument(&a[0])?).await?; Ok(view(&list, role, me)) }
        // mailingListsSvc.py:GetDisplayName consumes displayName or None.
        "GetInfo" => Ok(mailing_lists::get_info(c, id(&a[0])?).await?.map(|list| keyval([("displayName", Value::unicode(list.display_name))])).unwrap_or(Value::None)),
        // mailingListsSvc.py:Initialize consumes a list-ID map of the same member views.
        "GetJoinedLists" => Ok(Value::Dict(mailing_lists::joined(c).await?.into_iter().map(|(list, role)| (Value::Int(list.list_id), view(&list, role, me))).collect())),
        // mailingListsSvc.py:GetMembers consumes character IDs mapped to roles, including owner role 3.
        "GetMembers" => {
            let (list, members) = mailing_lists::get_members(c, id(&a[0])?).await?;
            let mut rows: BTreeMap<i64, i16> = members.into_iter().map(|m| (m.character_id, m.member_access)).collect();
            rows.insert(list.owner_id, ROLE_OWNER);
            Ok(Value::Dict(rows.into_iter().map(|(id, role)| (Value::Int(id), Value::Int(i64::from(role)))).collect()))
        }
        // mailingListsSvc.py:GetSettings consumes defaultAccess, defaultMemberAccess and access.
        "GetSettings" => {
            let (list, entries) = mailing_lists::get_settings(c, id(&a[0])?).await?;
            Ok(keyval([
                ("defaultAccess", Value::Int(i64::from(list.default_access))),
                ("defaultMemberAccess", Value::Int(i64::from(list.default_member_access))),
                ("access", Value::Dict(entries.into_iter().map(|e| (Value::Int(e.entity_id), Value::Int(i64::from(e.access)))).collect())),
            ]))
        }
        // mailingListsSvc.py:GetWelcomeMail consumes zero or one row with title and body.
        "GetWelcomeMail" => Ok(Value::list(mailing_lists::get_welcome(c, id(&a[0])?).await?.into_iter().map(|(title, body)| keyval([("title", Value::unicode(title)), ("body", Value::unicode(body))])))),
        // mailingListsSvc.py:LeaveMaillist ignores the mutation result.
        "Leave" => { mailing_lists::leave(c, id(&a[0])?).await?; Ok(Value::None) }
        // mailingListsSvc.py:DeleteMaillist ignores the mutation result.
        "Delete" => { mailing_lists::delete(c, id(&a[0])?).await?; Ok(Value::None) }
        // mailingListsSvc.py:KickMembers and SetMembersMuted/Operator/Clear ignore mutation results.
        "KickMembers" | "SetMembersMuted" | "SetMembersOperator" | "SetMembersClear" => {
            let action = match call.method { "KickMembers" => MemberAction::Kick, "SetMembersMuted" => MemberAction::Muted, "SetMembersOperator" => MemberAction::Operator, _ => MemberAction::Clear };
            mailing_lists::members_action(c, id(&a[0])?, batch(&a[1])?, action).await?; Ok(Value::None)
        }
        // mailingListsSvc.py:SetEntitiesAccess ignores the mutation result.
        "SetEntitiesAccess" => { mailing_lists::set_access(c, id(&a[0])?, access_entries(&a[1])?).await?; Ok(Value::None) }
        // mailingListsSvc.py:ClearEntityAccess ignores the mutation result.
        "ClearEntityAccess" => { mailing_lists::clear_access(c, id(&a[0])?, id(&a[1])?).await?; Ok(Value::None) }
        // mailingListsSvc.py:SetDefaultAccess ignores the mutation result.
        "SetDefaultAccess" => { mailing_lists::set_defaults(c, id(&a[0])?, access(&a[1])?, access(&a[2])?, fee(&a[3])?).await?; Ok(Value::None) }
        // mailingListsSvc.py:SaveWelcomeMail and SendWelcomeMail ignore mutation results.
        "SaveWelcomeMail" | "SendWelcomeMail" => { mailing_lists::save_welcome(c, id(&a[0])?, text_argument(&a[1])?, text_argument(&a[2])?, call.method == "SendWelcomeMail").await?; Ok(Value::None) }
        // mailingListsSvc.py:ClearWelcomeMail ignores the mutation result.
        "ClearWelcomeMail" => { mailing_lists::clear_welcome(c, id(&a[0])?).await?; Ok(Value::None) }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
