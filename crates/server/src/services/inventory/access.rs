//! Inventory container binding and corporate custody authorization for
//! `eve/client/script/environment/invCache.py`. Uses `game.items` ancestry and
//! corporate membership and role records; foreign shared-bay permissions are not
//! modelled. Office custody is state-independent identity, but access also
//! requires a currently active lease: an impounded or released office denies
//! instead of falling through to a personal container.
use super::{CONTAINER_CORP_MARKET, CONTAINER_HANGAR};
use infinity_core::consts::{CORP_HANGAR_DIVISIONS, FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, ROLE_DELIVERIES_TAKE, ROLE_DELIVERIES_VIEW, ROLE_HANGAR_CAN_QUERY_1, ROLE_HANGAR_CAN_TAKE_1};
use crate::db::executor::Executor;
use crate::sde::Sde;
use crate::services::{db_err, CallError};
use marshal::Value;
/// Which container a bound inventory lists.
#[derive(Clone)]
pub(crate) enum Container {
    /// The character's hangar in a station.
    Hangar { station_id: i64, owner_id: i64 },
    /// Corporation-owned deliveries at a station.
    Deliveries { station_id: i64, owner_id: i64 },
    /// Any item acting as a container (ship, character, can...).
    Item(i64),
}

pub(super) fn container_of(params: Option<&Value>) -> Option<Container> {
    let p = params?.items()?;
    match p {
        [Value::Int(station), Value::Int(CONTAINER_HANGAR), Value::Int(owner)] => {
            Some(Container::Hangar { station_id: *station, owner_id: *owner })
        }
        [Value::Int(station), Value::Int(CONTAINER_CORP_MARKET), Value::Int(owner)] => {
            Some(Container::Deliveries { station_id: *station, owner_id: *owner })
        }
        [Value::Int(item)] => Some(Container::Item(*item)),
        _ => None,
    }
}

pub(super) fn corp_hangar_role(flag: i32, take: bool) -> Option<i64> {
    if flag == FLAG_CORP_DELIVERIES {
        Some(if take { ROLE_DELIVERIES_TAKE } else { ROLE_DELIVERIES_VIEW })
    } else if (FLAG_CORP_SAG1..FLAG_CORP_SAG1 + CORP_HANGAR_DIVISIONS).contains(&flag) {
        let base = if take { ROLE_HANGAR_CAN_TAKE_1 } else { ROLE_HANGAR_CAN_QUERY_1 };
        Some(base << (flag - FLAG_CORP_SAG1))
    } else {
        None
    }
}

pub(in crate::services) async fn may_view_corp_hangar(ex: &(impl Executor + ?Sized), char_id: i64, corp_id: i64, station: i64, flag: i32) -> Result<bool, CallError> {
    match corp_hangar_role(flag, false) {
        Some(role) => super::super::corps::permissions::has_at(ex, char_id, corp_id, station, role).await,
        None => Ok(false),
    }
}

pub(in crate::services) async fn may_take_corp_hangar(ex: &(impl Executor + ?Sized), char_id: i64, corp_id: i64, station: i64, flag: i32) -> Result<bool, CallError> {
    match corp_hangar_role(flag, true) {
        Some(role) => super::super::corps::permissions::has_at(ex, char_id, corp_id, station, role).await,
        None => Ok(false),
    }
}

/// Resolve the office division or station Deliveries governing an item's ancestry.
/// Office custody is state-independent identity: an inactive lease still returns
/// the recognized (corporation, office, flag) so callers can deny rather than
/// fall through to a personal-container rule.
pub(in crate::services) async fn corporate_custody(ex: &(impl Executor + ?Sized), sde: Option<&Sde>, id: i64) -> Result<Option<(i64, i64, i32)>, CallError> {
    let rows = crate::dao::items::deliveries::ancestry_many(ex, &[id]).await.map_err(db_err)?;
    for row in &rows {
        if let Some(office) = &row.office {
            // Custody binds the office item and the division flag of its direct
            // child on this chain.
            let flag = rows.iter().find(|child| child.ancestor.location_id == row.ancestor.item_id)
                .map(|child| child.ancestor.flag_id).unwrap_or(row.ancestor.flag_id);
            return Ok(Some((office.corporation_id, row.ancestor.item_id, flag)));
        }
        if row.ancestor.flag_id == FLAG_CORP_DELIVERIES && sde.is_none_or(|sde| sde.map.station(row.ancestor.location_id).is_some())
            && super::super::corps::is_player_corp(row.ancestor.owner_id) {
            return Ok(Some((row.ancestor.owner_id, row.ancestor.location_id, row.ancestor.flag_id)));
        }
    }
    Ok(None)
}
/// Map a corporate custody container to the station where its roles apply.
pub(in crate::services) async fn custody_station(ex: &(impl Executor + ?Sized), location: i64, flag: i32) -> Result<Option<i64>, CallError> {
    if flag == FLAG_CORP_DELIVERIES { Ok(Some(location)) }
    else { crate::dao::offices::station(ex, location).await.map_err(db_err) }
}


pub(in crate::services) async fn custody_allowed(ex: &(impl Executor + ?Sized), sde: &Sde, id: i64, me: i64, station: Option<i64>, take: bool) -> Result<Option<bool>, CallError> {
    let Some((corp, location, flag)) = corporate_custody(ex, Some(sde), id).await? else { return Ok(None) };
    let Some(target_station) = custody_station(ex, location, flag).await? else { return Ok(Some(false)) };
    let role = if take { may_take_corp_hangar(ex, me, corp, target_station, flag).await? } else { may_view_corp_hangar(ex, me, corp, target_station, flag).await? };
    if flag == FLAG_CORP_DELIVERIES {
        return Ok(Some(role && station == Some(location)));
    }
    // An office division also requires a currently active lease.
    let now = crate::client::filetime_now();
    match crate::dao::offices::get(ex, location).await.map_err(db_err)? {
        Some(lease) => Ok(Some(role && lease.is_active(now))),
        None => Ok(Some(false)),
    }
}
