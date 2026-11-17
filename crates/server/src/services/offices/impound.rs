//! Impounded office contents: release to the corporation's station Deliveries for
//! a fixed fraction of the current rent, or destroy them entirely (both are
//! Director/active-CEO operations validated against the shared inventory closure
//! lifecycle). The retained office item is never moved or deleted; a re-rented
//! office restores its contents with no second release fee.

use super::notifications::{management_members, office_rental_notice};
use crate::client::filetime_now;
use crate::dao::{bills as bills_dao, characters::members, corporations::registry, offices};
use crate::db::Db;
use crate::isk::IskAmount;
use crate::sde::Sde;
use crate::space::{NotifyTx, World};
use crate::services::identity::Identity;
use crate::services::inventory::destination::Destination;
use crate::services::inventory::lifecycle::{self, CustodyRule, Operation, Prepared};
use crate::services::inventory::movement::{dispatch_move, move_core, MoveEffect};
use crate::services::{corps::is_player_corp, db_err, items::load_items, market, notify_error, CallError};
use infinity_core::consts::{ACCOUNT_CASH, FLAG_CORP_DELIVERIES};
use infinity_core::messages;
use std::sync::Arc;
use tracing::info;

/// Only a Director or the active CEO may release or trash impounded contents.
async fn may_manage_impound(ex: &(impl crate::db::executor::Executor + ?Sized), me: i64, corp: i64) -> Result<bool, CallError> {
    Ok(crate::services::corps::permissions::load(ex, me).await?
        .is_some_and(|authority| authority.corporation == corp
            && (crate::services::corps::permissions::allows(&authority, corp, infinity_core::consts::ROLE_DIRECTOR)
                || crate::services::corps::permissions::active_ceo(&authority, me))))
}

/// An inactive retained lease can receive contents from an existing obligation,
/// even after an empty lease was marked Released.
async fn impounded_lease(ex: &(impl crate::db::executor::Executor + ?Sized), corp: i64, station: i64, now: i64) -> Result<Option<offices::Lease>, CallError> {
    let Some(lease) = offices::identity(ex, corp, station).await.map_err(db_err)? else { return Ok(None) };
    Ok((!lease.is_active(now)).then_some(lease))
}

/// Whether the corporation has releasable impounded contents at `station`.
pub(super) async fn has_impounded(ex: &(impl crate::db::executor::Executor + ?Sized), corp: i64, station: i64) -> Result<bool, CallError> {
    let now = filetime_now();
    let Some(lease) = impounded_lease(ex, corp, station, now).await? else { return Ok(false) };
    offices::has_items(ex, lease.office_id).await.map_err(db_err)
}

/// The release price: the configured fraction of the station's current rent.
pub(super) fn release_price(sde: &Sde, station: i64) -> Result<IskAmount, CallError> {
    let fraction = crate::config::game().corporations.office_impound_release_fraction;
    IskAmount::from_isk(super::rent_price(sde, station) * fraction).map_err(db_err)
}

/// Capture the impounded closure for the shared lifecycle: roots are the office's
/// direct children, the expected outer ancestor is the office's station and the
/// retained office item is the required lineage boundary.
async fn capture_impound(db: &Db, sde: &Arc<Sde>, office: i64, station: i64, notify: &NotifyTx) -> Result<Prepared, CallError> {
    let roots = offices::child_ids(db, office).await.map_err(db_err)?;
    let snapshots_ids = crate::dao::items::inventory::descendants(db, &roots).await.map_err(db_err)?;
    let mut snapshots = load_items(db, sde, crate::dao::items::loading::ItemQuery::Ids(&snapshots_ids)).await.map_err(db_err)?;
    snapshots.sort_by_key(|item| item.item_id);
    let mut lineages = std::collections::HashMap::new();
    for item in &snapshots {
        if !lineages.contains_key(&item.location_id) {
            lineages.insert(item.location_id, crate::dao::items::deliveries::ancestry(db, item.location_id).await.map_err(db_err)?);
        }
    }
    Ok(Prepared {
        roots,
        snapshots,
        lineages,
        outer: station,
        boundary: Some(office),
        endpoints: Vec::new(),
        reach: None,
        shared_ids: std::collections::HashSet::new(),
        active_ship: None,
        station: None,
        notify: Some(notify.clone()),
    })
}

