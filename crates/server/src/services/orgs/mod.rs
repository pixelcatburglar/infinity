//! Bulletins, contacts and contact labels of corporations (corpRegistry) and alliances
//! (bound allianceRegistry), kept per owner in game.org_*.
//!
//! A corporation's communications officers (corpRoleChatManager) and directors edit its
//! bulletins, and its diplomats and directors its contacts and labels; for an alliance,
//! those of its executor corporation do.
//! The bulletin limit is an operator policy for both corporations and alliances.

use super::corps::permissions;
use super::{crowset, db_err, keyval, notify_error, value_ids, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use infinity_core::consts::ROLE_DIRECTOR;
use crate::dao::{alliances, orgs};
use crate::db::{Db, executor::Executor};
use infinity_core::messages;

mod alliance_registry;
mod corp_registry;
use marshal::{DbRowDescriptor, DbType, Value};

/// Client appConst.corpRoleChatManager.
const ROLE_CHAT_MANAGER: i64 = 36_028_797_018_963_968;
/// Client appConst.corpRoleDiplomat.
const ROLE_DIPLOMAT: i64 = 17_179_869_184;
/// Player alliance ID range, matching the server's game.alliances sequence.
const FIRST_PLAYER_ALLIANCE: i64 = 99_000_000;
/// Server label-mask policy: exclusive upper bit position for positive i64 labels.
const LABEL_BIT_END: u32 = 62;

/// The owner a call is for: the bound corporation, or the bound alliance.
fn owner(c: &Client, call: &Call<'_>) -> Option<i64> {
    let bound = call.bind_params.and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int())));
    match call.service {
        "corpRegistry" => bound.or_else(|| c.session.int("corpid")),
        _ => bound.or_else(|| c.session.int("allianceid")),
    }
}

/// Whether `me` holds one of `roles` in `owner` (a corporation) or in its executor
/// corporation (an alliance).
async fn may(db: &(impl Executor + ?Sized), me: i64, owner: i64, expected_corp: i64, roles: i64) -> Result<bool, CallError> {
    let Some(authority) = permissions::load(db, me).await? else { return Ok(false) };
    let acting_corp = if owner >= FIRST_PLAYER_ALLIANCE {
        alliances::executor_corp(db, owner).await.map_err(db_err)?
    } else {
        Some(owner)
    };
    Ok(authority.live && authority.corporation == expected_corp && acting_corp == Some(authority.corporation) && authority.roles[0] & (roles | ROLE_DIRECTOR) != 0)
}

fn bulletin_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("bulletinID", DbType::I4),
        ("ownerID", DbType::I4),
        ("title", DbType::WStr),
        ("body", DbType::WStr),
        ("editCharacterID", DbType::I4),
        ("editDateTime", DbType::FileTime),
        ("sortOrder", DbType::I4),
    ])
}

async fn bulletins(db: &Db, owner: i64) -> CallResult {
    let rows = orgs::bulletins(db, owner).await.map_err(db_err)?;
    let header = bulletin_header();
    let rows = rows
        .into_iter()
        .map(|r| {
            header.row(&[
                Value::Int(r.id),
                Value::Int(owner),
                Value::unicode(r.title),
                Value::unicode(r.body),
                Value::Int(r.edit_char_id),
                Value::Int(r.edit_date),
                Value::Int(r.sort_order as i64),
            ])
        })
        .collect();
    Ok(crowset(&header, rows))
}

/// AddBulletin(title, body, bulletinID=None, editDateTime=None): a new bulletin, or an edit of
/// `bulletinID` unless someone saved it after `editDateTime`.
async fn add_bulletin(db: &(impl Executor + ?Sized), me: i64, owner: i64, call: &Call<'_>) -> CallResult {
    let text = |i: usize| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let (title, body) = (text(0), text(1));
    let edit_of = call.kwargs.get("bulletinID").or_else(|| call.args.get(2)).and_then(|v| v.as_int());
    let seen = call.kwargs.get("editDateTime").or_else(|| call.args.get(3)).and_then(|v| v.as_int());
    let now = filetime_now();
    match edit_of {
        Some(id) => {
            let current = orgs::edit_date(db, id, owner).await.map_err(db_err)?;
            match current {
                None => return Err(CallError::User { msg: messages::MSG_CORP_BULLETIN_NOT_FOUND.into(), dict: vec![] }),
                Some(t) if seen.is_some_and(|s| s < t) => return Err(CallError::User { msg: messages::MSG_CORP_BULLETIN_HAS_BEEN_EDITED.into(), dict: vec![] }),
                Some(_) => {}
            }
            orgs::update_bulletin(db, id, &title, &body, me, now).await.map_err(db_err)?;
        }
        None => {
            let count = orgs::bulletin_count(db, owner).await.map_err(db_err)?;
            if count >= crate::config::game().corporations.max_bulletins {
                return Err(CallError::User { msg: messages::MSG_CORP_TOO_MANY_BULLETINS.into(), dict: vec![] });
            }
            orgs::insert_bulletin(db, owner, &title, &body, me, now).await.map_err(db_err)?;
        }
    }
    Ok(Value::None)
}

/// {contactID: KeyVal(contactID, relationshipID, labelMask)}
async fn contacts(db: &Db, owner: i64) -> CallResult {
    let rows = orgs::contacts(db, owner).await.map_err(db_err)?;
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let id = r.id;
                let rel = r.relationship;
                (
                    Value::Int(id),
                    keyval([("contactID", Value::Int(id)), ("relationshipID", Value::Float(rel)), ("labelMask", Value::Int(r.label_mask))]),
                )
            })
            .collect(),
    ))
}

