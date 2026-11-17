//! Capsule-group medical loss shared by the combat ship-loss transaction and
//! home-station clone self-destruction, mirroring the client's `homestation`
//! flow and `idCheckers` group classification. Only a real piloted
//! capsule-group hull takes this path: the owner's unprotected active
//! augmentation rows die with the body and a standard capsule is created at
//! the resolved home station, all inside the caller's transaction. The docked
//! `self_destruct` entry owns its own market guard, online fences and
//! membership gates around one transaction; the shared pieces open no
//! transaction and hold no market, World, killmail or insurance authority,
//! and dormant clone custody is never read or locked.
use std::sync::Arc;
use infinity_core::{consts::{FLAG_HANGAR, GROUP_CAPSULE, TYPE_CAPSULE}, messages, session::OnlineSession};
use marshal::Value;
use crate::dao::{characters::{selection, ship as character_dao, social}, items::{brain, clones as custody, creation, fit, inventory, parent_locks::{LockRequest, LockStrength}}};
use crate::db::{executor::Executor, outbox::Outbox, Db};
use crate::sde::Sde;
use crate::services::{character::home_station, db_err, effects::online::{self, OnlineChanged, OnlineFences}, identity::Identity, inventory::destination, notify_error, ship::{transport::station, SessionChange}, skills::body_change, CallError};
use crate::space::World;

/// `idCheckers.py` classifies capsules by group, not by the standard type.
pub(in crate::services) fn is_capsule(sde: &Sde, type_id: i32) -> bool {
    sde.types.get(type_id).is_some_and(|ty| ty.group_id == Some(GROUP_CAPSULE))
}

/// The owner's medical lock set: every online hull plus the active brain rows,
/// merged by the caller into the unified loss request set.
pub(in crate::services) async fn body_lock_requests(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<LockRequest>, CallError> {
    let mut requests: Vec<LockRequest> = fit::owner_online_ships(ex, owner).await.map_err(db_err)?
        .into_iter().map(|item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
    requests.extend(brain::list(ex, owner).await.map_err(db_err)?
        .into_iter().map(|item| LockRequest { item_id: item.item_id, strength: LockStrength::Update }));
    Ok(requests)
}

/// Create the standard replacement capsule at the resolved home station,
/// relocate the character under one guarded write and finish the body loss.
/// The caller already consumed the old capsule in this transaction and holds
/// the character and medical item locks.
pub(in crate::services) async fn rebirth_tx<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Arc<Sde>, fences: &OnlineFences,
    identity: &Identity, home: &home_station::Station, now: i64, loss: body_change::BodyLoss,
) -> Result<(i64, body_change::BodyRefresh), CallError> {
    let expected_ship = identity.ship_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("lost capsule identity missing")))?;
    let capsule = creation::rookie_ship(ex, TYPE_CAPSULE, identity.char_id, home.station_id, FLAG_HANGAR).await.map_err(db_err)?;
    if !character_dao::set_home(ex, identity.char_id, expected_ship, identity.station_id, capsule,
        home.station_id, home.solar_system_id, home.constellation_id, home.region_id).await.map_err(db_err)? {
        return Err(CallError::Internal(anyhow::anyhow!("home station relocation refused")));
    }
    let refresh = body_change::finish_loss_tx(ex, outbox, sde, fences, identity.char_id, now, loss).await?;
    Ok((capsule, refresh))
}

/// The docked `home_station.self_destruct_clone` path: destroy only the
/// boarded capsule-group hull and its unprotected active augmentation rows,
/// then rebirth at the explicit home station. No killmail, insurance, fee,
/// jump cooldown or dormant-clone work runs here. `publish` runs once after
/// commit with the complete session change while the market guard and the
/// membership gates are still held; it must not await World or the database.
pub(in crate::services) async fn self_destruct(
    identity: &Identity, expected: &OnlineSession, db: &Db, world: &World, sde: &Arc<Sde>,
    publish: impl FnOnce(SessionChange, Outbox<OnlineChanged>) -> Result<(), CallError> + Send,
) -> Result<(), CallError> {
    let stale = || notify_error(messages::clones::STALE_BODY);
    let character = identity.char_id;
    if character <= 0 || expected.character_id != character
        || expected.ship_id != identity.ship_id || expected.station_id != identity.station_id
        || expected.system_id != identity.system_id
        || world.online_session(character).as_ref() != Some(expected) {
        return Err(stale());
    }
    let _market = crate::services::market::lock().await;
    // A racing reconnect may have replaced the session while the market
    // guard was pending; the captured world session is checked again under it.
    if world.online_session(character).as_ref() != Some(expected) { return Err(stale()); }
    let fences = online::capture_online_fences(world, &[character]).await;
    let mut systems: Vec<i64> = fences.systems().collect();
    systems.sort_unstable();
    systems.dedup();
    let mut _gates = Vec::with_capacity(systems.len());
    for system in systems {
        _gates.push(world.lock_condition_snapshot(system).await.map_err(db_err)?);
    }
    let identity = *identity;
    let expected = expected.clone();
    let sde = sde.clone();
    let now = crate::client::filetime_now();
    db.transaction(
        |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            Box::pin(async move { self_destruct_tx(tx, outbox, &sde, &fences, &identity, now).await })
        },
        |(home, capsule), outbox| async move {
            let mut change = station::session_change(station::Destination {
                station_id: home.station_id, system_id: home.solar_system_id,
                constellation_id: home.constellation_id, region_id: home.region_id,
            });
            change.updates.push(("shipid", Value::Int(capsule)));
            let mut next = expected.clone();
            next.apply_updates(change.updates.iter().map(|(key, value)| (*key, value)));
            // The committed destruction refreshes only the captured session; a
            // post-commit mismatch never reruns settlement, and the reconnected
            // client rebuilds its session from the database.
            if world.online_session(character).as_ref() != Some(&next)
                && !world.replace_online_session(character, Some(&expected), Some(next)) {
                tracing::warn!(character, "committed clone destruction session is stale");
                return Err(CallError::Internal(anyhow::anyhow!("committed clone destruction but the online session changed")));
            }
            let result = publish(change, outbox);
            crate::scheduler::wake();
            result
        },
    ).await
}

