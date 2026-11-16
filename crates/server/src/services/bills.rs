//! Bills (billMgr; bound allianceRegistry's GetBills/PayBill): what corporations, alliances
//! and characters owe, in game.bills, using startup SDE station data for office landlords.
//!
//! Office rent is the one bill issued: before an office's rental period ends its corporation
//! is billed the station's rent, due at the boundary U and covering [U, U+period); paying
//! while the term is still live extends exactly from U. A corporation can have its
//! accountants' chosen bill types paid automatically from a wallet division (billMgr
//! automatic payment settings); an alliance's settings live on the alliance row and are
//! settled by its current executor corporation. Advance billing and maintenance cadence
//! are operator policies. Billing scans run as durable periodic jobs registered through
//! the services scheduler; internal errors propagate to the scheduler retry.

use super::{db_err, has_role_or_is_ceo, keyval, notify_error, Call, CallError, CallResult};
use crate::dao::bills as bills_dao;
use crate::client::Client;
use infinity_core::consts::{ACCOUNT_CASH, ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_JUNIOR_ACCOUNTANT, TYPE_OFFICE_RENTAL};
use crate::db::{Db, executor::Executor};
use infinity_core::messages;
use marshal::Value;

pub(super) mod maintenance;
pub(super) mod settlement;
pub(super) mod settings;

// appConst.billType* values: the office rental is issued by the billing scan;
// the other known client types settle as existing pure debts only.
use infinity_core::consts::{BILL_TYPE_RENTAL as BILL_RENTAL, SETTLED_BILL_TYPES};

fn bill_row(r: &bills_dao::Bill) -> Value {
    // corpBillsUtil.GetTextForBill reads a rental bill's externalID as the
    // typeOfficeRental type id and externalID2 as the station; the database keeps
    // the real office item id and projects it here. Other types keep their
    // stored externals; a missing one is the client's -1 sentinel.
    let missing = Some(-1);
    let (external, external2) = if r.bill_type == BILL_RENTAL {
        (Some(TYPE_OFFICE_RENTAL as i64), r.external_id2.or(missing))
    } else {
        (r.external_id.or(missing), r.external_id2.or(missing))
    };
    keyval([
        ("billID", Value::Int(r.bill_id)),
        ("billTypeID", Value::Int(r.bill_type as i64)),
        ("debtorID", Value::Int(r.debtor_id)),
        ("creditorID", Value::Int(r.creditor_id)),
        ("amount", Value::Float(r.amount.wire_isk())),
        ("dueDateTime", Value::Int(r.due)),
        ("interest", Value::Float(r.interest)),
        ("externalID", external.map(Value::Int).unwrap_or(Value::None)),
        ("externalID2", external2.map(Value::Int).unwrap_or(Value::None)),
        ("paid", Value::Bool(false)),
    ])
}
/// Current management authorization for a corporation or its executor alliance.
pub(super) async fn authorize_read(db: &Db, me: i64, owner: i64) -> Result<(), CallError> {
    let authority = crate::dao::characters::roles::authorities(db, &[me]).await.map_err(db_err)?
        .into_iter().next();
    let management = match authority {
        Some(authority) => {
            // An alliance's bills belong to its current executor's authorized members.
            let managing_corp = if authority.corporation == owner {
                Some(authority.corporation)
            } else {
                crate::dao::alliances::executor_corp(db, owner).await.map_err(db_err)?
            };
            managing_corp == Some(authority.corporation)
                && (crate::services::corps::permissions::allows(&authority, authority.corporation, ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT | ROLE_DIRECTOR)
                    || crate::services::corps::permissions::active_ceo(&authority, me))
        }
        None => false,
    };
    if !management {
        return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
    }
    Ok(())
}

/// Unpaid bills `owner` owes (`creditor` false) or is owed.
pub(super) async fn bills(db: &Db, me: i64, owner: i64, creditor: bool) -> CallResult {
    authorize_read(db, me, owner).await?;
    let rows = bills_dao::list(db, owner, creditor).await.map_err(db_err)?;
    Ok(Value::list(rows.iter().map(bill_row)))
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
        "GetCorporationBills" => bills(db, me, corp, false).await,
        "GetCorporationBillsReceivable" => bills(db, me, corp, true).await,
        // PayCorporationBill(billID, fromAccountKey=...)
        "PayCorporationBill" => {
            let key = match call.kwargs.get("fromAccountKey").or_else(|| call.args.get(1)) {
                Some(value) => match value.as_int().and_then(|key| i32::try_from(key).ok()) {
                    Some(key) => key,
                    None => return Some(Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED))),
                },
                None => ACCOUNT_CASH,
            };
            settlement::pay(db, world, int(0).unwrap_or(0), settlement::Payer::Corporation { actor: me, corporation: corp, key }).await.map(|_| Value::None)
        }
        "CharPayBill" => {
            let bill = int(0).unwrap_or(0);
            settlement::pay(db, world, bill, settlement::Payer::Character { actor: me }).await.map(|_| Value::None)
        }
        // {ownerID: {billTypeID: bool, 'divisionID': accountKey}}
        "GetAutomaticPaySettings" => settings::read(db, me).await,
        // SendAutomaticPaySettings({ownerID: {billTypeID: bool, 'divisionID': accountKey}})
        "SendAutomaticPaySettings" => settings::save(db, me, call.args.first()).await.map(|_| Value::None),
        _ => return None,
    })
}
#[cfg(all(test, any(feature = "tests-economy", feature = "tests-social")))]
pub(super) mod tests;
