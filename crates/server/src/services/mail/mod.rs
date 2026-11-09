//! EVE mail and notifications (eve/client/script/ui/services/mail: mailSvc.py, notificationSvc.py).
//!
//! A message is stored once (game.mail_messages); each sender and recipient has a status row
//! (game.mail_status) with read/replied/forwarded/trashed bits and a label bitmask: Inbox,
//! Sent, Corporation and Alliance, then the character's own labels (game.mail_labels) at the
//! higher bits. The client keeps headers and bodies cached and syncs by message ID; bodies
//! travel zlib-compressed. The operator configures the maximum mail title length.
//! Mail reaches corporation members and persistent mailing-list members.
//!
//! Notifications (game.notifications) are what other systems post for a character
//! (`notify`): a type, a sender and YAML data the client's formatter for that type reads.

pub(crate) mod delivery;
mod mail_mgr;
mod mailing_lists_mgr;
pub(super) mod mailing_lists;
mod notification_mgr;

use super::{db_err, notify_error, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::db::{executor::Executor, outbox::Outbox};
use marshal::Value;

pub(super) fn text_argument(v: &Value) -> Result<String, CallError> {
    match v {
        Value::Unicode(s) => Ok(s.clone()),
        Value::Bytes(s) => String::from_utf8(s.clone()).map_err(|_| notify_error(infinity_core::messages::mail::INVALID_ARGUMENT)),
        _ => Err(notify_error(infinity_core::messages::mail::INVALID_ARGUMENT)),
    }
}

/// `eve/common/lib/appConst.py`: `mailStatusMaskRead`.
pub(super) const STATUS_READ: i32 = 1;

/// `eve/common/lib/appConst.py`: `mailingListBlocked`.
pub(super) const ACCESS_BLOCKED: i16 = 0;
/// `eve/common/lib/appConst.py`: `mailingListAllowed`.
pub(super) const ACCESS_ALLOWED: i16 = 1;
/// `eve/common/lib/appConst.py`: `mailingListMemberMuted`.
pub(super) const ROLE_MUTED: i16 = 0;
/// `eve/common/lib/appConst.py`: `mailingListMemberDefault`.
pub(super) const ROLE_MEMBER: i16 = 1;
/// `eve/common/lib/appConst.py`: `mailingListMemberOperator`.
pub(super) const ROLE_OPERATOR: i16 = 2;
/// `eve/common/lib/appConst.py`: `mailingListMemberOwner`.
pub(super) const ROLE_OWNER: i16 = 3;
/// `eve/common/lib/appConst.py`: `mailingListMaxNameSize`.
pub(super) const MAX_LIST_NAME: usize = 60;
/// `eve/common/lib/appConst.py`: `mailinglistMaxMembers`.
pub(super) const MAX_MEMBERS: usize = 3000;
/// `eve/common/lib/appConst.py`: `mailinglistMaxMembersUpdated`.
pub(super) const MAX_MEMBER_BATCH: usize = 1000;
/// `eve/common/lib/appConst.py`: `mailMaxRecipients`.
pub(super) const MAX_RECIPIENTS: usize = 50;
/// `eve/common/lib/appConst.py`: `mailMaxSubjectSize`.
pub(super) const MAX_TITLE: usize = 150;
/// `eve/common/lib/appConst.py`: `mailMaxTaggedBodySize`.
pub(super) const MAX_BODY: usize = 10000;
/// `eve/common/lib/appConst.py`: `mailStatusMaskAutomated`.
pub(super) const STATUS_AUTOMATED: i32 = 32;

/// Post a notification to a character (and show it at once if they're online). `data` is
/// the YAML mapping the client's formatter for `type_id` reads; `group` is its
/// notificationconst group.
pub async fn notify<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, receiver: i64, type_id: i32, group: i32, sender: i64, data: &str) -> Result<(), CallError> {
    let created = filetime_now();
    let id = crate::dao::mail::insert_notification(db, receiver, type_id, group, sender, created, data).await.map_err(db_err)?;
    outbox.notify_pilot(receiver, "OnNotificationReceived", vec![Value::Int(id), Value::Int(type_id as i64), Value::Int(sender), Value::Int(created), Value::str(data)]);
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "mailMgr" => mail_mgr::dispatch(c, call).await,
        "mailingListsMgr" => mailing_lists_mgr::dispatch(c, call).await,
        "notificationMgr" => notification_mgr::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)]
mod tests;
