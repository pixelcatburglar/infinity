//! Atomic GM fitting using the same item move and slot selection as invInventory.
//! The slash command supplies a resolved type and optional item ID; game.items
//! supplies the target's ship and inventory. Modules are not brought online.

use super::is_fitting_flag;
use super::destination::{Custody, Destination};
use super::notifications::{notify_item_change, route_target};
use crate::client::Client;
use infinity_core::consts::{CATEGORY_SHIP, FLAG_CARGO, FLAG_HANGAR, IX_LOCATION_ID};
use crate::dao::items::loading::{Item, ItemQuery};
use crate::db::outbox::Outbox;
use infinity_core::messages;
use crate::services::{db_err, items::load_items, notify_error, CallError};
use marshal::Value;
use super::movement::{dispatch_move, move_core, MoveEffect};
use crate::services::identity::Identity;

pub(super) fn available(item: &Item, target: i64, type_id: i32, ship: i64, station: Option<i64>) -> bool {
    item.owner_id == target && item.type_id == type_id && item.quantity > 0
        && ((item.location_id == ship && item.flag_id == FLAG_CARGO)
            || (station == Some(item.location_id) && item.flag_id == FLAG_HANGAR))
}

/// Fit onto the target's active ship, taking owned cargo/hangar items before creating stock.
pub(in crate::services) async fn gm_fit(c: &mut Client, target: i64, type_id: i32, item_id: Option<i64>, quantity: i32) -> Result<usize, CallError> {
    let _lock = super::super::market::lock().await;
    let refuse = || notify_error(messages::gm::GM_FIT_REFUSED);
    if quantity <= 0 { return Err(refuse()); }
    let ship = crate::dao::characters::selection::ship_id(&c.db, target).await.map_err(db_err)?.ok_or_else(refuse)?;
    let station = crate::dao::items::transfers::docked(&c.db, target).await.map_err(db_err)?;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let fences = super::super::effects::online::capture_online_fences(&c.world, &[target]).await;
    db.transaction(
        move |tx, outbox: &mut Outbox<MoveEffect>| {
            let sde = sde.clone();
            let fences = fences.clone();
            Box::pin(async move {
                let refuse = || notify_error(messages::gm::GM_FIT_REFUSED);
                if crate::dao::characters::selection::ship_id(tx, target).await.map_err(db_err)? != Some(ship)
                    || crate::dao::items::transfers::docked(tx, target).await.map_err(db_err)? != station {
                    return Err(refuse());
                }
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
                if hull.owner_id != target || hull.category_id != CATEGORY_SHIP as i64 { return Err(refuse()); }
                let source_location = station.unwrap_or(ship);
                let source_flag = if station.is_some() { FLAG_HANGAR } else { FLAG_CARGO };
                let mut sources = if let Some(id) = item_id {
                    load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?
                } else {
                    load_items(tx, &sde, ItemQuery::OwnerTypeTwoLocations(target, type_id, ship, FLAG_CARGO, source_location, source_flag)).await.map_err(db_err)?
                };
                sources.retain(|item| available(item, target, type_id, ship, station));
                let mut remaining = i64::from(quantity);
                let mut requests = vec![crate::dao::items::parent_locks::LockRequest {
                    item_id: ship, strength: crate::dao::items::parent_locks::LockStrength::Update,
                }];
                for source in &sources {
                    if remaining <= 0 { break; }
                    requests.push(crate::dao::items::parent_locks::LockRequest {
                        item_id: source.item_id, strength: crate::dao::items::parent_locks::LockStrength::Update,
                    });
                    remaining -= source.quantity.min(remaining);
                }
                let locks = super::destination::lock_items(tx, &sde, &requests, &[]).await?;
                if !locks.row(ship).is_some_and(|row| row.jump_clone_id.is_none() && row.type_id == hull.type_id
                    && row.owner_id == hull.owner_id && row.location_id == hull.location_id && row.flag_id == hull.flag_id
                    && row.quantity == hull.quantity && row.singleton == hull.singleton)
                    || sources.iter().filter(|item| locks.is_write_locked(item.item_id)).any(|item|
                        !locks.row(item.item_id).is_some_and(|row| row.jump_clone_id.is_none() && row.type_id == item.type_id
                            && row.owner_id == item.owner_id && row.location_id == item.location_id && row.flag_id == item.flag_id
                            && row.quantity == item.quantity && row.singleton == item.singleton)) {
                    return Err(refuse());
                }
                if item_id.is_some() && sources.iter().map(|item| i128::from(item.quantity)).sum::<i128>() < i128::from(quantity) { return Err(refuse()); }
                let before = super::super::effects::online::fitting::capture(tx, &sde, &fences, target, ship).await?;
                let mut final_fit = None;
                for _ in 0..quantity {
                    let item = if let Some(item) = sources.first().cloned() {
                        item
                    } else {
                        let id = crate::dao::items::transfers::insert(tx, type_id, target, source_location, source_flag, 1).await.map_err(db_err)?;
                        notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                        load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?
                    };
                    let flag = super::super::effects::fitting::free_slot(tx, sde.clone(), target, ship, item.item_id, type_id).await?.ok_or_else(|| notify_error(messages::fitting::SLOTS))?;
                    let take = (!item.singleton && item.quantity > 1).then_some(1);
                    let moved = move_core(tx, &sde, outbox, &item, &Destination::owned(target, ship, flag), take, Custody::Preserve, &locks, None, None, target).await?
                        .ok_or_else(refuse)?;
                    super::super::effects::fitting::validate_fit(tx, sde.clone(), target, ship, moved).await?;
                    final_fit = Some(super::super::effects::online::reconcile_core(tx, &sde, outbox, &fences, target, ship).await?);
                    if !sources.is_empty() {
                        if take.is_some() { sources[0].quantity -= 1; } else { sources.remove(0); }
                    }
                }
                if let Some(final_fit) = final_fit { super::super::effects::online::fitting::finish(tx, before, final_fit, outbox).await?; }
                Ok(quantity as usize)
            })
        },
        |value, outbox| async move { dispatch_move(value, outbox, &c.world, |n| super::super::effects::online::dispatch_notification(route_target(n, target, c.session.int("charid")), &c.world, &c.notify_tx, c.session.int("charid"))) },
    ).await
}



