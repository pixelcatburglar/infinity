//! `sharedSettingsMgr` stores and retrieves permanent filter and broadcast snapshots.
//! `sharedSettings/util.py:GetDataFromSettingKey`,
//! `eve/client/script/ui/shared/inventory/filterSvc.py:LoadSharedFilter` and
//! `eve/client/script/parklife/fleetSvc.py:LoadSharedBroadcastSettings` consume links.
//! Data comes from client payloads and `game.shared_setting_links`. Any authenticated
//! complete-key holder can read; private access, revocation, expiry and overview
//! links are unsupported.
use marshal::Value;
use crate::{client::Client, dao::shared_settings::{self, LinkKey, NewLink}, messages::settings_sharing as messages, services::{Call, CallResult, keyval, notify_error}};
use super::{codec, BROADCAST, ITEMS};

/// Filter and condition triples, and broadcast entries, as unpacked by filterSvc and fleetSvc.
const TRIPLE_ARITY: usize = 3;
/// The fleet color picker produces RGB; BroadcastEntry appends alpha before SetRGBA.
/// Sources: `evefleet/client/ui/color_picker.py:FleetColorPickerCont` and
/// `eve/client/script/ui/shared/fleet/fleetbroadcast.py:BroadcastEntry.Load`.
const COLOR_COMPONENTS: usize = 3;

fn sequence(value: &Value) -> Option<&[Value]> {
    match value { Value::List(v) | Value::Tuple(v) => Some(v), _ => None }
}
fn valid_shape(kind: i32, value: &Value) -> bool {
    let Value::Dict(entries) = value else { return false };
    let field = if kind == ITEMS { "filter" } else { "broadcastSettings" };
    let Some((_, value)) = entries.iter().find(|(key, _)| codec::text(key) == Some(field)) else { return false };
    let Some(items) = sequence(value) else { return false };
    if kind == ITEMS {
        items.len() == TRIPLE_ARITY && codec::text(&items[0]).is_some() && matches!(items[1], Value::Int(_))
            && sequence(&items[2]).is_some_and(|conditions| conditions.iter().all(|v| sequence(v).is_some_and(|v| v.len() == TRIPLE_ARITY && matches!(v[0], Value::Int(_)) && matches!(v[1], Value::Int(_)))))
    } else {
        items.iter().all(|v| sequence(v).is_some_and(|v| v.len() == TRIPLE_ARITY && (matches!(v[0], Value::Int(_)) || codec::text(&v[0]).is_some()) && matches!(v[1], Value::Bool(_)) && (matches!(v[2], Value::None) || sequence(&v[2]).is_some_and(|color| color.len() == COLOR_COMPONENTS && color.iter().all(|component| match component { Value::Int(_) => true, Value::Float(n) => n.is_finite(), _ => false })))))
    }
}
fn invalid() -> crate::services::CallError { notify_error(messages::INVALID) }

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if !matches!(call.method, "StoreSettingLinkAndGetID" | "GetStoredSharedSetting") { return None; }
    Some(handle(c, call).await)
}
async fn handle(c: &mut Client, call: &Call<'_>) -> CallResult {
    if c.login.user_id <= 0 { return Err(notify_error(messages::AUTH)); }
    if !matches!(call.kwargs, Value::None) && !matches!(call.kwargs, Value::Dict(v) if v.is_empty()) { return Err(invalid()); }
    match call.method {
        // `eve/client/script/ui/shared/inventory/filterSvc.py:ItemFilterDragData.get_link`
        // and `eve/client/script/ui/control/eveEditPlainText.py:OnDropDataDelegate`
        // submit the setting type and data.
        "StoreSettingLinkAndGetID" => {
            let [Value::Int(kind), value] = call.args else { return Err(invalid()) };
            let kind = i32::try_from(*kind).map_err(|_| invalid())?;
            if !matches!(kind, ITEMS | BROADCAST) { return Err(invalid()); }
            let policy = &crate::config::game().shared_settings;
            let (payload, hash) = codec::encode(value, policy.max_payload_bytes, policy.max_nesting_depth).map_err(|_| invalid())?;
            if !valid_shape(kind, value) { return Err(invalid()); }
            let stored = shared_settings::insert(&c.db, &NewLink { setting_type_id: kind, hash_value: &hash, payload: &payload, creator_user_id: c.login.user_id }).await?;
            Ok(keyval([("hashvalue", Value::str(hash)), ("sqID", Value::Int(stored.sq_id)), ("settingTypeID", Value::Int(i64::from(kind)))]))
        }
        // `sharedSettings/util.py:GetDataFromSettingKey` passes one full key;
        // GetSettingKeyFromKeyVal reads hashvalue, sqID and settingTypeID.
        "GetStoredSharedSetting" => {
            let [key] = call.args else { return Err(invalid()) };
            let Some([hash, Value::Int(sq_id), Value::Int(kind)]) = sequence(key) else { return Err(invalid()) };
            let hash = codec::text(hash).ok_or_else(invalid)?;
            let Ok(kind) = i32::try_from(*kind) else { return Ok(Value::None) };
            let result = shared_settings::get(&c.db, &LinkKey { sq_id: *sq_id, hash_value: hash, setting_type_id: kind }).await?;
            Ok(result.map_or(Value::None, |row| Value::str(row.payload)))
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
