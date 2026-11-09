//! Ship insurance (insuranceSvc, bound per station; eve/client/script/ui/station/insurance,
//! eve/common/script/util/insuranceConst.py).
//!
//! A ship's insured value is its retail adjusted or average price (else its base price). A contract
//! (game.insurance) pays out `fraction` of that value when the ship is destroyed and costs
//! (fraction - 0.4) / 2 of it up front, as insuranceConst prices its packages from Basic
//! (0.5) to Platinum (1.0); it runs for 12 weeks. Destroyed ships pay their owner and post an
//! insurance payout notification.
//! Payout and policy issuance each commit their wallet, policy and notification writes atomically.

use super::{db_err, keyval, notify_error, value_id_list, Call, CallError, CallResult};
use crate::dao::insurance;
use crate::client::{filetime_now, Client};
use infinity_core::consts::{CATEGORY_SHIP, FILETIME_DAY};
use crate::db::{executor::Executor, outbox::{dispatch_world, Outbox}};
use infinity_core::messages;
use marshal::Value;
use tracing::info;

/// Server insurance policy: twelve-week contract duration.
const CONTRACT_DAYS: i64 = 84;
/// Client insuranceConst package price base fraction.
const BASE_FRACTION: f64 = 0.4;
/// Client insuranceConst package price multiplier.
const FRACTION_INCREMENT: f64 = 0.5;
/// Client insuranceConst insurance package payout fractions.
const FRACTIONS: [f64; 6] = [0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
/// Client notificationconst insurance payout type.
const NOTIFICATION_PAYOUT: i32 = 35;
/// Client notificationconst insurance issued type.
const NOTIFICATION_ISSUED: i32 = 55;
/// Client notificationconst insurance group.
const NOTIFICATION_GROUP_INSURANCE: i32 = 11;
/// SDE NPC corporations: Secure Commerce Commission signs insurance.
const INSURER: i64 = 1000132;
/// Server insurance quote policy: permitted absolute difference in ISK.
const QUOTE_TOLERANCE_ISK: f64 = 1.0;

/// What a ship type is insured for (the payout at fraction 1.0); 0 if it can't be insured.
async fn value(db: &(impl Executor + ?Sized), sde: &crate::sde::Sde, type_id: i32) -> Result<f64, CallError> {
    let Some(ty) = sde.types.get(type_id).filter(|t| t.category_id == Some(CATEGORY_SHIP)) else { return Ok(0.0); };
    let price = insurance::value(db, type_id).await.map_err(db_err)?;
    Ok(price_value(price.as_ref().and_then(|p| p.adjusted), price.as_ref().and_then(|p| p.average), ty.base_price))
}

fn price_value(adjusted: Option<f64>, average: Option<f64>, base: Option<f64>) -> f64 {
    adjusted.filter(|v| *v != 0.0).or_else(|| average.filter(|v| *v != 0.0)).or(base).unwrap_or(0.0)
}

fn contract_row(r: &insurance::Contract) -> Value {
    keyval([
        ("shipID", Value::Int(r.ship_id)),
        ("ownerID", Value::Int(r.owner_id)),
        ("fraction", Value::Float(r.fraction)),
        ("startDate", Value::Int(r.start_date)),
        ("endDate", Value::Int(r.end_date)),
    ])
}

/// `InsureShip(itemID, amount, isCorpItem, voidOld=False)`.
async fn insure(c: &Client, call: &Call<'_>) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let amount = call.args.get(1).and_then(|v| v.as_float()).unwrap_or(0.0);
    let void_old = call.kwargs.get("voidOld").or(call.args.get(3)).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int() == Some(1));
    let Some(item) = crate::services::items::load_items(db, &c.sde, crate::dao::items::loading::ItemQuery::Id(ship)).await.map_err(db_err)?.pop() else {
        return Err(CallError::User { msg: messages::MSG_INS_COULD_NOT_FIND_ITEM.into(), dict: vec![] });
    };
    if item.owner_id != me || !item.singleton || item.category_id != CATEGORY_SHIP as i64 {
        return Err(notify_error(messages::INSURANCE_REQUIRES_OWN_ASSEMBLED_SHIP));
    }
    let full = value(db, &c.sde, item.type_id).await?;
    let Some(fraction) = FRACTIONS.iter().copied().find(|f| (full * (f - BASE_FRACTION) * FRACTION_INCREMENT - amount).abs() < QUOTE_TOLERANCE_ISK) else {
        return Err(notify_error(messages::INSURANCE_QUOTE_EXPIRED));
    };
    let existing = insurance::contract(db, ship).await.map_err(db_err)?;
    if existing.is_some() && !void_old {
        return Err(CallError::User { msg: messages::MSG_INSURE_SHIP_FAILED_SINGLE_CONTRACT.into(), dict: vec![("ownerID".into(), Value::Int(me))] });
    }
    let now = filetime_now();
    let type_id = item.type_id;
    db.transaction(
        |tx, outbox: &mut Outbox| Box::pin(async move {
            super::wallets::debit(tx, outbox, me, crate::isk::IskAmount::from_isk(amount)?).await?;
            insurance::insure(tx, ship, me, type_id, fraction, now, now + CONTRACT_DAYS * FILETIME_DAY).await.map_err(db_err)?;
            super::mail::notify(tx, outbox, me, NOTIFICATION_ISSUED, NOTIFICATION_GROUP_INSURANCE, INSURER, &format!("{{itemID: {ship}, typeID: {type_id}}}")).await?;
            outbox.notify_pilot(me, "OnShipInsured", vec![]);
            Ok(())
        }),
        |(), outbox| async move { dispatch_world((), outbox, world) },
    ).await?;
    info!("insurance: {me} insured ship {ship} at {fraction} for {amount:.0} ISK");
    Ok(Value::None)
}

