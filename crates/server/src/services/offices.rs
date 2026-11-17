//! Corporation offices (officeManager, bound per station): a player corporation rents an
//! office at a station from its master wallet for appConst.rentalPeriodOffice days at a
//! time. The office is an item (typeOffice) owned by the corporation at the station, and
//! the corporation hangar divisions are its contents under flagCorpSAG1-7. A lease is
//! Active only while `rented_until > now`; an unpaid deadline materializes Impounded
//! (contents retained) or Released (empty) through the billing scan, and the same
//! guarded transition answers unrent, release, trash and re-rent. Renting an active
//! office refuses: renewal is its rental bill (services/bills). Re-renting an
//! impounded or released office restores its contents with no second fee. Office
//! capacity and security-based rent are operator policies; station existence checks are shared.

use super::corps::is_player_corp;
use super::{db_err, has_role_or_is_ceo, keyval, notify_error, Call, CallError, CallResult};
use infinity_core::consts::{ROLE_DIRECTOR, TYPE_OFFICE};
use crate::client::{filetime_now, Client};
use crate::dao::{items::loading::ItemQuery, offices};
use crate::db::executor::Executor;
use crate::db::Db;
use infinity_core::messages;
use crate::sde::Sde;
use marshal::Value;

pub(super) mod impound;
pub(super) mod lifecycle;
pub(super) mod notifications;

/// Server office item policy: no inventory flag on the station's office item.
pub(super) const OFFICE_ITEM_FLAG: i32 = 0;
/// appConst.corpRoleCanRentOffice
const ROLE_CAN_RENT_OFFICE: i64 = 562_949_953_421_312;
/// Server office rent policy: security used when the station's system is unavailable.
const DEFAULT_SECURITY: f64 = 0.5;

/// The rent for one period at `station`: dearer in busier, higher-security space.
pub(super) fn rent_price(sde: &Sde, station: i64) -> f64 {
    let security = sde.map.station(station)
        .and_then(|st| st.solar_system_id)
        .and_then(|id| sde.map.system(id))
        .map(|system| system.security.unwrap_or(0.0))
        .unwrap_or(DEFAULT_SECURITY);
    let policy = &crate::config::game().corporations;
    (policy.office_rent_base_isk + policy.office_rent_per_security_isk * security.clamp(0.0, 1.0)).round()
}

/// Require an SDE station before an office operation.
fn require_station(sde: &Sde, station: i64) -> Result<(), CallError> {
    sde.map.station(station).map(|_| ())
        .ok_or_else(|| notify_error(messages::offices::OFFICE_STATION_UNSUPPORTED))
}

/// The corporation's office at `station`, only while its lease is active.
pub(super) async fn office_at(db: &(impl Executor + ?Sized), corp: i64, station: i64) -> Result<Option<i64>, CallError> {
    offices::active_at(db, corp, station, filetime_now()).await.map_err(db_err)
        .map(|lease| lease.map(|lease| lease.office_id))
}

/// Whether `item` is an office, and whose; identity is state-independent.
pub(super) async fn office_owner(db: &(impl Executor + ?Sized), item: i64) -> Result<Option<i64>, CallError> {
    offices::owner(db, item).await.map_err(db_err)
}

/// A director, the CEO, or a member allowed to rent offices.
pub(super) async fn may_rent(db: &(impl Executor + ?Sized), me: i64, corp: i64) -> Result<bool, CallError> {
    has_role_or_is_ceo(db, me, corp, ROLE_DIRECTOR | ROLE_CAN_RENT_OFFICE).await
}

/// The office rows the client keeps (officeID, stationID, solarsystemID, stationTypeID).
async fn my_offices(db: &Db, sde: &Sde, corp: i64) -> CallResult {
    let rows = offices::active_list(db, corp, filetime_now()).await.map_err(db_err)?;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    Ok(Value::list(rows.iter().filter_map(|r| {
        let station = sde.map.station(r.station_id)?;
        Some(keyval([
            ("officeID", Value::Int(r.office_id)),
            ("stationID", Value::Int(r.station_id)),
            ("solarsystemID", opt(station.solar_system_id)),
            ("stationTypeID", opt(station.type_id)),
            ("typeID", Value::Int(TYPE_OFFICE as i64)),
            ("rentedUntil", Value::Int(r.rented_until)),
        ]))
    })))
}

/// Prime the bound office inventory: read the real office item and let the
/// caller's cache update with an empty change dict. No item is created.
async fn prime_office_item(c: &mut Client, corp: i64, station: i64) -> CallResult {
    let now = filetime_now();
    let Some(lease) = offices::active_at(&c.db, corp, station, now).await.map_err(db_err)? else {
        return Ok(Value::None);
    };
    if let Some(item) = super::items::load_items(&c.db, &c.sde, ItemQuery::Id(lease.office_id)).await.map_err(db_err)?.into_iter().next() {
        let _ = c.notify_tx.send(("OnItemChange".into(), vec![item.row(), Value::Dict(vec![]), Value::None]));
    }
    Ok(Value::None)
}

