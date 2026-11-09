//! Industry owner, location and wallet authorization for station jobs.
//! Mirrors industryCommon.py and industrySvc.py in the client industry package.
//! Reads current corporation membership, office items and wallet roles through service/DAO APIs.
//! Structures, nested containers and corporation offices at other facilities are not supported.

use super::{field, Facility};
use infinity_core::consts::{ACCOUNT_CASH, CORP_HANGAR_DIVISIONS, FLAG_CORP_SAG1, FLAG_HANGAR, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_FACTORY_MANUFACTURING, ROLE_FACTORY_SCIENCE, ROLE_HANGAR_CAN_TAKE_1};
use crate::dao::items::loading::ItemQuery;
use crate::db::executor::Executor;
use infinity_core::messages;
use crate::sde::industry::{COPYING, INVENTION, MANUFACTURING, RESEARCH_MATERIAL, RESEARCH_TIME};
use crate::sde::Sde;
use crate::services::{corps, corpwallet, db_err, items::load_items, offices, user_error, CallError};
use marshal::Value;

#[derive(Clone)]
pub(super) struct Location {
    pub(super) item_id: i64,
    pub(super) type_id: i64,
    pub(super) owner_id: i64,
    pub(super) flag_id: i32,
    pub(super) can_view: bool,
    pub(super) can_take: bool,
}

impl Location {
    pub(super) fn value(&self) -> Value {
        Value::reduce("industry.job.Location", vec![], Some(Value::dict([
            ("itemID", Value::Int(self.item_id)), ("typeID", Value::Int(self.type_id)),
            ("ownerID", Value::Int(self.owner_id)), ("flagID", Value::Int(i64::from(self.flag_id))),
            ("canView", Value::Bool(self.can_view)), ("canTake", Value::Bool(self.can_take)),
        ])))
    }
}

pub(super) async fn owner(ex: &(impl Executor + ?Sized), actor: i64, owner: i64, activity: Option<i32>) -> Result<(), CallError> {
    if actor == owner { return Ok(()); }
    let deny = || user_error(messages::MSG_INDUSTRY_JOB_ACCESS_DENIED, vec![]);
    if !corps::is_player_corp(owner) { return Err(deny()); }
    let authority = corps::permissions::load(ex, actor).await?.ok_or_else(deny)?;
    if !authority.live || authority.corporation != owner { return Err(deny()); }
    let required = ROLE_FACTORY_MANAGER | match activity {
        None => 0,
        Some(MANUFACTURING) => ROLE_FACTORY_MANUFACTURING,
        Some(RESEARCH_TIME | RESEARCH_MATERIAL | COPYING | INVENTION) => ROLE_FACTORY_SCIENCE,
        Some(_) => return Err(deny()),
    };
    if authority.roles[0] & ROLE_DIRECTOR != 0 || authority.roles[0] & required == required {
        Ok(())
    } else {
        Err(deny())
    }
}

pub(super) async fn locations(ex: &(impl Executor + ?Sized), sde: &Sde, actor: i64, owner_id: i64, facility: &Facility) -> Result<Vec<Location>, CallError> {
    owner(ex, actor, owner_id, None).await?;
    if actor == owner_id {
        return Ok(vec![Location { item_id: facility.id, type_id: facility.type_id, owner_id, flag_id: FLAG_HANGAR, can_view: true, can_take: true }]);
    }
    let Some(office_id) = offices::office_at(ex, owner_id, facility.id).await? else { return Ok(vec![]); };
    let Some(office) = load_items(ex, sde, ItemQuery::Id(office_id)).await.map_err(db_err)?.into_iter()
        .find(|item| item.owner_id == owner_id && item.location_id == facility.id) else { return Ok(vec![]); };
    let authority = corps::permissions::load(ex, actor).await?.ok_or_else(|| user_error(messages::MSG_INDUSTRY_JOB_ACCESS_DENIED, vec![]))?;
    if !authority.live || authority.corporation != owner_id { return Err(user_error(messages::MSG_INDUSTRY_JOB_ACCESS_DENIED, vec![])); }
    let inherited = authority.roles[0] & ROLE_DIRECTOR != 0;
    if !inherited && authority.roles[0] & ROLE_FACTORY_MANAGER == 0 {
        return Err(user_error(messages::MSG_INDUSTRY_JOB_ACCESS_DENIED, vec![]));
    }
    Ok((0..CORP_HANGAR_DIVISIONS).map(|division| Location {
        item_id: office_id, type_id: i64::from(office.type_id), owner_id,
        flag_id: FLAG_CORP_SAG1 + division, can_view: true,
        can_take: inherited || corps::permissions::at_station(&authority, facility.id) & (ROLE_HANGAR_CAN_TAKE_1 << division) != 0,
    }).collect())
}

pub(super) fn select_location(request: Option<&Value>, locations: &[Location], input: bool) -> Result<Location, CallError> {
    let deny = || user_error(messages::MSG_INDUSTRY_BLUEPRINT_ACCESS_DENIED, vec![]);
    let location = match request {
        None => locations.iter().filter(|location| location.flag_id == FLAG_HANGAR).next().filter(|_| locations.len() == 1),
        Some(value) => {
            let item = field(value, "itemID").and_then(Value::as_int).ok_or_else(deny)?;
            let flag = field(value, "flagID").and_then(Value::as_int).and_then(|v| i32::try_from(v).ok()).ok_or_else(deny)?;
            locations.iter().find(|location| location.item_id == item && location.flag_id == flag)
        }
    }.ok_or_else(deny)?;
    if input && !location.can_take { return Err(deny()); }
    Ok(location.clone())
}

pub(super) async fn payer(ex: &(impl Executor + ?Sized), actor: i64, account: Option<&Value>) -> Result<(i64, i32), CallError> {
    let Some(account) = account else { return Ok((actor, ACCOUNT_CASH)); };
    let deny = || user_error(messages::MSG_INDUSTRY_JOB_ACCESS_DENIED, vec![]);
    let values = match account { Value::Tuple(values) | Value::List(values) => values.as_slice(), _ => return Err(deny()) };
    let [Value::Int(owner), Value::Int(key)] = values else { return Err(deny()); };
    let key = i32::try_from(*key).map_err(|_| deny())?;
    if !corpwallet::valid_key(key) { return Err(deny()); }
    if *owner == actor { return if key == ACCOUNT_CASH { Ok((actor, key)) } else { Err(deny()) }; }
    if !corps::is_player_corp(*owner) || corps::membership(ex, actor).await?.0 != *owner || !corpwallet::may_take(ex, actor, *owner, key).await? {
        return Err(deny());
    }
    Ok((*owner, key))
}

#[cfg(test)]
mod tests;
