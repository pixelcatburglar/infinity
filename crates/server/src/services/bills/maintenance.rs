//! The periodic billing scan (scheduler `BILLS`): issue the next-term office
//! rental bill inside the advance window, attempt automatic payment on every
//! still-live configured debt, then materialize expired leases. Each bill and
//! lease settles in its own market-locked transaction; internal errors propagate
//! to the scheduler retry while ordinary business refusals (insufficient funds,
//! a lease that moved on) skip to the next row. No grace period: at the boundary
//! an unpaid term simply expires.

use super::settlement::{self, Payer};
use crate::client::filetime_now;
use crate::dao::{bills as bills_dao, offices};
use crate::db::{outbox::dispatch_world, Db};
use crate::isk::IskAmount;
use crate::services::{db_err, market, notify_error, CallError};
use crate::space::World;
use infinity_core::consts::{FILETIME_DAY, RENTAL_DAYS};
use infinity_core::messages;
use std::future::Future;
use std::sync::Arc;
use tracing::info;

/// Issue the renewal bill for one due lease, guarded so a rerun or a concurrent
/// rent change cannot produce a duplicate period row.
async fn issue_renewal(db: &Db, world: &World, due: &bills_dao::DueLease) -> Result<(), CallError> {
    // The station's mapped landlord is the only genuine creditor; a station
    // without an owner has no one to bill and this lease is skipped.
    let Some(landlord) = world.sde().map.station(due.station_id).and_then(|st| st.owner_id) else {
        return Err(notify_error(messages::offices::OFFICE_STATION_UNSUPPORTED));
    };
    let amount = IskAmount::from_isk(super::super::offices::rent_price(world.sde(), due.station_id)).map_err(db_err)?;
    let _trading = market::lock().await;
    db.transaction(
        |tx, outbox: &mut crate::db::outbox::Outbox| {
            let due = (*due).clone();
            Box::pin(async move {
            if !crate::dao::corporations::registry::lock_admission(tx, &[], &[due.corporation_id], &[]).await.map_err(db_err)?
                || !crate::dao::items::ship::lock_reload_ship(tx, due.office_id).await.map_err(db_err)? {
                return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
            }
            let now = filetime_now();
            let end = due.rented_until.checked_add(RENTAL_DAYS * FILETIME_DAY)
                .ok_or_else(|| notify_error(messages::offices::OFFICE_NOT_RENTED))?;
            // The lease must still be Active with exactly this boundary; a
            // re-rented or unrented office cannot receive its stale bill.
            let Some(lease) = offices::get(tx, due.office_id).await.map_err(db_err)? else {
                return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
            };
            if !lease.is_active(now) || lease.rented_until != due.rented_until || lease.corporation_id != due.corporation_id || lease.station_id != due.station_id {
                return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
            }
            if !bills_dao::issue_renewal(tx, due.corporation_id, landlord, &amount, due.rented_until, due.office_id, due.station_id,
                due.rented_until, end, now).await.map_err(db_err)? {
                return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
            }
            for id in super::super::offices::notifications::management_members(tx, due.corporation_id).await? {
                outbox.notify_pilot(id, "OnBillReceived", vec![]);
            }
            Ok(())
        })},
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    info!("bills: billed {} {:.0} ISK rent for office {}", due.corporation_id, amount.wire_isk(), due.office_id);
    Ok(())
}

/// One scan: issue → automatic payment → expiry. Each row settles in its own
/// lock and transaction: a business refusal skips that row, while an internal
/// error ends this scan and the scheduler retries it.
async fn run(db: &Db, world: &World) -> Result<(), CallError> {
    static RUN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _run = RUN_LOCK.lock().await;
    let now = filetime_now();
    let ahead = crate::config::game().corporations.advance_billing_days.saturating_mul(FILETIME_DAY);
    let due = bills_dao::leases_due(db, now, ahead).await.map_err(db_err)?;
    for lease in due {
        match issue_renewal(db, world, &lease).await {
            Ok(()) => {}
            // A lease that ended or moved on between the scan and the write is
            // skipped; internal errors propagate to the scheduler retry.
            Err(CallError::User { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    let auto = bills_dao::automatic(db).await.map_err(db_err)?;
    for row in auto {
        // The scan carries the bill, debtor and its observed owner kind; the
        // settlement re-reads the bill, owner, division and executor live.
        let owner = if row.corporation {
            bills_dao::BillOwner::Corporation(row.debtor_id)
        } else {
            bills_dao::BillOwner::Alliance(row.debtor_id)
        };
        match settlement::pay(db, world, row.bill_id, Payer::Automatic { owner }).await {
            Ok(()) => {}
            Err(CallError::Internal(e)) => return Err(db_err(e)),
            Err(CallError::User { .. }) => {
                // A business refusal (insufficient balance, stale bill, changed
                // settings) leaves the bill unpaid for the next scan or expiry.
            }
        }
    }
    let expired = bills_dao::leases_expired(db, now).await.map_err(db_err)?;
    for lease in expired {
        let _trading = market::lock().await;
        let corp = lease.corporation_id;
        let station = lease.station_id;
        let office = lease.office_id;
        let sde = world.sde().clone();
        let guests = world.station_guests(station).iter().map(|session| session.character_id).collect::<Vec<_>>();
        match db.transaction(
            |tx, outbox: &mut crate::db::outbox::Outbox| {
                let guests = guests.clone();
                let lease = lease.clone();
                let sde = sde.clone();
                Box::pin(async move {
                if !crate::dao::corporations::registry::lock_admission(tx, &[], &[corp], &[]).await.map_err(db_err)? {
                    return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
                }
                super::super::offices::lifecycle::expire_current(tx, &lease).await?;
                let members = crate::dao::characters::members::active_ids(tx, corp).await.map_err(db_err)?;
                super::super::offices::notifications::office_rental_notice(outbox, corp, office, &members, &guests);
                let items = crate::services::items::load_items(tx, &sde, crate::dao::items::loading::ItemQuery::Location(office)).await.map_err(db_err)?;
                let audience = super::super::offices::notifications::management_members(tx, corp).await?;
                super::super::offices::notifications::corp_asset_notice(outbox, station, items.iter().map(|item| item.row()).collect(), &audience);
                Ok(())
            })},
            |value, outbox| async move { dispatch_world(value, outbox, world) },
        ).await {
            Ok(()) => info!("bills: office {office} at {station} expired"),
            // The lease moved on (renewed, re-rented or unrented): a stale due row
            // is skipped, not a retryable fault. Internal errors propagate.
            Err(CallError::User { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub(crate) fn run_billing(db: Db, world: Arc<World>, _: crate::scheduler::Job) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        match run(&db, &world).await {
            Ok(()) | Err(CallError::User { .. }) => Ok(()),
            Err(CallError::Internal(e)) => Err(e),
        }
    })
}
