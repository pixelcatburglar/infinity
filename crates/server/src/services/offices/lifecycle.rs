//! Office lease lifecycle: rent, unrent and deadline materialization.
//! Rent creates or reactivates the office item for one full term paid from the
//! master division; an already-active lease refuses (renewal is its rental bill).
//! Unrent — local or remote bound station — converts the lease to Impounded with
//! retained contents or to Released when empty, and cancels its unearned renewal
//! bills. Expiry materializes the same transition through the billing scheduler,
//! guarded by the exact term so a stale pass cannot touch a renewed lease.
//! A pure lease flip takes the retained office item's row lock after the
//! organization admission locks, serializing with non-economic subtree writers;
//! descendants and fittings are never loaded for a state change.

use super::{may_rent, rent_price, OFFICE_ITEM_FLAG};
use super::notifications::{management_members, office_rental_notice};
use crate::sde::Sde;
use crate::client::filetime_now;
use crate::dao::{bills as bills_dao, characters::members, corporations::registry, items::creation, offices};
use crate::db::{executor::Executor, outbox::dispatch_world, Db};
use crate::isk::IskAmount;
use crate::services::{corps::is_player_corp, db_err, market, notify_error, CallError};
use crate::services::identity::Identity;
use crate::space::World;
use infinity_core::consts::{ACCOUNT_CASH, FILETIME_DAY, RENTAL_DAYS, TYPE_OFFICE};
use infinity_core::messages;
use std::sync::Arc;
use tracing::info;

/// The guests currently docked at `station`, for the public rental refresh.
fn station_guests(world: &World, station: i64) -> Vec<i64> {
    world.station_guests(station).iter().map(|session| session.character_id).collect()
}

/// The retained office item's row lock; `false` means the item is gone, which a
/// writer proves by refusing after it re-reads the lease.
async fn lock_office_item(ex: &(impl Executor + ?Sized), office: i64) -> Result<bool, CallError> {
    crate::dao::items::ship::lock_reload_ship(ex, office).await.map_err(db_err)
}

/// Rent `corp`'s office at `station` for one term; `quote` is the client's agreed
/// price, compared exactly against the current rent inside the transaction.
pub(super) async fn rent(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, station: i64, quote: IskAmount) -> Result<i64, CallError> {
    super::require_station(sde, station)?;
    let (me, corp) = (identity.char_id, identity.corp_id);
    let guests = station_guests(world, station);
    let _trading = market::lock().await;
    db.transaction(
        |tx, outbox| {
            let guests = guests.clone();
            let sde = sde.clone();
            Box::pin(async move {
            let price = IskAmount::from_isk(rent_price(&sde, station))?;
            if quote != price {
                return Err(notify_error(messages::OFFICE_RENT_CHANGED));
            }
            if !registry::lock_admission(tx, &[], &[corp], &[me]).await.map_err(db_err)?
                || !is_player_corp(corp) || !may_rent(tx, me, corp).await? {
                return Err(notify_error(messages::NO_OFFICE_RENTAL_ROLE));
            }
            let now = filetime_now();
            let until = now.checked_add(RENTAL_DAYS * FILETIME_DAY).ok_or_else(|| notify_error(messages::OFFICE_RENT_CHANGED))?;
            if let Some(lease) = offices::identity(tx, corp, station).await.map_err(db_err)? {
                if !lock_office_item(tx, lease.office_id).await? {
                    return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
                }
                let now = filetime_now();
                let until = now.checked_add(RENTAL_DAYS * FILETIME_DAY).ok_or_else(|| notify_error(messages::OFFICE_RENT_CHANGED))?;
                let lease = offices::identity(tx, corp, station).await.map_err(db_err)?
                    .filter(|current| current.office_id == lease.office_id && !current.is_active(now))
                    .ok_or_else(|| notify_error(messages::offices::OFFICE_ALREADY_RENTED))?;
                if offices::active_count(tx, station, now).await.map_err(db_err)? >= crate::config::game().corporations.office_slots_per_station {
                    return Err(notify_error(messages::offices::OFFICE_NO_FREE_SLOTS));
                }
                super::super::wallets::corp_debit(tx, outbox, corp, ACCOUNT_CASH, price).await?;
                // A retained impounded or released office is reused as-is; an
                // expired Active row reactivates on the same guarded transition.
                if !offices::transition(tx, lease.office_id, lease.state, lease.rented_until, offices::LeaseState::Active, until, now).await.map_err(db_err)? {
                    return Err(notify_error(messages::OFFICE_RENT_CHANGED));
                }
                bills_dao::cancel_unpaid_renewals(tx, lease.office_id, corp, i64::MIN, now).await.map_err(db_err)?;
                let members = members::active_ids(tx, corp).await.map_err(db_err)?;
                office_rental_notice(outbox, corp, lease.office_id, &members, &guests);
                return Ok((lease.office_id, false));
            }
            if offices::active_count(tx, station, now).await.map_err(db_err)? >= crate::config::game().corporations.office_slots_per_station {
                return Err(notify_error(messages::offices::OFFICE_NO_FREE_SLOTS));
            }
            super::super::wallets::corp_debit(tx, outbox, corp, ACCOUNT_CASH, price).await?;
            let office = creation::rookie_ship(tx, TYPE_OFFICE, corp, station, OFFICE_ITEM_FLAG).await.map_err(db_err)?;
            offices::insert(tx, office, corp, station, until, now).await.map_err(db_err)?;
            let members = members::active_ids(tx, corp).await.map_err(db_err)?;
            office_rental_notice(outbox, corp, office, &members, &guests);
            Ok((office, true))
        })},
        |(office, created), outbox| async move {
            dispatch_world((), outbox, world)?;
            if created {
                info!("offices: {corp} rented office {office} at {station} for {:.0} ISK", quote.wire_isk());
            }
            Ok(office)
        },
    ).await
}

