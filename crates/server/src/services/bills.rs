//! Bills (billMgr; bound allianceRegistry's GetBills/PayBill): what corporations, alliances
//! and characters owe, in game.bills, using startup SDE station data for office landlords.
//!
//! Office rent is the one bill issued: before an office's rental period ends its corporation
//! is billed the station's rent, due when the period ends; paying extends the office by
//! another period. A corporation can have its accountants' chosen bill types paid automatically
//! from a wallet division (billMgr automatic payment settings). Advance billing and maintenance
//! cadence are operator policies. An office whose rent goes unpaid isn't taken away.
//! Billing scans run as durable periodic jobs registered through the services scheduler
//! and persisted in game.jobs, not detached maintenance sleeps. Internal scan errors warn
//! but the scan still completes.

use super::{db_err, has_role_or_is_ceo, keyval, notify_error, Call, CallError, CallResult};
use crate::dao::characters::members;
use crate::dao::bills as bills_dao;
use crate::client::{filetime_now, Client};
use infinity_core::consts::{ACCOUNT_CASH, FILETIME_DAY, RENTAL_DAYS, ROLE_ACCOUNTANT, ROLE_DIRECTOR};
use crate::db::{Db, executor::Executor};
use crate::db::outbox::dispatch_world;
use infinity_core::messages;
use crate::space::World;
use marshal::Value;
use std::future::Future;
use std::sync::Arc;
use tracing::{info, warn};

// appConst.billType* and bill status
const BILL_RENTAL: i32 = 2;
const UNPAID: i32 = 0;
const PAID: i32 = 1;

fn bill_row(r: &bills_dao::Bill) -> Value {
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    keyval([
        ("billID", Value::Int(r.bill_id)),
        ("billTypeID", Value::Int(r.bill_type as i64)),
        ("debtorID", Value::Int(r.debtor_id)),
        ("creditorID", Value::Int(r.creditor_id)),
        ("amount", Value::Float(r.amount)),
        ("dueDateTime", Value::Int(r.due)),
        ("interest", Value::Float(r.interest)),
        ("externalID", opt(r.external_id)),
        ("externalID2", opt(r.external_id2)),
        ("paid", Value::Bool(false)),
    ])
}

/// Unpaid bills `owner` owes (`creditor` false) or is owed.
pub(super) async fn bills(db: &Db, owner: i64, creditor: bool) -> CallResult {
    let rows = bills_dao::list(db, owner, creditor, UNPAID).await.map_err(db_err)?;
    Ok(Value::list(rows.iter().map(bill_row)))
}

/// Tell `corp`'s online members their bills changed.
async fn announce(db: &Db, world: &World, corp: i64) -> Result<(), CallError> {
    for id in members::active_ids(db, corp).await.map_err(db_err)? {
        world.notify_pilot(id, "OnBillReceived", vec![]);
    }
    Ok(())
}

/// Pay a bill from `corp`'s division `key` (or, with `corp` None, the debtor character's
/// wallet), credit the creditor, and carry out what it paid for.
pub(super) async fn pay(db: &Db, world: &World, bill: i64, expected_debtor: i64, corp: Option<i64>, key: i32, actor: Option<i64>) -> Result<(), CallError> {
    let _trading = super::market::lock().await;
    db.transaction(
        |tx, outbox| Box::pin(async move {
            let Some(r) = bills_dao::get(tx, bill, UNPAID).await.map_err(db_err)? else {
                return Err(notify_error(messages::BILL_ALREADY_PAID_OR_MISSING));
            };
            let (kind, debtor, creditor, amount, external) = (r.bill_type, r.debtor_id, r.creditor_id, r.amount, r.external_id);
            if debtor != expected_debtor { return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED)); }
            if let Some(actor) = actor {
                let alliance = corp.filter(|corp| *corp != debtor).map(|_| debtor);
                let alliances: Vec<i64> = alliance.into_iter().collect();
                let corps: Vec<i64> = corp.into_iter().collect();
                if !crate::dao::corporations::registry::lock_admission(tx, &alliances, &corps, &[actor]).await.map_err(db_err)? {
                    return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
                }
                match corp {
                    Some(corp) => {
                        let authority = super::corps::permissions::load(tx, actor).await?;
                        if (alliance.is_some() && crate::dao::alliances::executor_corp(tx, debtor).await.map_err(db_err)? != Some(corp))
                            || !authority.is_some_and(|authority| super::corps::permissions::allows(&authority, corp, ROLE_ACCOUNTANT)
                                && super::corpwallet::can_take(&authority, corp, key)) {
                            return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
                        }
                    }
                    None if debtor != actor => return Err(notify_error(messages::BILL_NOT_OWNED_BY_CHARACTER)),
                    None => {}
                }
            } else if corp.is_some_and(|corp| corp != debtor) {
                return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
            }
            let amount = crate::isk::IskAmount::from_isk(amount)?;
            match corp {
                Some(corp) => super::wallets::corp_debit(tx, outbox, corp, key, amount).await?,
                None => super::wallets::debit(tx, outbox, debtor, amount).await?,
            }
            if super::corps::is_player_corp(creditor) {
                super::wallets::corp_credit(tx, outbox, creditor, ACCOUNT_CASH, amount).await?;
            }
            bills_dao::set_paid(tx, bill, PAID).await.map_err(db_err)?;
            if kind == BILL_RENTAL {
                if let Some(office) = external {
                    bills_dao::extend_office(tx, office, RENTAL_DAYS * FILETIME_DAY).await.map_err(db_err)?;
                }
            }
            if let Some(corp) = corp {
                for id in members::active_ids(tx, corp).await.map_err(db_err)? {
                    outbox.notify_pilot(id, "OnBillReceived", vec![]);
                }
            }
            Ok((debtor, creditor, amount))
        }),
        |(debtor, creditor, amount), outbox| async move {
            dispatch_world((), outbox, world)?;
            info!("bills: {debtor} paid bill {bill} ({:.0} ISK to {creditor})", amount.wire_isk());
            Ok(())
        },
    ).await
}

