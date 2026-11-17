//! The one exact bill settlement for every payer: a character paying their own
//! debt, an authorized corporation member paying from a chosen division, an
//! alliance's current executor paying from the alliance's configured division,
//! or the scheduler's automatic payment for a configured owner. Authority is
//! re-read inside the transaction; stale scanned or client-supplied settings,
//! executors and divisions never shortcut it. An office rental bill pays only
//! while its covered term is still live and extends the lease exactly from its
//! boundary; every other known client type settles the debt alone.

use infinity_core::consts::{BILL_TYPE_RENTAL as BILL_RENTAL, SETTLED_BILL_TYPES};
use crate::client::filetime_now;
use crate::dao::{bills as bills_dao, corporations::registry, offices};
use crate::db::{outbox::dispatch_world, Db};
use crate::isk::IskAmount;
use crate::services::{corpwallet, db_err, market, notify_error, CallError};
use crate::space::World;
use infinity_core::consts::{ACCOUNT_CASH, ROLE_ACCOUNTANT};
use infinity_core::messages;
use tracing::info;

/// Who settles a bill; the scheduler may only ever construct `Automatic`.
#[derive(Clone, Copy)]
pub(crate) enum Payer {
    /// The debtor character paying their own bill.
    Character { actor: i64 },
    /// An authorized member paying from `corporation`'s division `key`.
    Corporation { actor: i64, corporation: i64, key: i32 },
    /// The alliance's current executor paying from the client's selected key on
    /// the executor wallet.
    Alliance { actor: i64, alliance: i64, executor: i64, key: i32 },
    /// The billing scan paying from the owner's configured division.
    Automatic { owner: bills_dao::BillOwner },
}

/// What a payment resolves to inside the transaction.
struct Settlement {
    /// Wallet entity actually debited (`None` for the character's own wallet).
    debtor_corp: Option<i64>,
    /// Division for a corporate debit.
    key: i32,
    /// Organizations locked for authority: (alliances, corps, characters).
    locks: (Vec<i64>, Vec<i64>, Vec<i64>),
}

/// Verify the payer against the current bill inside the transaction and map it
/// to the wallet debit; a refusal rolls everything back.
async fn admit(ex: &(impl crate::db::executor::Executor + ?Sized), bill: &bills_dao::Bill, payer: &Payer, expected_debtor: i64) -> Result<Settlement, CallError> {
    if bill.debtor_id != expected_debtor {
        return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
    }
    match *payer {
        Payer::Character { actor } => {
            if bill.debtor_id != actor {
                return Err(notify_error(messages::BILL_NOT_OWNED_BY_CHARACTER));
            }
            Ok(Settlement { debtor_corp: None, key: 0,
                locks: (vec![], vec![], vec![actor]) })
        }
        Payer::Corporation { actor, corporation, key } => {
            if bill.debtor_id != corporation {
                return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
            }
            Ok(Settlement { debtor_corp: Some(corporation), key,
                locks: (vec![], vec![corporation], vec![actor]) })
        }
        Payer::Alliance { actor, alliance, executor, key } => {
            if bill.debtor_id != alliance {
                return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
            }
            Ok(Settlement { debtor_corp: Some(executor), key,
                locks: (vec![alliance], vec![executor], vec![actor]) })
        }
        Payer::Automatic { owner } => {
            if bill.debtor_id != owner.id() {
                return Err(notify_error(messages::BILL_ALREADY_PAID_OR_MISSING));
            }
            match owner {
                bills_dao::BillOwner::Corporation(corp) => {
                    let key = bills_dao::division(ex, owner).await.map_err(db_err)?
                        .ok_or_else(|| notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED))?;
                    Ok(Settlement { debtor_corp: Some(corp), key,
                        locks: (vec![], vec![corp], vec![]) })
                }
                bills_dao::BillOwner::Alliance(alliance) => {
                    let key = bills_dao::division(ex, owner).await.map_err(db_err)?
                        .ok_or_else(|| notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED))?;
                    let Some(executor) = crate::dao::alliances::executor_corp(ex, alliance).await.map_err(db_err)? else {
                        return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
                    };
                    Ok(Settlement { debtor_corp: Some(executor), key,
                        locks: (vec![alliance], vec![executor], vec![]) })
                }
            }
        }
    }
}