/// Transaction body of the docked self-destruction: the character lock is
/// taken first, then the live row and the explicit home station are read and
/// the three client conditions are evaluated independently. Only a passing
/// state resolves the home station and takes the unified capsule, online
/// hull and active brain lock set; the boarded capsule is re-proven before
/// the guarded delete consumes exactly it.
async fn self_destruct_tx(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>,
    sde: &Arc<Sde>, fences: &OnlineFences, identity: &Identity, now: i64,
) -> Result<(home_station::Station, i64), CallError> {
    let stale = || notify_error(messages::clones::STALE_BODY);
    let character = identity.char_id;
    selection::lock_character(ex, character).await.map_err(db_err)?;
    let Some(current) = character_dao::current(ex, character).await.map_err(db_err)? else {
        return Err(stale());
    };
    let home_id = social::home_station_id(ex, character).await.map_err(db_err)?;
    let ship_type = match current.ship {
        Some(ship) => inventory::type_id(ex, ship).await.map_err(db_err)?,
        None => None,
    };
    let already_home = home_id.is_some() && current.station == home_id;
    let not_docked = current.station.is_none();
    let not_capsule = !ship_type.is_some_and(|type_id| is_capsule(sde, type_id));
    if already_home || not_docked || not_capsule {
        return Err(notify_error(messages::clones::self_destruct_validation_error(already_home, not_docked, not_capsule)));
    }
    let Some(home_id) = home_id else {
        return Err(CallError::Internal(anyhow::anyhow!("explicit home station required")));
    };
    let home = home_station::resolve(sde, home_id)?;
    let (Some(ship), Some(station)) = (current.ship, current.station) else {
        return Err(CallError::Internal(anyhow::anyhow!("validated capsule state incomplete")));
    };
    // Docking at a non-NPC structure follows the existing unsupported
    // boundary rather than the medical path.
    if sde.map.station(station).is_none() {
        return Err(notify_error(messages::CLONE_STRUCTURE_INSTALL_UNSUPPORTED));
    }
    // The request must still name the locked hull tuple; a stale request may
    // never consume a replacement ship.
    if identity.ship_id != Some(ship) || identity.station_id != Some(station) || identity.system_id != current.system {
        return Err(stale());
    }
    let mut requests = vec![LockRequest { item_id: ship, strength: LockStrength::Update }];
    requests.extend(body_lock_requests(ex, character).await?);
    let locks = destination::lock_items(ex, sde, &requests, &[]).await
        .map_err(|error| match error {
            CallError::User { ref msg, .. } if msg == messages::ITEM_NOT_AT_ORIGINAL_LOCATION => stale(),
            error => error,
        })?;
    let Some(hull) = locks.row(ship) else { return Err(stale()); };
    if hull.owner_id != character || hull.location_id != station || hull.flag_id != FLAG_HANGAR
        || !hull.singleton || hull.quantity != 1 || hull.jump_clone_id.is_some() || !is_capsule(sde, hull.type_id) {
        return Err(stale());
    }
    let capsule_type = hull.type_id;
    let loss = body_change::begin_loss_tx(ex, outbox, sde, character, now, |_| {}).await?;
    if custody::delete_capsule(ex, character, ship, capsule_type).await.map_err(db_err)? != 1 {
        return Err(stale());
    }
    let (capsule, _refresh) = rebirth_tx(ex, outbox, sde, fences, identity, &home, now, loss).await?;
    Ok((home, capsule))
}