/// Unload selected modules and their bank's charges in one transaction.
pub(in crate::services) async fn gm_unload(c: &mut Client, ship: i64, owner: i64, mut ids: Vec<i64>, destination: i64, flag: i32) -> Result<usize, CallError> {
    use super::super::effects::online::{dispatch_changed, set_online_core, OnlineChanged, OnlinePolicy};
    let _lock = super::super::market::lock().await;
    let policy = OnlinePolicy::from_identity(&Identity::of_client(c), c.notify_tx.clone())?;
    if policy.owner != owner { return Err(notify_error(messages::gm::GM_FIT_REFUSED)); }
    ids.sort_unstable();
    ids.dedup();
    let db = c.db.clone();
    let sde = c.sde.clone();
    let fences = super::super::effects::online::capture_online_fences(&c.world, &[owner]).await;
    db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let ids = ids.clone();
            let policy = policy.clone();
            let fences = fences.clone();
            Box::pin(async move {
                let refuse = || notify_error(messages::gm::GM_FIT_REFUSED);
                if crate::dao::characters::selection::ship_id(tx, owner).await.map_err(db_err)? != Some(ship) {
                    return Err(refuse());
                }
                let station = crate::dao::items::transfers::docked(tx, owner).await.map_err(db_err)?;
                if (destination, flag) != station.map(|s| (s, FLAG_HANGAR)).unwrap_or((ship, FLAG_CARGO)) {
                    return Err(refuse());
                }
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
                if hull.owner_id != owner || hull.category_id != CATEGORY_SHIP as i64 { return Err(refuse()); }
                let modules = load_items(tx, &sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
                if modules.len() != ids.len() || modules.iter().any(|m| m.owner_id != owner || m.location_id != ship || !is_fitting_flag(m.flag_id)) {
                    return Err(refuse());
                }
                let mut charges = std::collections::BTreeSet::new();
                for module in &modules {
                    let master = super::super::combat::targeting_charges::master_for_member(tx, ship, module.item_id).await?;
                    charges.extend(super::super::combat::targeting_charges::bank(tx, ship, master).await?);
                }
                let charge_ids: Vec<i64> = charges.into_iter().collect();
                let charge_modules = load_items(tx, &sde, ItemQuery::Ids(&charge_ids)).await.map_err(db_err)?;
                if charge_modules.len() != charge_ids.len() || charge_modules.iter().any(|m| m.owner_id != owner || m.location_id != ship || !is_fitting_flag(m.flag_id)) {
                    return Err(refuse());
                }
                let flags: Vec<i32> = charge_modules.iter().map(|m| m.flag_id).collect();
                if !crate::dao::items::pending_reloads::for_flags(tx, ship, &flags).await.map_err(db_err)?.is_empty() {
                    return Err(refuse());
                }
                use crate::dao::items::parent_locks::{LockRequest, LockStrength};
                let mut requests = vec![LockRequest { item_id: ship, strength: LockStrength::Update }];
                requests.extend(modules.iter().chain(&charge_modules).map(|item| LockRequest { item_id: item.item_id, strength: LockStrength::Update }));
                let strict: std::collections::HashSet<_> = requests.iter().map(|request| request.item_id).collect();
                let mut preparations = Vec::new();
                let deposit_destination = Destination::owned(owner, destination, flag);
                for module in &charge_modules {
                    let loaded = super::super::combat::targeting_charges::loaded(tx, ship, module.flag_id).await?;
                    let (preparation, canonical) = match loaded {
                        Some((type_id, _)) => {
                            if let Some(index) = preparations.iter().position(|(_, prep, _): &(_, Option<super::super::items::DepositPreparation>, _)| prep.as_ref().is_some_and(|prep| prep.destination == deposit_destination && prep.type_id == type_id)) {
                                (None, Some(index))
                            } else {
                                (Some(super::super::items::DepositPreparation::capture(tx, &sde, &deposit_destination, type_id).await?), Some(preparations.len()))
                            }
                        }
                        None => (None, None),
                    };
                    preparations.push((loaded, preparation, canonical));
                }
                let optional: Vec<_> = preparations.iter().filter_map(|(_, prep, _)| prep.as_ref()?.candidate_id())
                    .filter(|id| !strict.contains(id)).collect();
                requests.extend(preparations.iter().filter_map(|(_, prep, _)| prep.as_ref()?.candidate_request()));
                let locks = super::destination::lock_items(tx, &sde, &requests, &optional).await?;
                for item in std::iter::once(&hull).chain(&modules).chain(&charge_modules) {
                    if !locks.row(item.item_id).is_some_and(|row| row.jump_clone_id.is_none() && row.type_id == item.type_id
                        && row.owner_id == item.owner_id && row.location_id == item.location_id && row.flag_id == item.flag_id
                        && row.quantity == item.quantity && row.singleton == item.singleton) { return Err(refuse()); }
                }
                if crate::dao::characters::selection::ship_id(tx, owner).await.map_err(db_err)? != Some(ship)
                    || crate::dao::items::transfers::docked(tx, owner).await.map_err(db_err)? != station { return Err(refuse()); }
                let mut current_banks = std::collections::BTreeSet::new();
                for module in &modules {
                    let master = super::super::combat::targeting_charges::master_for_member(tx, ship, module.item_id).await?;
                    current_banks.extend(super::super::combat::targeting_charges::bank(tx, ship, master).await?);
                }
                if current_banks.into_iter().collect::<Vec<_>>() != charge_ids { return Err(refuse()); }
                for (module, (loaded, _, _)) in charge_modules.iter().zip(&preparations) {
                    if super::super::combat::targeting_charges::loaded(tx, ship, module.flag_id).await? != *loaded { return Err(refuse()); }
                }
                if !crate::dao::items::pending_reloads::for_flags(tx, ship, &flags).await.map_err(db_err)?.is_empty() { return Err(refuse()); }
                let before = super::super::effects::online::fitting::capture(tx, &sde, &fences, owner, ship).await?;
                for module in &modules {
                    if super::super::effects::lifecycle::has_effect(sde.types.effects(module.type_id), infinity_core::consts::dogma::EFFECT_ONLINE as i32) {
                        set_online_core(tx, &sde, &policy, outbox, &fences, ship, module.item_id, false).await?;
                    }
                }
                for (index, module) in charge_modules.iter().enumerate() {
                    let (earlier, current) = preparations.split_at_mut(index);
                    let (_, own, canonical) = &mut current[0];
                    if let Some(canonical) = *canonical {
                        let preparation = if canonical == index { own.as_mut().unwrap() } else { earlier[canonical].1.as_mut().unwrap() };
                        super::super::combat::targeting_charges::unload(tx, &sde, outbox, owner, ship, module, destination, flag, preparation, &locks).await?;
                    }
                }
                for module in &modules {
                    move_core(tx, &sde, outbox, module, &Destination::owned(owner, destination, flag), None, Custody::Preserve, &locks, None, None, owner).await?;
                }
                let final_fit = super::super::effects::online::reconcile_core(tx, &sde, outbox, &fences, owner, ship).await?;
                super::super::effects::online::fitting::finish(tx, before, final_fit, outbox).await?;
                Ok(modules.len())
            })
        },
        |value, outbox| async move { dispatch_changed(value, outbox, c) },
    ).await
}
