//! Shared mail delivery, mirroring `eve/client/script/ui/services/mail/mailSvc.py`.
//! Uses the mail DAO's message and status tables and character membership.
//! Message, status and reply/forward writes commit together before notifications.
//! Mailing lists are not supported.

use super::super::{db_err, notify_error, CallError};
use super::STATUS_READ;
use crate::client::filetime_now;
use crate::consts::FILETIME_MINUTE;
use crate::dao::mail;
use crate::db::Db;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
use crate::space::World;
use marshal::Value;
use tracing::info;

/// Client appConst.mailStatusMask replied bit.
const STATUS_REPLIED: i32 = 2;
/// Client appConst.mailStatusMask forwarded bit.
const STATUS_FORWARDED: i32 = 4;
/// Client appConst.mailLabel inbox bit.
const LABEL_INBOX: i32 = 1;
/// Client appConst.mailLabel sent bit.
const LABEL_SENT: i32 = 2;
/// Client appConst.mailLabel corporation bit.
const LABEL_CORPORATION: i32 = 4;
/// Client appConst.mailMaxRecipients.
const MAX_RECIPIENTS: usize = 50;

/// Player members of a corporation.
async fn corp_members(db: &Db, corp: i64) -> Result<Vec<i64>, CallError> {
    crate::dao::characters::members::active_ids(db, corp).await.map_err(db_err)
}

pub(crate) struct SendMail {
    pub to_chars: Vec<i64>,
    pub to_list: Option<i64>,
    pub to_corp: Option<i64>,
    pub title: String,
    pub body: String,
    pub reply_to: i64,
    pub forwarded_from: i64,
}

pub(crate) async fn send_mail(db: &Db, world: &World, me: i64, corpid: Option<i64>, input: SendMail) -> Result<i64, CallError> {
    let SendMail { to_chars, to_list, to_corp, title, body, reply_to, forwarded_from } = input;
    let title = title.chars().take(usize::try_from(crate::config::game().mail.title_max_len).unwrap_or(usize::MAX)).collect::<String>();
    if to_list.is_some() {
        return Err(notify_error(messages::MAILING_LISTS_UNSUPPORTED));
    }
    if to_chars.is_empty() && to_corp.is_none() {
        return Err(notify_error(messages::MAIL_RECIPIENT_REQUIRED));
    }
    if to_chars.len() > MAX_RECIPIENTS {
        return Err(notify_error(messages::TOO_MANY_MAIL_RECIPIENTS));
    }
    if let Some(corp) = to_corp {
        if corpid != Some(corp) {
            return Err(notify_error(messages::MAIL_ONLY_TO_OWN_CORPORATION));
        }
    }
    let sent = filetime_now() / FILETIME_MINUTE * FILETIME_MINUTE;
    let to_text = to_chars.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    // Everyone involved gets a status row; the sender's starts read.
    let mut boxes: Vec<(i64, i32, i32)> = vec![];
    for ch in &to_chars {
        boxes.push((*ch, 0, LABEL_INBOX));
    }
    if let Some(corp) = to_corp {
        for ch in corp_members(db, corp).await? {
            match boxes.iter_mut().find(|b| b.0 == ch) {
                Some(b) => b.2 |= LABEL_CORPORATION,
                None => boxes.push((ch, 0, LABEL_CORPORATION)),
            }
        }
    }
    match boxes.iter_mut().find(|b| b.0 == me) {
        Some(b) => b.2 |= LABEL_SENT,
        None => boxes.push((me, STATUS_READ, LABEL_SENT)),
    }
    // Stable sorting preserves which status wins for duplicate recipients.
    let mut sorted_boxes = boxes.clone();
    sorted_boxes.sort_by_key(|b| b.0);
    let mut originals = [(reply_to, STATUS_REPLIED), (forwarded_from, STATUS_FORWARDED)];
    originals.sort_by_key(|(id, _)| *id);
    let to_list_value = Value::list(to_chars.iter().map(|i| Value::Int(*i)));
    let inputs = std::sync::Arc::new((to_text, title, body, boxes, sorted_boxes, to_list_value));
    let id = db.transaction(
        |tx, outbox: &mut Outbox| {
            let inputs = std::sync::Arc::clone(&inputs);
            Box::pin(async move {
                let (to_text, title, body, boxes, sorted_boxes, to_list_value) = &*inputs;
                let id = mail::insert_message(tx, me, to_text, to_corp, title, body, sent).await.map_err(db_err)?;
                for (ch, status, labels) in sorted_boxes {
                    mail::insert_status(tx, id, *ch, *status, *labels).await.map_err(db_err)?;
                }
                for (original, bit) in originals {
                    if original > 0 {
                        mail::mark_original(tx, original, me, bit).await.map_err(db_err)?;
                    }
                }
                for (ch, _, _) in boxes.iter().filter(|b| b.0 != me) {
                    outbox.notify_pilot(
                        *ch,
                        "OnMailSent",
                        vec![
                            Value::Int(id),
                            Value::Int(me),
                            Value::Int(sent),
                            to_list_value.clone(),
                            Value::None,
                            to_corp.map(Value::Int).unwrap_or(Value::None),
                            Value::unicode(title.as_str()),
                            Value::Int(0),
                        ],
                    );
                }
                Ok(id)
            })
        },
        |id, outbox| async move { dispatch_world(id, outbox, world) },
    ).await?;
    info!("mail: {me} sent message {id} to {} recipients", inputs.3.len() - 1);
    Ok(id)
}