/// Move the verified direct roots of the impounded office to the corporation's
/// station Deliveries, preserving every descendant's flag and identity.
pub(super) async fn release(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, sender: NotifyTx, station: i64, quote: IskAmount) -> Result<(), CallError> {
    super::require_station(sde, station)?;
    let (me, corp) = (identity.char_id, identity.corp_id);
    let guests = world.station_guests(station).iter().map(|session| session.character_id).collect::<Vec<_>>();
    let _trading = market::lock().await;
    let Some(lease) = offices::identity(db, corp, station).await.map_err(db_err)? else {
        return Err(notify_error(messages::offices::OFFICE_IMPOUND_EMPTY));
    };
    let prepared = capture_impound(db, sde, lease.office_id, station, &sender).await?;
    let custody = CustodyRule::Corporate { corporation: corp };
    let released = db.transaction(
        |tx, outbox: &mut crate::db::outbox::Outbox<MoveEffect>| {
            let prepared = prepared.clone(); let custody = custody; let guests = guests.clone(); let sde = sde.clone();
            Box::pin(async move {
                if !registry::lock_admission(tx, &[], &[corp], &[me]).await.map_err(db_err)?
                    || !is_player_corp(corp) || !may_manage_impound(tx, me, corp).await? {
                    return Err(notify_error(messages::NO_OFFICE_RELEASE_ROLE));
                }
                let verified = lifecycle::verify_closure(tx, &sde, &prepared, &custody, Operation::Release).await?;
                let now = filetime_now();
                let Some(current) = impounded_lease(tx, corp, station, now).await? else {
                    return Err(notify_error(messages::offices::OFFICE_IMPOUND_EMPTY));
                };
                if current.office_id != lease.office_id || current.rented_until != lease.rented_until {
                    return Err(notify_error(messages::offices::OFFICE_IMPOUND_EMPTY));
                }
                let roots: Vec<_> = verified.items.iter().filter(|item| item.location_id == current.office_id).cloned().collect();
                if roots.is_empty() {
                    // Nothing to release: the retained identity becomes Released
                    // and no fee is taken.
                    if !offices::transition(tx, current.office_id, current.state, current.rented_until, offices::LeaseState::Released, current.rented_until, now).await.map_err(db_err)? {
                        return Err(notify_error(messages::offices::OFFICE_IMPOUND_EMPTY));
                    }
                    let members = members::active_ids(tx, corp).await.map_err(db_err)?;
                    office_rental_notice(outbox, corp, current.office_id, &members, &guests);
                    return Ok(false);
                }
                let price = release_price(&sde, station)?;
                if quote != price {
                    return Err(notify_error(messages::offices::OFFICE_RELEASE_QUOTE_CHANGED));
                }
                super::super::wallets::corp_debit(tx, outbox, corp, ACCOUNT_CASH, price).await?;
                let destination = Destination::owned(corp, station, FLAG_CORP_DELIVERIES);
                for item in &roots {
                    let moved = move_core(tx, &sde, outbox, item, &destination, None, crate::services::inventory::destination::Custody::Preserve,
                        &verified.locks, None, None, me).await?
                        .ok_or_else(|| notify_error(messages::offices::OFFICE_IMPOUND_UNSAFE))?;
                    outbox.event(crate::lua::events::GameEvent::ItemsMoved {
                        char_id: me, item_id: moved, from_location_id: item.location_id,
                        to_location_id: station, quantity: crate::services::item_volume::stack_size(item.singleton, item.quantity),
                    });
                }
                if !offices::transition(tx, current.office_id, current.state, current.rented_until, offices::LeaseState::Released, current.rented_until, now).await.map_err(db_err)? {
                    return Err(notify_error(messages::offices::OFFICE_IMPOUND_EMPTY));
                }
                bills_dao::cancel_unpaid_renewals(tx, current.office_id, corp, i64::MIN, now).await.map_err(db_err)?;
                let members = members::active_ids(tx, corp).await.map_err(db_err)?;
                office_rental_notice(outbox, corp, current.office_id, &members, &guests);
                let audience = management_members(tx, corp).await?;
                let root_ids: Vec<_> = roots.iter().map(|item| item.item_id).collect();
                let moved = load_items(tx, &sde, crate::dao::items::loading::ItemQuery::Ids(&root_ids)).await.map_err(db_err)?;
                super::notifications::corp_asset_notice(outbox, station, moved.iter().map(|item| item.row()).collect(), &audience);
                Ok(true)
            })
        },
        |value, outbox| async move {
            dispatch_move(value, outbox, world, |notification| notification.dispatch_sender(world, Some(&sender)))
        },
    ).await?;
    if released {
        info!("offices: {corp} released impounded contents of {} at {station} for {:.0} ISK", lease.office_id, quote.wire_isk());
    }
    Ok(())
}

