//! corpmgr public information and corporation assets from actual item ancestry,
//! office leases and station/system facts. Structures are not supported.

use super::*;
use crate::services::notify_error;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let my_corp = c.session.int("corpid").unwrap_or(0);
    let arg_int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    Some(Ok(match (call.service, call.method) {
        ("corpmgr", "AuditMember") => return Some(crate::services::corps::audit::member_history(c, call).await),
        ("corpmgr", "GetCorporations") => return Some(corporation(c, arg_int(0).unwrap_or(my_corp)).await),
        ("corpmgr", "GetAssetInventory") => {
            let corp = arg_int(0).unwrap_or(0);
            if !crate::services::corps::is_player_corp(corp) { return Some(Err(notify_error(infinity_core::messages::INSUFFICIENT_ROLE_FOR_OPERATION))); }
            let Some(mode) = call.args.get(1).and_then(Value::as_text).and_then(|mode| super::assets::AssetRootKind::parse(&mode)) else {
                return Some(Err(notify_error(infinity_core::messages::INSUFFICIENT_ROLE_FOR_OPERATION)));
            };
            let mut assets = match super::assets::Snapshot::load(c, corp).await {
                Ok(assets) => assets, Err(error) => return Some(Err(error)),
            };
            Value::list(assets.locations(mode, None))
        }
        ("corpmgr", "GetAssetInventoryForLocation") => {
            let corp = arg_int(0).unwrap_or(0);
            if !crate::services::corps::is_player_corp(corp) { return Some(Err(notify_error(infinity_core::messages::INSUFFICIENT_ROLE_FOR_OPERATION))); }
            let location = arg_int(1).unwrap_or(0);
            let Some(mode) = call.args.get(2).and_then(Value::as_text).and_then(|mode| super::assets::AssetRootKind::parse(&mode)) else {
                return Some(Err(notify_error(infinity_core::messages::INSUFFICIENT_ROLE_FOR_OPERATION)));
            };
            let mut assets = match super::assets::Snapshot::load(c, corp).await {
                Ok(assets) => assets, Err(error) => return Some(Err(error)),
            };
            let rows = assets.detail(mode, location);
            crate::services::crowset(&crate::services::item_descriptor(), rows)
        }
        ("corpmgr", "SearchAssets") => {
            if !crate::services::corps::is_player_corp(my_corp) { return Some(Err(notify_error(infinity_core::messages::INSUFFICIENT_ROLE_FOR_OPERATION))); }
            let Some(mode) = call.args.first().and_then(Value::as_text).and_then(|mode| super::assets::AssetRootKind::parse(&mode)) else {
                return Some(Err(notify_error(infinity_core::messages::INSUFFICIENT_ROLE_FOR_OPERATION)));
            };
            let optional = |index: usize| -> Result<Option<i64>, CallError> {
                match call.args.get(index) {
                    None | Some(Value::None) => Ok(None),
                    Some(value) => value.as_int().map(Some).ok_or_else(|| notify_error(infinity_core::messages::INSUFFICIENT_ROLE_FOR_OPERATION)),
                }
            };
            let filter = match (optional(1), optional(2), optional(3), optional(4)) {
                (Ok(category), Ok(group), Ok(kind), Ok(quantity)) => super::assets::Filter { category, group, kind, quantity: quantity.map(|n| n.max(0)) },
                _ => return Some(Err(notify_error(infinity_core::messages::INSUFFICIENT_ROLE_FOR_OPERATION))),
            };
            let mut assets = match super::assets::Snapshot::load(c, my_corp).await {
                Ok(assets) => assets, Err(error) => return Some(Err(error)),
            };
            Value::list(assets.locations(mode, Some(filter)))
        }
        // Show info on a corporation (corporationInfoWindow reads ceoID, stationID, shares...).
        ("corpmgr", "GetPublicInfo") => return Some(corporation(c, arg_int(0).unwrap_or(my_corp)).await),
        ("corpmgr", "GetCorporationIDForCharacter") => {
            let id = arg_int(0).unwrap_or(0);
            let r = crate::dao::characters::crimewatch::corporation(&c.db, id).await;
            match r {
                Ok(r) => r.or_else(|| c.sde.characters.npc_character(id).and_then(|npc| npc.corporation_id)).map(Value::Int).unwrap_or(Value::None),
                Err(e) => return Some(Err(db_err(e))),
            }
        }
        _ => return None,
    }))
}

#[cfg(all(test, feature = "tests-content"))] mod tests;
