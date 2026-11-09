//! Corporation offices (officeManager, bound per station): a player corporation rents an
//! office at a station from its master wallet for appConst.rentalPeriodOffice days at a
//! time. The office is an item (typeOffice) owned by the corporation at the station, and
//! the corporation hangar divisions are its contents under flagCorpSAG1-7 (services/
//! inventory lists and moves them). Renting again before the period ends extends it;
//! renewing is billed (services/bills). Station and system data come from the startup SDE;
//! office records and counts come from game.offices. Impounding is not supported.
//! Office capacity and security-based rent are operator policies.

use super::corps::is_player_corp;
use super::{db_err, has_role_or_is_ceo, keyval, notify_error, Call, CallError, CallResult};
use crate::dao::characters::members;
use crate::dao::{offices, items::{creation, inventory}};
use crate::client::{filetime_now, Client};
use infinity_core::consts::{ACCOUNT_CASH, FILETIME_DAY, RENTAL_DAYS, ROLE_DIRECTOR};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::dispatch_world;
use infinity_core::messages;
use crate::sde::Sde;
use crate::space::World;
use marshal::Value;
use tracing::info;

/// SDE invTypes: Office.
const TYPE_OFFICE: i32 = 27;
/// Server office item policy: no inventory flag on the station's office item.
const OFFICE_ITEM_FLAG: i32 = 0;
/// appConst.corpRoleCanRentOffice
const ROLE_CAN_RENT_OFFICE: i64 = 562_949_953_421_312;
/// Server office rent policy: security used when the station's system is unavailable.
const DEFAULT_SECURITY: f64 = 0.5;
/// Server office rent policy: half-ISK tolerance for rounded quotes.
const QUOTE_TOLERANCE: f64 = 0.5;

/// The rent for one period at `station`: dearer in busier, higher-security space.
pub(super) fn rent_price(sde: &Sde, station: i64) -> f64 {
    let security = sde.map.station(i64::from(station as i32))
        .and_then(|st| st.solar_system_id)
        .and_then(|id| sde.map.system(id))
        .map(|system| system.security.unwrap_or(0.0))
        .unwrap_or(DEFAULT_SECURITY);
    let policy = &crate::config::game().corporations;
    (policy.office_rent_base_isk + policy.office_rent_per_security_isk * security.clamp(0.0, 1.0)).round()
}

/// The corporation's office at `station`, if it rents one.
pub(super) async fn office_at(db: &(impl Executor + ?Sized), corp: i64, station: i64) -> Result<Option<i64>, CallError> {
    offices::at(db, corp, station).await.map_err(db_err)
}

/// Whether `item` is an office, and whose.
pub(super) async fn office_owner(db: &(impl Executor + ?Sized), item: i64) -> Result<Option<i64>, CallError> {
    offices::owner(db, item).await.map_err(db_err)
}


/// Rent (or extend) `corp`'s office at `station` for a period, paid from its master wallet.
pub(super) async fn rent(db: &Db, world: &World, me: i64, corp: i64, station: i64) -> Result<i64, CallError> {
    let price = rent_price(world.sde(), station);
    let _trading = super::market::lock().await;
    let now = filetime_now();
    db.transaction(
        |tx, outbox| Box::pin(async move {
            if !crate::dao::corporations::registry::lock_admission(tx, &[], &[corp], &[me]).await.map_err(db_err)?
                || !may_rent(tx, me, corp).await? { return Err(notify_error(messages::NO_OFFICE_RENTAL_ROLE)); }
            super::wallets::corp_debit(tx, outbox, corp, ACCOUNT_CASH, crate::isk::IskAmount::from_isk(price)?).await?;
            if let Some(office) = office_at(tx, corp, station).await? {
                offices::extend(tx, office, now, RENTAL_DAYS * FILETIME_DAY).await.map_err(db_err)?;
                return Ok((office, false));
            }
            let office = creation::rookie_ship(tx, TYPE_OFFICE, corp, station, OFFICE_ITEM_FLAG).await.map_err(db_err)?;
            offices::insert(tx, office, corp, station, now + RENTAL_DAYS * FILETIME_DAY).await.map_err(db_err)?;
            for id in members::active_ids(tx, corp).await.map_err(db_err)? {
                outbox.notify_pilot(id, "OnOfficeRentalChange", vec![Value::Int(corp), Value::Int(office)]);
            }
            Ok((office, true))
        }),
        |(office, created), outbox| async move {
            dispatch_world((), outbox, world)?;
            if created {
                info!("offices: {corp} rented office {office} at {station} for {price:.0} ISK");
            }
            Ok(office)
        },
    ).await
}