/// Destroy every impounded descendant through the same shared lifecycle as
/// ordinary trash; the office identity becomes Released.
pub(super) async fn trash_impounded(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, sender: NotifyTx, station: i64) -> Result<(), CallError> {
    super::require_station(sde, station)?;
    let (me, corp) = (identity.char_id, identity.corp_id);
    let guests = world.station_guests(station).iter().map(|session| session.character_id).collect::<Vec<_>>();
    let _trading = market::lock().await;
    let Some(lease) = offices::identity(db, corp, station).await.map_err(db_err)? else {
        return Err(notify_error(messages::offices::OFFICE_IMPOUND_EMPTY));
    };
    let prepared = capture_impound(db, sde, lease.office_id, station, &sender).await?;
    let mut owners: Vec<_> = prepared.snapshots.iter().map(|item| item.owner_id).collect();
    owners.extend(prepared.lineages.values().flat_map(|rows| rows.iter().map(|row| row.owner_id)));
    owners.sort_unstable();
    owners.dedup();
    let fences = crate::services::effects::online::capture_online_fences(world, &owners).await;
    let custody = CustodyRule::Corporate { corporation: corp };
    db.transaction(
        |tx, outbox: &mut crate::db::outbox::Outbox<MoveEffect>| {
            let prepared = prepared.clone(); let custody = custody; let guests = guests.clone(); let sde = sde.clone(); let fences = fences.clone();
            Box::pin(async move {
                if !registry::lock_admission(tx, &[], &[corp], &[me]).await.map_err(db_err)?
                    || !is_player_corp(corp) || !may_manage_impound(tx, me, corp).await? {
                    return Err(notify_error(messages::NO_OFFICE_RELEASE_ROLE));
                }
                let verified = lifecycle::verify_closure(tx, &sde, &prepared, &custody, Operation::Destroy).await?;
                let now = filetime_now();
                let Some(current) = impounded_lease(tx, corp, station, now).await? else {
                    return Err(notify_error(messages::offices::OFFICE_IMPOUND_EMPTY));
                };
                if current.office_id != lease.office_id || current.rented_until != lease.rented_until {
                    return Err(notify_error(messages::offices::OFFICE_IMPOUND_EMPTY));
                }
                let removed: Vec<_> = verified.items.iter().filter(|item| item.location_id == current.office_id).map(|item| item.row()).collect();
                lifecycle::destroy(tx, &sde, outbox, &prepared, &custody, None, &fences, me, verified).await?;
                if !offices::transition(tx, current.office_id, current.state, current.rented_until, offices::LeaseState::Released, current.rented_until, now).await.map_err(db_err)? {
                    return Err(notify_error(messages::offices::OFFICE_IMPOUND_EMPTY));
                }
                bills_dao::cancel_unpaid_renewals(tx, current.office_id, corp, i64::MIN, now).await.map_err(db_err)?;
                let members = members::active_ids(tx, corp).await.map_err(db_err)?;
                office_rental_notice(outbox, corp, current.office_id, &members, &guests);
                let audience = management_members(tx, corp).await?;
                super::notifications::corp_asset_notice(outbox, station, removed, &audience);
                Ok(())
            })
        },
        |value, outbox| async move {
            dispatch_move(value, outbox, world, |notification| notification.dispatch_sender(world, Some(&sender)))
        },
    ).await?;
    info!("offices: {corp} trashed impounded contents of {} at {station}", lease.office_id);
    Ok(())
}
