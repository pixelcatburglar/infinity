//! corpmgr public information and corporation delivery asset locations.
//! The client contracts and corporation assets UIs consume station locations from
//! `game.items` and loaded SDE station metadata. Other asset modes remain empty.

use super::*;
use crate::services::keyval;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let my_corp = c.session.int("corpid").unwrap_or(0);
    let arg_int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    Some(Ok(match (call.service, call.method) {
        ("corpmgr", "AuditMember") => return Some(crate::services::corps::audit::member_history(c, call).await),
        ("corpmgr", "GetCorporations") => return Some(corporation(c, arg_int(0).unwrap_or(my_corp)).await),
        ("corpmgr", "GetAssetInventory") => {
            if !call.args.get(1).and_then(Value::as_text).is_some_and(|mode| mode == "deliveries") {
                return Some(Ok(Value::list([])));
            }
            let corp = arg_int(0).unwrap_or(0);
            let me = c.session.int("charid").unwrap_or(0);
            let authority = match crate::services::corps::permissions::load(&c.db, me).await {
                Ok(Some(authority)) if authority.live && authority.corporation == corp => authority,
                Ok(_) => return Some(Ok(Value::list([]))),
                Err(error) => return Some(Err(error)),
            };
            let locations = match crate::dao::items::deliveries::locations(&c.db, corp).await {
                Ok(rows) => rows,
                Err(error) => return Some(Err(db_err(error))),
            };
            Value::list(locations.into_iter().filter_map(|row| {
                let station = c.sde.map.station(row.location_id)?;
                if authority.roles[0] & infinity_core::consts::ROLE_DIRECTOR == 0
                    && crate::services::corps::permissions::at_station(&authority, row.location_id) & infinity_core::consts::ROLE_DELIVERIES_VIEW == 0 { return None; }
                Some(keyval([
                    ("locationID", Value::Int(row.location_id)),
                    ("solarsystemID", station.solar_system_id.map(Value::Int).unwrap_or(Value::None)),
                    ("itemCount", Value::Int(row.item_count)),
                ]))
            }))
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

#[cfg(test)] mod tests;