/// A director, the CEO, or a member allowed to rent offices.
async fn may_rent(db: &(impl Executor + ?Sized), me: i64, corp: i64) -> Result<bool, CallError> {
    has_role_or_is_ceo(db, me, corp, ROLE_DIRECTOR | ROLE_CAN_RENT_OFFICE).await
}

/// The office rows the client keeps (officeID, stationID, solarsystemID, stationTypeID).
async fn my_offices(db: &Db, sde: &Sde, corp: i64) -> CallResult {
    let rows = offices::list(db, corp).await.map_err(db_err)?;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    Ok(Value::list(rows.iter().map(|r| {
        let station = sde.map.station(r.station_id);
        keyval([
            ("officeID", Value::Int(r.office_id)),
            ("stationID", Value::Int(r.station_id)),
            ("solarsystemID", opt(station.and_then(|st| st.solar_system_id))),
            ("stationTypeID", opt(station.and_then(|st| st.type_id))),
            ("typeID", Value::Int(TYPE_OFFICE as i64)),
            ("rentedUntil", Value::Int(r.rented_until)),
        ])
    })))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "officeManager" {
        return None;
    }
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    if call.method == "GetMyCorporationsOffices" {
        return Some(my_offices(db, world.sde(), corp).await);
    }
    // Bound to a station.
    let station = call
        .bind_params
        .and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int())))
        .or_else(|| c.session.int("stationid"))?;
    Some(match call.method {
        // [corporationID] with an office here.
        "GetCorporationsWithOffices" => match offices::corporations(db, station).await {
            Ok(rows) => Ok(Value::list(rows.iter().copied().map(Value::Int))),
            Err(e) => Err(db_err(e)),
        },
        "GetEmptyOfficeCount" => match offices::count(db, station).await {
            Ok(count) => Ok(Value::Int((crate::config::game().corporations.office_slots_per_station - count).max(0))),
            Err(e) => Err(db_err(e)),
        },
        // GetPriceQuote(corpID) -> ISK for one rental period
        "GetPriceQuote" => Ok(Value::Float(rent_price(world.sde(), station))),
        // RentOffice(cost): the quote the player agreed to.
        "RentOffice" => {
            if !is_player_corp(corp) {
                return Some(Err(notify_error(messages::NPC_CORPORATION_CANNOT_RENT_OFFICE)));
            }
            let quoted = call.args.first().and_then(|v| v.as_float().or_else(|| v.as_int().map(|i| i as f64)));
            let price = rent_price(world.sde(), station);
            if quoted.is_some_and(|q| q + QUOTE_TOLERANCE < price) {
                return Some(Err(notify_error(messages::OFFICE_RENT_CHANGED)));
            }
            rent(db, world, me, corp, station).await.map(|_| Value::None)
        }
        // Give the office up; its hangars must be empty first.
        "UnrentOffice" => {
            let _trading = super::market::lock().await;
            db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| Box::pin(async move {
                    if !crate::dao::corporations::registry::lock_admission(tx, &[], &[corp], &[me]).await.map_err(db_err)?
                        || !may_rent(tx, me, corp).await? { return Err(notify_error(messages::NO_OFFICE_RELEASE_ROLE)); }
                    let Some(office) = office_at(tx, corp, station).await? else { return Ok(Value::None) };
                    if offices::has_items(tx, office).await.map_err(db_err)? { return Err(notify_error(messages::EMPTY_CORPORATION_HANGAR_BEFORE_RELEASE)); }
                    offices::delete(tx, office).await.map_err(db_err)?;
                    inventory::delete(tx, office).await.map_err(db_err)?;
                    for id in members::active_ids(tx, corp).await.map_err(db_err)? {
                        outbox.notify_pilot(id, "OnOfficeRentalChange", vec![Value::Int(corp), Value::Int(office)]);
                    }
                    Ok(Value::None)
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await
        }
        // Nothing is impounded: an office is only given up empty.
        "PrimeOfficeItem" => Ok(Value::None),
        "HasCorpImpoundedItems" => Ok(Value::Bool(false)),
        "GetImpoundReleasePrice" => Ok(Value::None),
        _ => return None,
    })
}