/// {labelID: KeyVal(labelID, name, color)}
async fn labels(db: &Db, owner: i64) -> CallResult {
    let rows = orgs::labels(db, owner).await.map_err(db_err)?;
    Ok(Value::Dict(
        rows.into_iter()
            .map(|r| {
                let id = r.id;
                let color = r.color.map(|c| Value::Int(c as i64)).unwrap_or(Value::None);
                (Value::Int(id), keyval([("labelID", Value::Int(id)), ("name", Value::unicode(r.name)), ("color", color)]))
            })
            .collect(),
    ))
}

async fn mutate(ex: &(impl Executor + ?Sized), me: i64, owner: i64, call: &Call<'_>) -> CallResult {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let relationship = |i: usize| call.kwargs.get("relationshipID").or_else(|| call.args.get(i)).and_then(|v| v.as_float().or_else(|| v.as_int().map(|i| i as f64)));
    let done = |result: Result<u64, crate::db::Error>| result.map(|_| Value::None).map_err(db_err);
    match call.method {
        "AddBulletin" => add_bulletin(ex, me, owner, call).await,
        "DeleteBulletin" => done(orgs::delete_bulletin(ex, int(0).unwrap_or(0), owner).await),
        "UpdateBulletinOrder" => orgs::order_bulletins(ex, owner, &value_ids(call.args.first())).await.map(|_| Value::None).map_err(db_err),
        "AddCorporateContact" | "AddAllianceContact" | "EditCorporateContact" | "EditAllianceContact" =>
            done(orgs::upsert_contact(ex, owner, int(0).unwrap_or(0), relationship(1).unwrap_or(0.0)).await),
        "RemoveCorporateContacts" | "RemoveAllianceContacts" =>
            done(orgs::delete_contacts(ex, owner, &value_ids(call.args.first())).await),
        "EditContactsRelationshipID" =>
            done(orgs::update_relationship(ex, owner, &value_ids(call.args.first()), relationship(1).unwrap_or(0.0)).await),
        "CreateLabel" => {
            let name = call.args.first().and_then(|v| v.as_text()).map(|t| t.trim().to_string()).unwrap_or_default();
            let color = int(1).map(|c| c as i32);
            if name.is_empty() { return Err(CallError::User { msg: messages::MSG_MAIL_LABEL_MUST_PROVIDE_NAME.into(), dict: vec![] }); }
            let used = orgs::label_ids(ex, owner).await.map_err(db_err)?;
            let Some(id) = (0..LABEL_BIT_END).map(|b| 1i64 << b).find(|b| !used.contains(b)) else { return Err(notify_error(messages::LABEL_LIMIT_REACHED)); };
            orgs::insert_label(ex, owner, id, &name, color).await.map(|_| Value::Int(id)).map_err(db_err)
        }
        "DeleteLabel" => {
            let id = int(0).unwrap_or(0);
            orgs::clear_label(ex, owner, id).await.map_err(db_err)?;
            done(orgs::delete_label(ex, id, owner).await)
        }
        "EditLabel" => {
            let name = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.trim().to_string());
            done(orgs::edit_label(ex, int(0).unwrap_or(0), owner, name, int(2).map(|c| c as i32)).await)
        }
        "AssignLabels" | "RemoveLabels" => {
            let change = if call.method == "AssignLabels" { orgs::LabelChange::Assign } else { orgs::LabelChange::Remove };
            done(orgs::change_labels(ex, owner, &value_ids(call.args.first()), int(1).unwrap_or(0), change).await)
        }
        _ => Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION)),
    }
}

async fn dispatch_shared(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let owner = owner(c, call)?;
    let roles = match call.method {
        "AddBulletin" | "DeleteBulletin" | "UpdateBulletinOrder" => ROLE_CHAT_MANAGER,
        "AddCorporateContact" | "AddAllianceContact" | "EditCorporateContact" | "EditAllianceContact" | "RemoveCorporateContacts"
        | "RemoveAllianceContacts" | "EditContactsRelationshipID" | "CreateLabel" | "DeleteLabel" | "EditLabel" | "AssignLabels" | "RemoveLabels" => ROLE_DIPLOMAT,
        "GetBulletins" => return Some(bulletins(&c.db, owner).await),
        "GetCorporateContacts" | "GetAllianceContacts" => return Some(contacts(&c.db, owner).await),
        "GetLabels" => return Some(labels(&c.db, owner).await),
        _ => return None,
    };
    let corp = match permissions::load(&c.db, me).await {
        Ok(Some(authority)) if authority.live => authority.corporation,
        Ok(_) => return Some(Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION))),
        Err(error) => return Some(Err(error)),
    };
    let method = call.method.to_owned();
    let args = call.args.to_vec();
    let kwargs = call.kwargs.clone();
    Some(c.db.transaction(
        |tx, _: &mut ()| {
            let method = method.clone();
            let args = args.clone();
            let kwargs = kwargs.clone();
            Box::pin(async move {
                let alliances = if owner >= FIRST_PLAYER_ALLIANCE { vec![owner] } else { Vec::new() };
                let corps = if owner >= FIRST_PLAYER_ALLIANCE { vec![corp] } else { vec![corp, owner] };
                if !crate::dao::corporations::registry::lock_admission(tx, &alliances, &corps, &[me]).await.map_err(db_err)?
                    || !may(tx, me, owner, corp, roles).await? { return Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION)); }
                let call = Call { service: "", method: &method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                mutate(tx, me, owner, &call).await
            })
        },
        |value, ()| async move { Ok(value) },
    ).await)
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "corpRegistry" => corp_registry::dispatch(c, call).await,
        "allianceRegistry" => alliance_registry::dispatch(c, call).await,
        _ => None,
    }
}