/// Consume an insurance contract and queue its payout in the ship loss transaction.
pub(crate) async fn ship_destroyed_in<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, sde: &crate::sde::Sde, ship: i64, now: i64,
) -> Result<crate::isk::IskAmount, CallError> {
    let Some(r) = insurance::take_payout(ex, ship).await.map_err(db_err)? else { return Ok(crate::isk::IskAmount::ZERO) };
    let (owner, type_id, fraction, end) = (r.owner_id, r.type_id, r.fraction, r.end_date);
    if end < now { return Ok(crate::isk::IskAmount::ZERO); }
    let payout = super::market::round2(value(ex, sde, type_id).await? * fraction);
    let posted = crate::isk::IskAmount::from_isk(payout)?;
    super::wallets::credit(ex, outbox, owner, posted).await?;
    let data = format!("{{amount: {payout}, itemID: {ship}, typeID: {type_id}, payout: 0}}");
    super::mail::notify(ex, outbox, owner, NOTIFICATION_PAYOUT, NOTIFICATION_GROUP_INSURANCE, INSURER, &data).await?;
    Ok(posted)
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "insuranceSvc" {
        return None;
    }
    let me = c.session.int("charid").unwrap_or(0);
    let db = &c.db;
    Some(match call.method {
        "GetInsurancePrice" => value(db, &c.sde, call.args.first().and_then(|v| v.as_int()).unwrap_or(0) as i32).await.map(Value::Float),
        "GetInsurancePrices" => {
            let types = value_id_list(call.args.first());
            let mut out = vec![];
            for t in types {
                match value(db, &c.sde, t as i32).await {
                    Ok(v) => out.push((Value::Int(t), Value::Float(v))),
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::Dict(out))
        }
        // GetContracts(includeCorp=0): the character's (corporations insure nothing yet).
        "GetContracts" => {
            if call.args.first().and_then(|v| v.as_int()).unwrap_or(0) != 0 {
                return Some(Ok(Value::list([])));
            }
            match insurance::contracts(db, me, filetime_now()).await {
                Ok(rows) => Ok(Value::list(rows.iter().map(contract_row))),
                Err(e) => Err(db_err(e)),
            }
        }
        "GetContractForShip" => {
            let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            match insurance::active_contract(db, ship, filetime_now()).await {
                Ok(r) => Ok(r.map(|r| contract_row(&r)).unwrap_or(Value::None)),
                Err(e) => Err(db_err(e)),
            }
        }
        "InsureShip" => insure(c, call).await,
        "UnInsureShip" => {
            let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            match insurance::uninsure(db, ship, me).await {
                Ok(_) => {
                    c.world.notify_pilot(me, "OnShipInsured", vec![]);
                    Ok(Value::None)
                }
                Err(e) => Err(db_err(e)),
            }
        }
        _ => return None,
    })
}

#[cfg(test)] mod tests;
