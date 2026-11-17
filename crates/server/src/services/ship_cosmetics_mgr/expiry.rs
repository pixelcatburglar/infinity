//! Durable per-character SKIN expiry and removal notifications.
//! The nearest deadline is scheduled in `game.jobs`; expiry is strict (`expires < now`).
//! Licenses and applications are stored in `game.ship_skin_licenses` and
//! `game.ship_skin_applications`, with ships in `game.items`.
//! `FirstPartySkin.expired` supplies the client expiry boundary;
//! `shipSkinApplicationSvc.py` handles removal and `cosmeticsSvc.py` activation metadata.
//! Zero-application removals are reflected on next login by server collection policy;
//! notices are not replayed. Structure and single-use SKINs are unsupported.
//! The due-removal step is also shared by bulk grants, which reschedule once
//! per operation instead of per skin.
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};
use marshal::Value;
use serde::{Deserialize, Serialize};
use crate::{client::filetime_now, dao::{characters::selection::lock_character, ship_skins}, db::{executor::Executor, outbox::{dispatch_world, Outbox}, Db}, scheduler::{self, Job}, services::{db_err, market, CallError}, space::{ship_cosmetics, World}};
use super::state::{self, State};

/// Server-designed identifier for durable SKIN expiry jobs.
pub(crate) const KIND: &str = "ship_skins.license.expire";
#[derive(Serialize, Deserialize)]
struct Payload { character: i64 }
fn id(character: i64) -> String { format!("{KIND}:{character}") }

/// One applied ship's captured publication target: the actor fence when the
/// hull is in space, and the effective pre-transaction projection used to
/// detect deadline changes.
struct Target {
    fact: ship_cosmetics::Ship,
    fence: Option<ship_cosmetics::SkinFence>,
    skin_id: Option<i32>,
    expires_at: Option<i64>,
}
#[derive(Clone, Default)]
pub(super) struct Targets(Arc<HashMap<i64, Target>>);

pub(super) async fn prepare(db: &Db, world: &World, character: i64) -> Result<Targets, CallError> {
    let rows = ship_skins::applications_for_character(db, character).await.map_err(db_err)?;
    let session = world.online_session(character);
    let mut facts = Vec::with_capacity(rows.len());
    let mut fences = Vec::with_capacity(rows.len());
    for row in &rows {
        let active = session.as_ref().filter(|s| s.ship_id == Some(row.ship_item_id));
        let system = active.filter(|s| s.station_id.is_none()).and_then(|s| s.system_id).unwrap_or(row.location_id);
        let fence = ship_cosmetics::capture(world, system, row.ship_item_id).await
            .filter(|fence| !fence.fact().is_npc && fence.fact().type_id == row.type_id);
        facts.push(fence.as_ref().map(|fence| fence.fact().clone()).unwrap_or(ship_cosmetics::Ship {
            ship_id: row.ship_item_id, type_id: row.type_id,
            character_id: active.map(|s| s.character_id), is_npc: false,
        }));
        fences.push(fence);
    }
    let prepared = state::prepare(db, world.sde(), &facts, filetime_now()).await?;
    let mut targets = HashMap::new();
    for ((row, fence), prepared) in rows.into_iter().zip(fences).zip(prepared) {
        targets.insert(row.ship_item_id, Target {
            fact: prepared.fact, fence,
            skin_id: prepared.state.skin_id, expires_at: prepared.expires_at,
        });
    }
    Ok(Targets(Arc::new(targets)))
}
pub(super) async fn schedule(ex: &(impl Executor + ?Sized), character: i64) -> Result<(), CallError> {
    if let Some(expiry) = ship_skins::next_expiry(ex, character).await.map_err(db_err)? {
        let due_at = expiry.checked_add(1).ok_or_else(|| CallError::Internal(anyhow::anyhow!("SKIN expiry deadline overflow")))?;
        let payload = serde_json::to_string(&Payload { character }).map_err(|e| CallError::Internal(e.into()))?;
        scheduler::schedule(ex, &Job { id: id(character), kind: KIND.into(), due_at, payload }).await?;
    } else { scheduler::cancel(ex, &id(character)).await?; }
    Ok(())
}


