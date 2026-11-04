//! Shared ordinary and automated mail delivery, mirroring `eve/client/script/ui/services/mail/mailSvc.py`.
//! Uses mail message/status tables and active character, corporation and mailing-list membership.
//! Message, status and reply/forward writes commit together before notifications.
//! Alliance delivery is not supported.

use super::super::{db_err, notify_error, CallError};
use super::{mailing_lists, MAX_BODY, MAX_RECIPIENTS, MAX_TITLE, STATUS_AUTOMATED, STATUS_READ};
use crate::client::filetime_now;
use crate::consts::FILETIME_MINUTE;
use crate::dao::{characters, mail};
use crate::db::{executor::Executor, outbox::{dispatch_world, Outbox}, Db};
use crate::messages;
use crate::space::World;
use marshal::Value;
use std::collections::BTreeSet;
use std::sync::Arc;

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

pub(crate) struct SendMail {
    pub to_chars: Vec<i64>,
    pub to_list: Option<i64>,
    pub to_corp: Option<i64>,
    pub title: String,
    pub body: String,
    pub reply_to: i64,
    pub forwarded_from: i64,
}

pub(crate) struct Delivery {
    pub sender: i64,
    pub to_chars: Vec<i64>,
    pub to_list: Option<i64>,
    pub to_corp: Option<i64>,
    pub title: String,
    pub body: String,
    pub sent: i64,
    pub recipients: Vec<i64>,
    pub automated: bool,
}

pub(crate) fn validate_content(title: &str, body: &str) -> Result<(), CallError> {
    let configured = usize::try_from(crate::config::game().mail.title_max_len).unwrap_or(usize::MAX);
    if title.chars().count() > configured.min(MAX_TITLE) || body.chars().count() > MAX_BODY {
        return Err(notify_error(messages::mail::INVALID_CONTENT));
    }
    Ok(())
}

/// Insert one message and its deduplicated status rows in the caller's transaction.
pub(crate) async fn deliver(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, input: &Delivery) -> Result<i64, CallError> {
    deliver_to(ex, outbox, input, &input.recipients).await
}

async fn deliver_to(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, input: &Delivery, group: &[i64]) -> Result<i64, CallError> {
    let to_text = input.to_chars.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let id = mail::insert_message(ex, input.sender, &to_text, input.to_list, input.to_corp, &input.title, &input.body, input.sent).await.map_err(db_err)?;
    let mut recipients: BTreeSet<i64> = group.iter().copied().collect();
    recipients.extend(input.to_chars.iter().copied());
    if !input.automated {
        recipients.insert(input.sender);
    }
    // mailSvc.py:OnMailSent derives labels from the explicit header and category, not expansion.
    let rows: Vec<_> = recipients.iter().map(|&ch| {
        let explicit = input.to_chars.contains(&ch);
        let mut labels = if explicit { LABEL_INBOX } else { 0 };
        if input.to_corp.is_some() { labels |= LABEL_CORPORATION; }
        if !input.automated && ch == input.sender { labels |= LABEL_SENT; }
        let status = if input.automated { STATUS_AUTOMATED } else if ch == input.sender && !explicit && input.to_list.is_none() && input.to_corp.is_none() { STATUS_READ } else { 0 };
        (ch, status, labels)
    }).collect();
    mail::insert_statuses(ex, id, &rows).await.map_err(db_err)?;
    let explicit = Value::list(input.to_chars.iter().copied().map(Value::Int));
    for ch in recipients {
        if !input.automated && ch == input.sender { continue; }
        outbox.notify_pilot(ch, "OnMailSent", vec![
            Value::Int(id), Value::Int(input.sender), Value::Int(input.sent), explicit.clone(),
            input.to_list.map(Value::Int).unwrap_or(Value::None),
            input.to_corp.map(Value::Int).unwrap_or(Value::None),
            Value::unicode(input.title.as_str()),
            Value::Int(if input.automated { STATUS_AUTOMATED as i64 } else { 0 }),
        ]);
    }
    Ok(id)
}

pub(crate) async fn send_mail(db: &Db, world: &World, me: i64, _corpid: Option<i64>, mut input: SendMail) -> Result<i64, CallError> {
    if me <= 0 { return Err(notify_error(messages::mail::SELECT_CHARACTER)); }
    if input.to_chars.len() > MAX_RECIPIENTS { return Err(notify_error(messages::TOO_MANY_MAIL_RECIPIENTS)); }
    if input.to_chars.iter().any(|id| *id <= 0) || input.to_list.is_some_and(|id| id <= 0) || input.to_corp.is_some_and(|id| id <= 0) || input.reply_to < 0 || input.forwarded_from < 0 || (input.to_list.is_some() && input.to_corp.is_some()) {
        return Err(notify_error(messages::mail::INVALID_ARGUMENT));
    }
    if input.to_chars.is_empty() && input.to_list.is_none() && input.to_corp.is_none() {
        return Err(notify_error(messages::MAIL_RECIPIENT_REQUIRED));
    }
    validate_content(&input.title, &input.body)?;
    input.to_chars.sort_unstable();
    input.to_chars.dedup();
    let sent = filetime_now() / FILETIME_MINUTE * FILETIME_MINUTE;
    let mut originals = [(input.reply_to, STATUS_REPLIED), (input.forwarded_from, STATUS_FORWARDED)];
    originals.sort_by_key(|(id, _)| *id);
    let delivery = Arc::new(Delivery {
        sender: me, to_chars: input.to_chars, to_list: input.to_list, to_corp: input.to_corp,
        title: input.title, body: input.body, sent, recipients: Vec::new(), automated: false,
    });
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let input = Arc::clone(&delivery);
            Box::pin(async move {
                if input.to_list.is_none() && characters::lookup::find(tx, "", Some(me)).await.map_err(db_err)?.is_none() {
                    return Err(notify_error(messages::mail::SELECT_CHARACTER));
                }
                for &ch in &input.to_chars {
                    if characters::lookup::find(tx, "", Some(ch)).await.map_err(db_err)?.is_none() {
                        return Err(notify_error(messages::mail::INVALID_MEMBER));
                    }
                }
                let recipients = if let Some(list_id) = input.to_list {
                    let list = crate::dao::mailing_lists::get_for_update(tx, list_id).await.map_err(db_err)?
                        .ok_or_else(|| notify_error(messages::mail::LIST_NOT_FOUND))?;
                    mailing_lists::authorize_sender(tx, &list, me).await?;
                    crate::dao::mailing_lists::recipient_ids(tx, list_id).await.map_err(db_err)?
                } else if let Some(corp) = input.to_corp {
                    if characters::crimewatch::corporation(tx, me).await.map_err(db_err)? != Some(corp) {
                        return Err(notify_error(messages::MAIL_ONLY_TO_OWN_CORPORATION));
                    }
                    characters::members::active_ids(tx, corp).await.map_err(db_err)?
                } else { Vec::new() };
                let id = deliver_to(tx, outbox, &input, &recipients).await?;
                for (original, bit) in originals {
                    if original > 0 { mail::mark_original(tx, original, me, bit).await.map_err(db_err)?; }
                }
                Ok(id)
            })
        },
        |id, outbox| async move { dispatch_world(id, outbox, world) },
    ).await
}

#[cfg(test)]
mod tests;