/// Read the quote argument: a finite, nonnegative client number converted once
/// through the exact IskAmount boundary.
fn quote_arg(call: &Call<'_>, index: usize) -> Result<crate::isk::IskAmount, CallError> {
    let value = call.args.get(index).ok_or_else(|| notify_error(messages::OFFICE_RENT_CHANGED))?;
    match value {
        Value::Int(amount) if *amount >= 0 => crate::isk::IskAmount::from_whole_isk(i128::from(*amount))
            .ok_or_else(|| notify_error(messages::OFFICE_RENT_CHANGED)),
        Value::Float(amount) if amount.is_finite() && *amount >= 0.0 => crate::isk::IskAmount::from_isk(*amount)
            .map_err(|_| notify_error(messages::OFFICE_RENT_CHANGED)),
        _ => Err(notify_error(messages::OFFICE_RENT_CHANGED)),
    }
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "officeManager" {
        return None;
    }
    let corp = c.session.int("corpid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let private = !matches!(call.method, "GetCorporationsWithOffices" | "GetEmptyOfficeCount");
    if private {
        match super::corps::permissions::load(db, me).await {
            Ok(Some(authority)) if authority.corporation == corp && (authority.live || !is_player_corp(corp)) => {}
            Ok(_) => return Some(Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION))),
            Err(error) => return Some(Err(error)),
        }
    }
    if matches!(call.method, "HasCorpImpoundedItems" | "GetImpoundReleasePrice") {
        if let Err(error) = super::bills::authorize_read(db, me, corp).await {
            return Some(Err(error));
        }
    }
    if call.method == "GetMyCorporationsOffices" {
        return Some(my_offices(db, world.sde(), corp).await);
    }
    // Bound to a station.
    let station = call
        .bind_params
        .and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int())))
        .or_else(|| c.session.int("stationid"))?;
    if let Err(error) = require_station(world.sde(), station) {
        return Some(Err(error));
    }
    let identity = super::identity::Identity::of_client(c);
    let sender = c.notify_tx.clone();
    let sde = c.sde.clone();
    Some(match call.method {
        // [corporationID] with an office here.
        "GetCorporationsWithOffices" => match offices::active_corporations(db, station, filetime_now()).await {
            Ok(rows) => Ok(Value::list(rows.iter().copied().map(Value::Int))),
            Err(e) => Err(db_err(e)),
        },
        "GetEmptyOfficeCount" => match offices::active_count(db, station, filetime_now()).await {
            Ok(count) => Ok(Value::Int((crate::config::game().corporations.office_slots_per_station - count).max(0))),
            Err(e) => Err(db_err(e)),
        },
        // GetPriceQuote(corpID) -> ISK for one rental period
        "GetPriceQuote" => {
            if call.args.first().and_then(|v| v.as_int()) != Some(corp) {
                return Some(Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION)));
            }
            Ok(Value::Float(rent_price(world.sde(), station)))
        }
        // RentOffice(cost): the quote the player agreed to.
        "RentOffice" => {
            if !is_player_corp(corp) {
                return Some(Err(notify_error(messages::NPC_CORPORATION_CANNOT_RENT_OFFICE)));
            }
            if let Err(error) = require_station(world.sde(), station) {
                return Some(Err(error));
            }
            match quote_arg(call, 0) {
                Err(e) => Err(e),
                Ok(quote) => lifecycle::rent(&identity, db, world, &sde, station, quote).await.map(|_| Value::None),
            }
        }
        // Give the office up; remote stations bind their own officeManager moniker.
        "UnrentOffice" => lifecycle::unrent(&identity, db, world, &sde, station).await.map(|_| Value::None),
        "PrimeOfficeItem" => prime_office_item(c, corp, station).await,
        "HasCorpImpoundedItems" => match impound::has_impounded(db, corp, station).await {
            Ok(found) => Ok(Value::Bool(found)),
            Err(e) => Err(e),
        },
        "GetImpoundReleasePrice" => match impound::has_impounded(db, corp, station).await {
            Err(e) => Err(e),
            Ok(false) => Ok(Value::None),
            Ok(true) => impound::release_price(world.sde(), station).map(|price| Value::Float(price.wire_isk())),
        },
        // GetItemsFromImpound(quotedPrice): release the impounded contents to Deliveries.
        "GetItemsFromImpound" => match quote_arg(call, 0) {
            Err(e) => Err(e),
            Ok(quote) => impound::release(&identity, db, world, &sde, sender, station, quote).await.map(|_| Value::None),
        },
        "TrashImpoundedOffice" => impound::trash_impounded(&identity, db, world, &sde, sender, station).await.map(|_| Value::None),
        _ => return None,
    })
}

#[cfg(all(test, feature = "tests-social"))]
mod tests;