/// Commit the post-commit projection of every target whose effective state
/// changed inside the transaction: a renewal, upgrade or grant updates the
/// stored deadline and version for ships applying the same skin ID, not just
/// the skin ID itself. Ships still on their actor publish through the captured
/// fence; the rest keep the direct owner notice.
pub(super) async fn commit_changes_tx<X: Send>(ex: &(impl Executor + ?Sized), sde: &crate::sde::Sde, character: i64, now: i64, targets: &Targets, outbox: &mut Outbox<X>) -> Result<(), CallError> {
    if targets.0.is_empty() { return Ok(()); }
    let facts: Vec<ship_cosmetics::Ship> = targets.0.values().map(|target| target.fact.clone()).collect();
    for (target, prepared) in targets.0.values().zip(state::prepare(ex, sde, &facts, now).await?) {
        if prepared.state.skin_id == target.skin_id && prepared.expires_at == target.expires_at { continue; }
        match &target.fence {
            Some(fence) => outbox.ship_cosmetics(fence.committed(prepared)),
            None => for notice in crate::gateway::ship_cosmetics::notice(&prepared.state, &[character])? { outbox.notify_gateway(notice); },
        }
    }
    Ok(())
}
pub(super) async fn remove_tx<X: Send>(ex: &(impl Executor + ?Sized), character: i64, skin: i32, before: Option<i64>, targets: &Targets, outbox: &mut Outbox<X>) -> Result<Option<usize>, CallError> {
    let Some(removed) = ship_skins::remove_license(ex, character, skin, before).await.map_err(db_err)? else { return Ok(None) };
    let count = removed.applications.len();
    for app in removed.applications {
        outbox.notify_pilot(character, "OnSkinLicenseRemoved", vec![Value::Int(i64::from(removed.skin_id)), Value::Int(app.ship_item_id), Value::Int(i64::from(app.type_id))]);
        let target = targets.0.get(&app.ship_item_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing SKIN removal target")))?;
        let state = State { ship_id: app.ship_item_id, character_id: target.fact.character_id.unwrap_or(0), skin_id: None };
        match &target.fence {
            Some(fence) => outbox.ship_cosmetics(fence.committed(ship_cosmetics::PreparedSkin {
                fact: target.fact.clone(), state, expires_at: None,
            })),
            None => for notice in crate::gateway::ship_cosmetics::notice(&state, &[character])? { outbox.notify_gateway(notice); },
        }
    }
    Ok(Some(count))
}

/// Remove every due license and queue its effects; reports whether any row was
/// actually removed. Rescheduling the nearest deadline stays with the caller.
pub(super) async fn expire_due_tx<X: Send>(ex: &(impl Executor + ?Sized), character: i64, now: i64, targets: &Targets, outbox: &mut Outbox<X>) -> Result<bool, CallError> {
    let mut removed = false;
    for row in ship_skins::due_licenses(ex, character, now).await.map_err(db_err)? {
        if remove_tx(ex, character, row.skin_id, Some(now), targets, outbox).await?.is_some() { removed = true; }
    }
    Ok(removed)
}

pub(super) async fn expire_tx<X: Send>(ex: &(impl Executor + ?Sized), character: i64, now: i64, targets: &Targets, outbox: &mut Outbox<X>) -> Result<(), CallError> {
    expire_due_tx(ex, character, now, targets, outbox).await?;
    schedule(ex, character).await
}

pub(crate) async fn converge(db: &Db, world: &Arc<World>, character: i64, now: i64) -> Result<(), CallError> {
    let guard = market::lock().await;
    converge_locked(db, world, character, now, &guard).await
}

/// The convergence critical section under a caller-held market guard; an entry
/// reuses the same lock to refuse a failed actor before any expiry write.
pub(crate) async fn converge_locked(db: &Db, world: &Arc<World>, character: i64, now: i64, _market: &market::economy_lock::Guard) -> Result<(), CallError> {
    if ship_skins::next_expiry(db, character).await.map_err(db_err)?.is_none() { return Ok(()); }
    let targets = prepare(db, world, character).await?;
    db.transaction(|tx, outbox: &mut Outbox| {
        let targets = targets.clone();
        Box::pin(async move {
            lock_character(tx, character).await.map_err(db_err)?;
            expire_tx(tx, character, now, &targets, outbox).await
        })
    }, |value, outbox| async move { let result = dispatch_world(value, outbox, world); scheduler::wake(); result }).await
}

pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let payload: Payload = serde_json::from_str(&job.payload)?;
        if payload.character <= 0 || job.kind != KIND || job.id != id(payload.character) { return Ok(()); }
        converge(&db, &world, payload.character, filetime_now()).await.map_err(|e| anyhow::anyhow!("SKIN expiry failed: {e:?}"))
    })
}
pub(crate) async fn recover(db: &Db, world: &Arc<World>) -> Result<(), CallError> {
    for character in ship_skins::finite_characters(db).await.map_err(db_err)? { converge(db, world, character, filetime_now()).await?; }
    Ok(())
}

#[cfg(all(test, feature = "tests-ships"))]
mod tests;
