//! Online status for personal contacts, mirroring
//! `eve/client/script/ui/shared/comtool/onlineStatus.py`.
//! Addresses come from `game.char_contacts`; live presence is the process-local
//! World pilot registry. Cross-process presence and persistent presence are unsupported.

use std::collections::HashSet;

use marshal::{DbRowDescriptor, DbType, Value};
use tracing::warn;

use super::{crowset, db_err, Call, CallError, CallResult};
use crate::{client::Client, dao::characters::contacts, db::Db, space::World};

fn initial_state(contacts: &[contacts::Contact], online: &[i64]) -> Value {
    let online: HashSet<i64> = online.iter().copied().collect();
    let header = DbRowDescriptor::new([("contactID", DbType::I4), ("online", DbType::Bool)]);
    let rows = contacts.iter().map(|contact| {
        header.row(&[Value::Int(contact.contact_id), Value::Bool(online.contains(&contact.contact_id))])
    }).collect();
    crowset(&header, rows)
}

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "onlineStatus" { return None; }
    Some(match call.method {
        "GetOnlineStatus" => match call.args.first().and_then(Value::as_int) {
            Some(id) => Ok(Value::Bool(c.world.is_online(id))),
            None => Err(CallError::Internal(anyhow::anyhow!("GetOnlineStatus without character ID"))),
        },
        "GetInitialState" => match c.session.int("charid") {
            Some(id) => contacts::list(&c.db, id).await.map(|rows| initial_state(&rows, &c.world.online_pilots())).map_err(db_err),
            None => Err(CallError::Internal(anyhow::anyhow!("GetInitialState without character"))),
        },
        _ => return None,
    })
}

pub(crate) async fn presence_changed(db: &Db, world: &World, char_id: i64, online: bool) {
    match contacts::watchers_of(db, char_id).await {
        Ok(watchers) => {
            let event = if online { "OnContactLoggedOn" } else { "OnContactLoggedOff" };
            for watcher in watchers {
                if world.is_online(watcher) {
                    world.notify_pilot(watcher, event, vec![Value::Int(char_id)]);
                }
            }
        }
        Err(error) => warn!(char_id, "contact presence lookup failed: {error}"),
    }
}

#[cfg(all(test, feature = "tests-services"))]
mod tests;