/// Issue rent bills that have come due and pay those set to be paid automatically.
async fn run(db: &Db, world: &World) -> Result<(), CallError> {
    static RUN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _run = RUN_LOCK.lock().await;
    let now = filetime_now();
    let ahead = crate::config::game().corporations.advance_billing_days.saturating_mul(FILETIME_DAY);
    let due = bills_dao::due(db, now, ahead, BILL_RENTAL, UNPAID).await.map_err(db_err)?;
    for r in due {
        let (office, corp, station, until) = (r.office_id, r.corporation_id, r.station_id, r.rented_until);
        let landlord = world.sde().map.station(station).and_then(|st| st.owner_id).unwrap_or(1_000_001);
        let amount = super::offices::rent_price(world.sde(), station);
        {
            let _trading = super::market::lock().await;
            bills_dao::issue(db, BILL_RENTAL, corp, landlord, amount, until, office, station, now).await.map_err(db_err)?;
        }
        info!("bills: billed {corp} {amount:.0} ISK rent for office {office}");
        announce(db, world, corp).await?;
    }
    let auto = bills_dao::automatic(db, UNPAID).await.map_err(db_err)?;
    for r in auto {
        let (bill, corp, key) = (r.bill_id, r.debtor_id, r.bill_division);
        if let Err(e) = pay(db, world, bill, corp, Some(corp), key, None).await {
            if let CallError::Internal(e) = e {
                warn!("bills: paying {bill} automatically: {e:#}");
            }
        }
    }
    Ok(())
}

/// Execute one scheduler-driven billing scan.
pub(crate) fn run_billing(db: Db, world: Arc<World>, _: crate::scheduler::Job) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        if let Err(CallError::Internal(e)) = run(&db, &world).await { warn!("billing failed: {e:#}"); }
        Ok(())
    })
}

/// An accountant, director or the CEO of `corp`.
pub(super) async fn is_accountant(db: &(impl Executor + ?Sized), me: i64, corp: i64) -> Result<bool, CallError> {
    has_role_or_is_ceo(db, me, corp, ROLE_ACCOUNTANT | ROLE_DIRECTOR).await
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "billMgr" {
        return None;
    }
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    Some(match call.method {
        "GetCorporationBills" => bills(db, corp, false).await,
        "GetCorporationBillsReceivable" => bills(db, corp, true).await,
        // PayCorporationBill(billID, fromAccountKey=...)
        "PayCorporationBill" => {
            let key = call.kwargs.get("fromAccountKey").and_then(|v| v.as_int()).or(int(1)).unwrap_or(ACCOUNT_CASH as i64) as i32;
            pay(db, world, int(0).unwrap_or(0), corp, Some(corp), key, Some(me)).await.map(|_| Value::None)
        }
        "CharPayBill" => {
            let bill = int(0).unwrap_or(0);
            pay(db, world, bill, me, None, 0, Some(me)).await.map(|_| Value::None)
        }
        // {ownerID: {billTypeID: bool, 'divisionID': accountKey}}
        "GetAutomaticPaySettings" => {
            let rows = match bills_dao::settings(db, corp).await {
                Ok(rows) => rows,
                Err(e) => return Some(Err(db_err(e))),
            };
            let division: i32 = match bills_dao::division(db, corp).await {
                Ok(r) => r.unwrap_or(ACCOUNT_CASH),
                Err(e) => return Some(Err(db_err(e))),
            };
            let mut settings: Vec<(Value, Value)> = rows.iter().map(|r| (Value::Int(r.bill_type as i64), Value::Bool(r.enabled))).collect();
            settings.push((Value::str("divisionID"), Value::Int(division as i64)));
            Ok(Value::Dict(vec![(Value::Int(corp), Value::Dict(settings))]))
        }
        // SendAutomaticPaySettings({ownerID: {billTypeID: bool, 'divisionID': accountKey}})
        "SendAutomaticPaySettings" => {
            let mut changes = Vec::new();
            if let Some(Value::Dict(owners)) = call.args.first() {
                for (owner, settings) in owners {
                    if owner.as_int() != Some(corp) { continue; }
                    let Value::Dict(settings) = settings else { continue };
                    for (key, value) in settings {
                        if let Some(kind) = key.as_int() {
                            let enabled = matches!(value, Value::Bool(true)) || value.as_int().is_some_and(|v| v != 0);
                            changes.push((Some(kind as i32), i32::from(enabled)));
                        } else if key.as_text().as_deref() == Some("divisionID") {
                            changes.push((None, value.as_int().unwrap_or(ACCOUNT_CASH as i64) as i32));
                        }
                    }
                }
            }
            db.transaction(
                |tx, _: &mut ()| {
                    let changes = changes.clone();
                    Box::pin(async move {
                        if !crate::dao::corporations::registry::lock_admission(tx, &[], &[corp], &[me]).await.map_err(db_err)?
                            || !is_accountant(tx, me, corp).await? { return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS)); }
                        for (kind, value) in changes {
                            match kind {
                                Some(kind) => { bills_dao::set_setting(tx, corp, kind, value != 0).await.map_err(db_err)?; }
                                None => { bills_dao::set_division(tx, corp, value).await.map_err(db_err)?; }
                            }
                        }
                        Ok(Value::None)
                    })
                },
                |value, ()| async move { Ok(value) },
            ).await
        }
        _ => return None,
    })
}