/// Settle `bill` for `payer`: guarded claim, exact debit once, genuine player
/// creditor credit, term extension for a live office renewal — atomically.
pub(crate) async fn pay(db: &Db, world: &World, bill: i64, payer: Payer) -> Result<(), CallError> {
    let _trading = market::lock().await;
    let station = bills_dao::get(db, bill).await.map_err(db_err)?.and_then(|row| {
        (row.bill_type == BILL_RENTAL).then_some(row.external_id2).flatten()
    });
    let guests = station.map(|station| world.station_guests(station).iter().map(|session| session.character_id).collect::<Vec<_>>()).unwrap_or_default();
    db.transaction(
        |tx, outbox: &mut crate::db::outbox::Outbox| {
            let guests = guests.clone();
            Box::pin(async move {
            let Some(r) = bills_dao::get(tx, bill).await.map_err(db_err)? else {
                return Err(notify_error(messages::BILL_ALREADY_PAID_OR_MISSING));
            };
            if r.status != infinity_core::consts::BILL_STATUS_UNPAID {
                return Err(notify_error(messages::BILL_ALREADY_PAID_OR_MISSING));
            }
            // Pre-read identifiers only to choose the sorted lock set.
            let settlement = admit(tx, &r, &payer, match &payer {
                Payer::Character { actor } => *actor,
                Payer::Corporation { corporation, .. } => *corporation,
                Payer::Alliance { alliance, .. } => *alliance,
                Payer::Automatic { owner } => owner.id(),
            }).await?;
            let (alliances, mut corps, characters) = settlement.locks.clone();
            if super::super::corps::is_player_corp(r.creditor_id) {
                corps.push(r.creditor_id);
                corps.sort_unstable();
                corps.dedup();
            }
            if !registry::lock_admission(tx, &alliances, &corps, &characters).await.map_err(db_err)? {
                return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
            }
            // Re-read inside the lock and re-verify every authority fact.
            let Some(r) = bills_dao::get(tx, bill).await.map_err(db_err)? else {
                return Err(notify_error(messages::BILL_ALREADY_PAID_OR_MISSING));
            };
            let settlement = admit(tx, &r, &payer, match &payer {
                Payer::Character { actor } => *actor,
                Payer::Corporation { corporation, .. } => *corporation,
                Payer::Alliance { alliance, .. } => *alliance,
                Payer::Automatic { owner } => owner.id(),
            }).await?;
            let mut current_locks = settlement.locks.clone();
            if super::super::corps::is_player_corp(r.creditor_id) {
                current_locks.1.push(r.creditor_id);
                current_locks.1.sort_unstable();
                current_locks.1.dedup();
            }
            if current_locks != (alliances, corps, characters) {
                return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
            }
            if r.bill_type == BILL_RENTAL {
                let office = r.external_id.ok_or_else(|| notify_error(messages::bills::BILL_TYPE_UNKNOWN))?;
                if !crate::dao::items::ship::lock_reload_ship(tx, office).await.map_err(db_err)? {
                    return Err(notify_error(messages::bills::BILL_RENEWAL_EXPIRED));
                }
            }
            let now = filetime_now();
            match &payer {
                Payer::Character { .. } => {}
                Payer::Corporation { actor, corporation, key } => {
                    let authority = super::super::corps::permissions::load(tx, *actor).await?;
                    if !authority.is_some_and(|a| super::super::corps::permissions::allows(&a, *corporation, ROLE_ACCOUNTANT)
                        && corpwallet::can_take(&a, *corporation, *key)) {
                        return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
                    }
                }
                Payer::Alliance { actor, alliance, executor, key } => {
                    // The debtor is the alliance, the wallet its current executor;
                    // the client's selected key is Taken against at the executor.
                    if crate::dao::alliances::executor_corp(tx, *alliance).await.map_err(db_err)? != Some(*executor) {
                        return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
                    }
                    let authority = super::super::corps::permissions::load(tx, *actor).await?;
                    if !authority.is_some_and(|a| super::super::corps::permissions::allows(&a, *executor, ROLE_ACCOUNTANT)
                        && corpwallet::can_take(&a, *executor, *key)) {
                        return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
                    }
                }
                Payer::Automatic { owner } => {
                    if !corpwallet::valid_key(settlement.key) {
                        return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
                    }
                    if !bills_dao::settings(tx, owner.id()).await.map_err(db_err)?
                        .iter().any(|setting| setting.bill_type == r.bill_type && setting.enabled) {
                        return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
                    }
                    // Re-read the owner's live configuration rather than trusting
                    // the scan's snapshot.
                    if bills_dao::division(tx, *owner).await.map_err(db_err)? != Some(settlement.key) {
                        return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
                    }
                    if let bills_dao::BillOwner::Alliance(alliance) = owner {
                        // The wallet belongs to the alliance's current executor.
                        if crate::dao::alliances::executor_corp(tx, *alliance).await.map_err(db_err)? != settlement.debtor_corp {
                            return Err(notify_error(messages::BILL_PAYMENT_PERMISSION_REQUIRED));
                        }
                    }
                }
            }
            let amount: IskAmount = r.amount;
            match settlement.debtor_corp {
                Some(corp) => super::super::wallets::corp_debit(tx, outbox, corp, settlement.key, amount).await?,
                None => super::super::wallets::debit(tx, outbox, r.debtor_id, amount).await?,
            }
            if super::super::corps::is_player_corp(r.creditor_id) {
                super::super::wallets::corp_credit(tx, outbox, r.creditor_id, ACCOUNT_CASH, amount).await?;
            }
            if !bills_dao::claim_paid(tx, &r, now).await.map_err(db_err)? {
                return Err(notify_error(messages::BILL_ALREADY_PAID_OR_MISSING));
            }
            if r.bill_type == BILL_RENTAL {
                // The covered term must still be live: the office remains Active,
                // its deadline is exactly this boundary and the debtor still
                // rents it; payment extends exactly from the boundary, never now.
                let office = r.external_id.ok_or_else(|| notify_error(messages::bills::BILL_TYPE_UNKNOWN))?;
                let period_start = r.period_start.ok_or_else(|| notify_error(messages::bills::BILL_TYPE_UNKNOWN))?;
                let period_end = r.period_end.ok_or_else(|| notify_error(messages::bills::BILL_TYPE_UNKNOWN))?;
                if r.due != period_start
                    || period_start.checked_add(infinity_core::consts::RENTAL_DAYS * infinity_core::consts::FILETIME_DAY) != Some(period_end) {
                    return Err(notify_error(messages::bills::BILL_TYPE_UNKNOWN));
                }
                if now >= period_start {
                    return Err(notify_error(messages::bills::BILL_RENEWAL_EXPIRED));
                }
                let Some(lease) = offices::get(tx, office).await.map_err(db_err)? else {
                    return Err(notify_error(messages::bills::BILL_TYPE_UNKNOWN));
                };
                if lease.corporation_id != r.debtor_id || lease.station_id != r.external_id2.unwrap_or(0)
                    || lease.rented_until != period_start {
                    return Err(notify_error(messages::bills::BILL_TYPE_UNKNOWN));
                }
                if !offices::renew(tx, office, period_start, period_end, now).await.map_err(db_err)? {
                    return Err(notify_error(messages::bills::BILL_RENEWAL_EXPIRED));
                }
                if station != Some(lease.station_id) {
                    return Err(notify_error(messages::bills::BILL_TYPE_UNKNOWN));
                }
                let members = crate::dao::characters::members::active_ids(tx, r.debtor_id).await.map_err(db_err)?;
                super::super::offices::notifications::office_rental_notice(outbox, r.debtor_id, office, &members, &guests);
            } else if !SETTLED_BILL_TYPES.contains(&r.bill_type) {
                return Err(notify_error(messages::bills::BILL_TYPE_UNKNOWN));
            }
            if let Some(corp) = settlement.debtor_corp {
                for id in super::super::offices::notifications::management_members(tx, corp).await? {
                    outbox.notify_pilot(id, "OnBillReceived", vec![]);
                }
            }
            Ok((r.debtor_id, r.creditor_id, amount))
        })},
        |(debtor, creditor, amount), outbox| async move {
            dispatch_world((), outbox, world)?;
            info!("bills: {debtor} paid bill {bill} ({:.0} ISK to {creditor})", amount.wire_isk());
            Ok(())
        },
    ).await?;
    Ok(())
}
