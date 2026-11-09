//! Personal `characterMiningLedger` as read by
//! `eve/client/script/ui/shared/ledger/ledgerSvc.py` and `ledgerEntry.py`.
//! Reads game.character_mining_ledger; corporate observers, RPC filters and
//! arguments are unsupported. The client applies its own filters.

use super::{db_err, filetime_now, keyval, notify_error, Call, CallError, CallResult, Client, Value};
use infinity_core::messages;
use crate::dao::mining_ledger::{self, Log};
use crate::services::user_error;

fn validate_request(character: Option<i64>, args: &[Value], kwargs: &Value) -> Result<i64, CallError> {
    let character = character.filter(|id| *id > 0)
        .ok_or_else(|| user_error(messages::MSG_CHARACTER_NOT_FOUND, vec![]))?;
    if !args.is_empty() || !match kwargs { Value::None => true, Value::Dict(v) => v.is_empty(), _ => false } {
        return Err(notify_error(messages::mining::INVALID_LEDGER_REQUEST));
    }
    Ok(character)
}

fn row(log: Log) -> Value {
    keyval([
        ("eventDate", Value::Int(log.event_time)),
        ("solarsystemID", Value::Int(log.solar_system_id)),
        ("typeID", Value::Int(i64::from(log.type_id))),
        ("quantity", Value::Int(log.quantity)),
        ("quantityWasted", Value::Int(log.quantity_wasted)),
    ])
}

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetCharacterLogs" => Some(get_character_logs(c, call).await),
        _ => None,
    }
}

async fn get_character_logs(c: &Client, call: &Call<'_>) -> CallResult {
    let character = validate_request(c.session.int("charid"), call.args, call.kwargs)?;
    let now = filetime_now();
    let rows = mining_ledger::list(&c.db, character, super::retention::cutoff(now), now).await.map_err(db_err)?;
    Ok(Value::List(rows.into_iter().map(row).collect()))
}

#[cfg(test)] mod tests;
