//! EVE mail and notifications (eve/client/script/ui/services/mail: mailSvc.py, notificationSvc.py).
//!
//! A message is stored once (game.mail_messages); each sender and recipient has a status row
//! (game.mail_status) with read/replied/forwarded/trashed bits and a label bitmask: Inbox,
//! Sent, Corporation and Alliance, then the character's own labels (game.mail_labels) at the
//! higher bits. The client keeps headers and bodies cached and syncs by message ID; bodies
//! travel zlib-compressed. The operator configures the maximum mail title length.
//! Mail to a corporation reaches its player members; mailing lists aren't supported.
//!
//! Notifications (game.notifications) are what other systems post for a character
//! (`notify`): a type, a sender and YAML data the client's formatter for that type reads.

pub(crate) mod delivery;
mod mail_mgr;
mod notification_mgr;

use super::{db_err, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::db::{executor::Executor, outbox::Outbox};
use marshal::Value;

/// Client appConst.mailStatusMask read bit.
pub(super) const STATUS_READ: i32 = 1;

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
        "notificationMgr" => notification_mgr::dispatch(c, call).await,
        _ => None,
    }
}
