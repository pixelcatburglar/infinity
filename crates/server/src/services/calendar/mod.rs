//! Calendar services (eve/client/script/ui/services/eveCalendarsvc.py).
//! Events and responses use game.calendar_events and game.calendar_responses.
//! The operator configures event title, description and initial invitee limits.
//! Alliance events are not supported.
mod calendar_mgr;
mod calendar_proxy;

use super::{db_err, keyval, notify_error, value_id_list, value_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use infinity_core::consts::{FILETIME_SECOND, FILETIME_UNIX_EPOCH, ROLE_DIRECTOR};
use crate::dao::calendar as dao;
use crate::db::outbox::{dispatch_world, Outbox};
use infinity_core::messages;
use marshal::{DbRowDescriptor, DbType, Value};

const RESPONSE_DELETED: i32 = 1;
const RESPONSE_UNDECIDED: i32 = 3;
const RESPONSE_ACCEPTED: i32 = 4;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "calendarProxy" => calendar_proxy::dispatch(c, call).await,
        "calendarMgr" => calendar_mgr::dispatch(c, call).await,
        _ => None,
    }
}
