//! Installed augmentation reads for `skillHandler`, consumed by `godma.py`.
//! Uses `game.items` through the active fit relation and persisted brain metadata.
//! Legacy boosters without timing metadata have no known expiry or duration.
use super::{db_err, keyval, CallResult};
use crate::client::{filetime_now, ms_from_filetime_ticks, Client};
use infinity_core::consts::{FLAG_BOOSTER, FLAG_IMPLANT};
use crate::dao::items::{brain, fit};
use marshal::Value;

pub(crate) async fn get_implants(c: &Client, char_id: i64) -> CallResult {
    let rows = fit::brain_items(&c.db, char_id, filetime_now()).await.map_err(db_err)?;
    Ok(Value::Dict(rows.into_iter().filter(|row| row.flag_id == FLAG_IMPLANT).map(|row| {
        (Value::Int(row.item_id), keyval([
            ("itemID", Value::Int(row.item_id)),
            ("typeID", Value::Int(i64::from(row.type_id))),
        ]))
    }).collect()))
}

pub(crate) async fn get_boosters(c: &Client, char_id: i64) -> CallResult {
    let rows = fit::brain_items(&c.db, char_id, filetime_now()).await.map_err(db_err)?;
    Ok(Value::Dict(rows.into_iter().filter(|row| row.flag_id == FLAG_BOOSTER).map(booster_record).collect()))
}

fn booster_record(row: brain::BrainItem) -> (Value, Value) {
    let duration = row.expires_at.zip(row.activated_at)
        .and_then(|(expiry, activation)| expiry.checked_sub(activation))
        .map(ms_from_filetime_ticks).map(Value::Int).unwrap_or(Value::None);
    let mut effects = row.effect_ids.unwrap_or_default();
    effects.sort_unstable();
    (Value::Int(row.item_id), keyval([
        ("itemID", Value::Int(row.item_id)),
        ("boosterTypeID", Value::Int(i64::from(row.type_id))),
        ("expiryTime", row.expires_at.map(Value::Int).unwrap_or(Value::None)),
        ("boosterDuration", duration),
        ("sideEffectIDs", Value::list(effects.into_iter().map(|id| Value::Int(i64::from(id))))),
    ]))
}

#[cfg(test)] mod tests;
