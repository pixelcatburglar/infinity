//! `mailMgr`, mirroring `eve/client/script/ui/services/mail/mailSvc.py`.
//! Uses the mail DAO's message, status and label tables and character membership.
//! Sending commits message, status and reply/forward writes together before notifications.
//! Mailing lists are not supported.
use super::super::{db_err, keyval, notify_error, value_id_list, Call, CallError, CallResult};
use super::{delivery, STATUS_READ};
use crate::client::Client;
use crate::dao::mail::{self, LabelAction, Selector};
use crate::db::Db;
use crate::messages;
use marshal::Value;
use std::io::Write;

/// Client appConst.mailStatusMask trashed bit.
const STATUS_TRASHED: i32 = 8;
/// Client appConst mail labels: lowest bit free for custom labels.
const FIRST_CUSTOM_LABEL: i32 = 16;
/// Server mail policy: first custom-label bit position.
const FIRST_CUSTOM_LABEL_BIT: u32 = 4;
/// Server mail policy: exclusive upper custom-label bit position.
const CUSTOM_LABEL_BIT_END: u32 = 30;
/// Client appConst mail labels: mask retaining the four built-in labels.
const BUILTIN_LABEL_MASK: i32 = 15;


/// The mail rows SyncMail and GetMailHeaders return.
fn header_rows(rows: &[mail::Header]) -> Value {
    Value::list(rows.iter().map(|r| {
        keyval([
            ("messageID", Value::Int(r.message_id)),
            ("senderID", Value::Int(r.sender_id)),
            ("toCharacterIDs", Value::str(r.to_character_ids.as_str())),
            ("toListID", r.to_list_id.map(Value::Int).unwrap_or(Value::None)),
            ("toCorpOrAllianceID", r.to_corp_or_alliance_id.map(Value::Int).unwrap_or(Value::None)),
            ("title", Value::unicode(r.title.as_str())),
            ("sentDate", Value::Int(r.sent_date)),
        ])
    }))
}


/// `SyncMail(firstID, lastID)` -> KeyVal(newMail, oldMail, mailStatus).
async fn sync(c: &Client, last_id: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let new = mail::new_headers(&c.db, me, last_id).await.map_err(db_err)?;
    let status = mail::statuses(&c.db, me).await.map_err(db_err)?;
    Ok(keyval([
        ("newMail", header_rows(&new)),
        ("oldMail", Value::None),
        (
            "mailStatus",
            Value::list(status.iter().map(|r| keyval([("messageID", Value::Int(r.message_id)), ("statusMask", Value::Int(r.status_mask)), ("labelMask", Value::Int(r.label_mask))]))),
        ),
    ]))
}


/// `SendMail(toCharacterIDs, toListID, toCorpOrAllianceID, title, body, isReplyTo, isForwardedFrom)` -> messageID.
async fn send(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let to_chars = value_id_list(call.args.first());
    let to_list = call.args.get(1).and_then(|v| v.as_int());
    let to_corp = call.args.get(2).and_then(|v| v.as_int());
    let title = call.args.get(3).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let body = call.args.get(4).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let (reply_to, forwarded_from) = (call.args.get(5).and_then(|v| v.as_int()).unwrap_or(0), call.args.get(6).and_then(|v| v.as_int()).unwrap_or(0));
    let input = delivery::SendMail { to_chars, to_list, to_corp, title, body, reply_to, forwarded_from };
    delivery::send_mail(&c.db, &c.world, me, c.session.int("corpid"), input).await.map(Value::Int)
}


/// `GetBody(messageID, markRead)` -> the zlib-compressed UTF-8 body.
async fn body(c: &Client, id: i64, mark_read: bool) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let Some(text) = mail::body(&c.db, id, me).await.map_err(db_err)?
    else {
        return Ok(Value::None);
    };
    if mark_read {
        set_bits(&c.db, me, Selector::Ids, &[id], STATUS_READ, true).await?;
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(text.as_bytes()).map_err(|e| CallError::Internal(e.into()))?;
    Ok(Value::str(z.finish().map_err(|e| CallError::Internal(e.into()))?))
}

/// Set or clear status bits on the character's messages matching `which` ($2 = ids).
async fn set_bits(db: &Db, me: i64, which: Selector, ids: &[i64], bits: i32, on: bool) -> Result<(), CallError> {
    mail::set_bits(db, me, which, ids, bits, on, STATUS_TRASHED).await.map_err(db_err)?;
    Ok(())
}

/// Messages a selector covers: listed ids, a label, a mailing list (none), or all.
fn selector(method: &str) -> Selector {
    if method.ends_with("ByLabel") {
        Selector::Label
    } else if method.ends_with("ByList") {
        Selector::List
    } else if method.starts_with("MoveAll") || method.starts_with("MarkAll") {
        Selector::All
    } else {
        Selector::Ids
    }
}

