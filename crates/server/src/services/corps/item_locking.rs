//! Corporation item lock queries for `itemLocking` (itemLocking.py).
//! Read-only views over `game.corp_item_locks` filtered by the caller's current
//! database corporation; pending vote-case reservations and other corporations'
//! locks are never reported. The client caches both answers and mutates the set on
//! OnLockedItemChange, so only enacted locks appear here.

use super::*;
use crate::dao::items::{corporation_locks, loading::ItemQuery};
use crate::services::items::load_items;

fn invalid() -> CallError { notify_error(messages::corporations::INVALID_CORPORATION_ARGUMENTS) }

/// itemLocking.py: the corporation's enacted locks at one location, keyed by item.
async fn items_by_location(c: &Client, corporation: i64, location: i64) -> CallResult {
    let mut entries = Vec::new();
    if is_player_corp(corporation) {
        let locks = corporation_locks::list(&c.db, corporation, location).await.map_err(db_err)?;
        let ids: Vec<i64> = locks.iter().map(|lock| lock.item_id).collect();
        if !ids.is_empty() {
            for item in load_items(&c.db, &c.sde, ItemQuery::Ids(&ids)).await.map_err(db_err)? {
                entries.push((Value::Int(item.item_id), item.row()));
            }
        }
    }
    Ok(Value::Dict(entries))
}

/// itemLocking.py: the mutable `set` of stations holding the corporation's locks.
async fn locked_locations(c: &Client, corporation: i64) -> CallResult {
    let stations = if is_player_corp(corporation) {
        corporation_locks::locations(&c.db, corporation).await.map_err(db_err)?
    } else {
        Vec::new()
    };
    Ok(Value::reduce("__builtin__.set", vec![Value::list(stations.into_iter().map(Value::Int))], None))
}

async fn handle(c: &Client, call: &Call<'_>) -> CallResult {
    let actor = c.session.int("charid").unwrap_or(0);
    let (corporation, _) = membership(&c.db, actor).await?;
    match call.method {
        "GetItemsByLocation" => {
            let location = match call.args {
                [Value::Int(location)] => *location,
                _ => return Err(invalid()),
            };
            items_by_location(c, corporation, location).await
        }
        "GetLockedItemLocations" => {
            if !call.args.is_empty() { return Err(invalid()); }
            locked_locations(c, corporation).await
        }
        _ => Err(invalid()),
    }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "itemLocking" { return None; }
    match call.method {
        "GetItemsByLocation" | "GetLockedItemLocations" => Some(handle(c, call).await),
        _ => None,
    }
}