/// Give up `corp`'s office lease at `station`, local or remote; contents stay
/// inside the retained office item under Impounded, an empty office is Released.
pub(super) async fn unrent(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, station: i64) -> Result<(), CallError> {
    super::require_station(sde, station)?;
    let (me, corp) = (identity.char_id, identity.corp_id);
    let guests = station_guests(world, station);
    let _trading = market::lock().await;
    db.transaction(
        |tx, outbox| {
            let guests = guests.clone();
            let sde = sde.clone();
            Box::pin(async move {
            if !registry::lock_admission(tx, &[], &[corp], &[me]).await.map_err(db_err)?
                || !is_player_corp(corp) || !may_rent(tx, me, corp).await? {
                return Err(notify_error(messages::NO_OFFICE_RELEASE_ROLE));
            }
            let Some(lease) = offices::identity(tx, corp, station).await.map_err(db_err)? else {
                return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
            };
            // Only a live or expired-but-unmaterialized Active lease can end.
            if lease.state != offices::LeaseState::Active {
                return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
            }
            if !lock_office_item(tx, lease.office_id).await? {
                return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
            }
            let now = filetime_now();
            let next = if offices::has_items(tx, lease.office_id).await.map_err(db_err)? {
                offices::LeaseState::Impounded
            } else {
                offices::LeaseState::Released
            };
            if !offices::transition(tx, lease.office_id, offices::LeaseState::Active, lease.rented_until, next, lease.rented_until, now).await.map_err(db_err)? {
                return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
            }
            bills_dao::cancel_unpaid_renewals(tx, lease.office_id, corp, i64::MIN, now).await.map_err(db_err)?;
            let members = members::active_ids(tx, corp).await.map_err(db_err)?;
            office_rental_notice(outbox, corp, lease.office_id, &members, &guests);
            let rows = super::super::items::load_items(tx, &sde, crate::dao::items::loading::ItemQuery::Location(lease.office_id)).await.map_err(db_err)?;
            let audience = management_members(tx, corp).await?;
            super::notifications::corp_asset_notice(outbox, station, rows.iter().map(|item| item.row()).collect(), &audience);
            Ok(())
        })},
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
}

/// Materialize one expired lease; the expected term keeps a renewed lease safe
/// from a stale pass.
pub(in crate::services) async fn expire_current(ex: &(impl Executor + ?Sized), due: &bills_dao::DueLease) -> Result<offices::LeaseState, CallError> {
    if !lock_office_item(ex, due.office_id).await? {
        return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
    }
    let now = filetime_now();
    if now < due.rented_until {
        return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
    }
    let current = offices::get(ex, due.office_id).await.map_err(db_err)?;
    if !current.is_some_and(|lease| lease.corporation_id == due.corporation_id
        && lease.station_id == due.station_id && lease.state == offices::LeaseState::Active
        && lease.rented_until == due.rented_until) {
        return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
    }
    let next = if offices::has_items(ex, due.office_id).await.map_err(db_err)? {
        offices::LeaseState::Impounded
    } else {
        offices::LeaseState::Released
    };
    if !offices::transition(ex, due.office_id, offices::LeaseState::Active, due.rented_until, next, due.rented_until, now).await.map_err(db_err)? {
        return Err(notify_error(messages::offices::OFFICE_NOT_RENTED));
    }
    // The unpaid renewal for this boundary is terminally expired, never payable
    // against a later term.
    if let Some(bill) = bills_dao::renewal_for(ex, due.office_id, due.corporation_id, due.rented_until).await.map_err(db_err)? {
        bills_dao::close_unpaid(ex, bill.bill_id, infinity_core::consts::BILL_STATUS_EXPIRED, now).await.map_err(db_err)?;
    }
    Ok(next)
}