async fn labels(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = mail::labels(&c.db, me).await.map_err(db_err)?;
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let id = r.label_id;
                (
                    Value::Int(id),
                    keyval([
                        ("labelID", Value::Int(id)),
                        ("name", Value::unicode(r.name.as_str())),
                        ("color", r.color.map(Value::Int).unwrap_or(Value::None)),
                    ]),
                )
            })
            .collect(),
    ))
}

async fn create_label(c: &Client, name: &str, color: Option<i64>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let used = mail::label_ids(&c.db, me).await.map_err(db_err)?;
    let Some(id) = (FIRST_CUSTOM_LABEL_BIT..CUSTOM_LABEL_BIT_END).map(|b| 1i32 << b).filter(|b| *b >= FIRST_CUSTOM_LABEL).find(|b| !used.contains(b)) else {
        return Err(notify_error(messages::LABEL_LIMIT_REACHED));
    };
    mail::insert_label(&c.db, me, id, name, color.map(|c| c as i32)).await.map_err(db_err)?;
    Ok(Value::Int(id as i64))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let db = &c.db;
    Some(match (call.service, call.method) {
        ("mailMgr", "SyncMail") => sync(c, int(1).unwrap_or(0)).await,
        ("mailMgr", "GetMailHeaders") => {
            let wanted = value_id_list(call.args.first());
            match mail::headers(db, me, &wanted).await {
                Ok(rows) => Ok(header_rows(&rows)),
                Err(e) => Err(db_err(e)),
            }
        }
        ("mailMgr", "SendMail") => send(c, call).await,
        ("mailMgr", "GetBody") => body(c, int(0).unwrap_or(0), call.args.get(1).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int() == Some(1))).await,
        ("mailMgr", m @ ("MarkAsRead" | "MarkAsReadByLabel" | "MarkAsReadByList" | "MarkAllAsRead")) => {
            let arg = if m.ends_with("ByLabel") { vec![int(0).unwrap_or(0)] } else { value_id_list(call.args.first()) };
            set_bits(db, me, selector(m), &arg, STATUS_READ, true).await.map(|_| Value::None)
        }
        ("mailMgr", m @ ("MarkAsUnread" | "MarkAsUnreadByLabel" | "MarkAsUnreadByList" | "MarkAllAsUnread")) => {
            let arg = if m.ends_with("ByLabel") { vec![int(0).unwrap_or(0)] } else { value_id_list(call.args.first()) };
            set_bits(db, me, selector(m), &arg, STATUS_READ, false).await.map(|_| Value::None)
        }
        ("mailMgr", m @ ("MoveToTrash" | "MoveToTrashByLabel" | "MoveToTrashByList" | "MoveAllToTrash")) => {
            let arg = if m.ends_with("ByLabel") { vec![int(0).unwrap_or(0)] } else { value_id_list(call.args.first()) };
            set_bits(db, me, selector(m), &arg, STATUS_TRASHED, true).await.map(|_| Value::None)
        }
        ("mailMgr", m @ ("MoveFromTrash" | "MoveAllFromTrash")) => {
            let arg = value_id_list(call.args.first());
            set_bits(db, me, selector(m), &arg, STATUS_TRASHED, false).await.map(|_| Value::None)
        }
        ("mailMgr", "EmptyTrash") => mail::empty_trash(db, me, STATUS_TRASHED).await.map(|_| Value::None).map_err(db_err),
        ("mailMgr", "DeleteMail") => {
            let wanted = value_id_list(call.args.first());
            mail::delete_mail(db, me, &wanted).await.map(|_| Value::None).map_err(db_err)
        }
        ("mailMgr", "GetLabels") => labels(c).await,
        // CreateLabel(name, color) -> labelID
        ("mailMgr", "CreateLabel") => {
            let name = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            create_label(c, &name, int(1)).await
        }
        // EditLabel(labelID, name, color)
        ("mailMgr", "EditLabel") => {
            let name = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned());
            let color = int(2).map(|c| c as i32);
            mail::edit_label(db, me, int(0).unwrap_or(0) as i32, name, color).await.map(|_| Value::None).map_err(db_err)
        }
        ("mailMgr", "DeleteLabel") => {
            let label = int(0).unwrap_or(0) as i32;
            match mail::delete_label(db, me, label).await {
                Ok(_) => mail::clear_label(db, me, label)
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err),
                Err(e) => Err(db_err(e)),
            }
        }
        // AssignLabels / RemoveLabels / ReplaceLabels([messageID], labelID)
        ("mailMgr", m @ ("AssignLabels" | "RemoveLabels" | "ReplaceLabels")) => {
            let wanted = value_id_list(call.args.first());
            let label = int(1).unwrap_or(0) as i32;
            let action = match m {
                "AssignLabels" => LabelAction::Assign,
                "RemoveLabels" => LabelAction::Remove,
                _ => LabelAction::Replace,
            };
            let result = mail::change_labels(db, me, &wanted, label, action, BUILTIN_LABEL_MASK).await;
            result.map(|_| Value::None).map_err(db_err)
        }
        _ => return None,
    })
}
