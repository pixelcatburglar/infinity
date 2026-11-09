//! Actor-visible ship facts and deferred first-party SKIN announcements.
//! The client ship-state messengers use live ballpark visibility and game SKIN
//! tables. Deployment region changes across restart are not supported; region
//! eligibility is checked only at write boundaries. NPC IDs are actor facts,
//! not game item identities, even when their numeric IDs coincide.
use infinity_core::{consts::{CATEGORY_ENTITY, CATEGORY_SHIP}};
use crate::{client::filetime_now, db::outbox::{dispatch_world, Outbox}, gateway::ship_cosmetics as gateway, services::{identity::Identity, market, ship_cosmetics_mgr::state}, space::{Ball, System, World}};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ship { pub ship_id: i64, pub type_id: i32, pub character_id: Option<i64>, pub is_npc: bool }

fn fact(system: &System, ball: &Ball) -> Option<Ship> {
    let category = system.sde()?.types.get(ball.type_id)?.category_id;
    let is_npc = system.is_npc(ball.id);
    if category != Some(CATEGORY_SHIP) && !(category == Some(CATEGORY_ENTITY) && is_npc) { return None; }
    Some(Ship { ship_id: ball.id, type_id: ball.type_id, character_id: ball.char_id, is_npc })
}

fn scope(identity: &Identity) -> Result<Option<i64>, crate::services::CallError> {
    if identity.station_id.is_some() { return Ok(None); }
    identity.system_id.map(Some).ok_or_else(|| crate::services::CallError::Internal(anyhow::anyhow!("ship SKIN query has no location")))
}

pub(crate) async fn visible(world: &World, identity: &Identity) -> Result<Vec<Ship>, crate::services::CallError> {
    let Some(system) = scope(identity)? else { return Ok(Vec::new()) };
    let character = identity.char_id;
    world.with(system, move |s| s.balls().filter(|b| s.fleet_broadcast_visible(character, b.id)).filter_map(|b| fact(s, b)).collect()).await
        .ok_or_else(|| crate::services::CallError::Internal(anyhow::anyhow!("ship SKIN system actor unavailable")))
}

pub(crate) async fn get_visible(world: &World, identity: &Identity, ship: i64) -> Result<Option<Ship>, crate::services::CallError> {
    let Some(system) = scope(identity)? else { return Ok(None) };
    let character = identity.char_id;
    world.with(system, move |s| {
        s.fleet_broadcast_visible(character, ship).then(|| s.ball(ship).and_then(|b| fact(s, b))).flatten()
    }).await.ok_or_else(|| crate::services::CallError::Internal(anyhow::anyhow!("ship SKIN system actor unavailable")))
}

pub(crate) async fn recipients(world: &World, system: i64, ship: i64) -> Vec<i64> {
    world.with(system, move |s| s.pilots().filter_map(|b| b.char_id).filter(|&id| s.fleet_broadcast_visible(id, ship)).collect()).await.unwrap_or_default()
}

pub(crate) async fn current(world: &World, system: i64, ship: i64) -> Option<Ship> {
    world.with(system, move |s| s.ball(ship).and_then(|b| fact(s, b))).await.flatten()
}

pub(super) fn announce(system: &System, ship: i64) {
    if system.ball(ship).and_then(|ball| fact(system, ball)).is_none() { return; }
    let weak = system.world.clone(); let system_id = system.id;
    system.defer(async move {
        let Some(world) = weak.upgrade() else { return };
        let Some((mut current, mut targets)) = world.with(system_id, move |s| {
            let fact = s.ball(ship).and_then(|b| fact(s, b))?;
            let targets: Vec<_> = s.pilots().filter_map(|b| b.char_id).filter(|&id| s.fleet_broadcast_visible(id, ship)).collect();
            Some((fact, targets))
        }).await.flatten() else { return };
        if targets.is_empty() { return; }
        let _guard = if current.is_npc { None } else {
            let guard = market::lock().await;
            let Some((fact, recipients)) = world.with(system_id, move |s| {
                let fact = s.ball(ship).and_then(|b| fact(s, b))?;
                let targets: Vec<_> = s.pilots().filter_map(|b| b.char_id).filter(|&id| s.fleet_broadcast_visible(id, ship)).collect();
                Some((fact, targets))
            }).await.flatten() else { return };
            current = fact;
            targets = recipients;
            if targets.is_empty() { return; }
            Some(guard)
        };
        let result = async {
            let states = state::project(&world.db, world.sde(), &[current], filetime_now()).await?;
            let mut outbox = Outbox::default();
            for notice in gateway::notice(&states[0], &targets)? { outbox.notify_gateway(notice); }
            dispatch_world((), outbox, &world)
        }.await;
        if let Err(error) = result { tracing::warn!(?error, "ship SKIN announcement failed"); }
    });
}

pub(super) fn observe(system: &System, ego: i64) {
    let Some(character) = system.ball(ego).and_then(|b| b.char_id) else { return };
    let weak = system.world.clone(); let system_id = system.id;
    system.defer(async move {
        let Some(world) = weak.upgrade() else { return };
        let _guard = market::lock().await;
        let Some(ships) = world.with(system_id, move |s| {
            if s.pilot_ball(character).is_none_or(|b| b.id != ego) || !s.fleet_broadcast_visible(character, ego) { return None; }
            Some(s.balls().filter(|b| s.fleet_broadcast_visible(character, b.id)).filter_map(|b| fact(s, b)).collect::<Vec<_>>())
        }).await.flatten() else { return };
        let result = async {
            let states = state::project(&world.db, world.sde(), &ships, filetime_now()).await?;
            let mut outbox = Outbox::default();
            outbox.notify_gateway(gateway::all_notice(&states, character)?);
            dispatch_world((), outbox, &world)
        }.await;
        if let Err(error) = result { tracing::warn!(?error, "ship SKIN snapshot failed"); }
    });
}

#[cfg(test)] mod tests;
