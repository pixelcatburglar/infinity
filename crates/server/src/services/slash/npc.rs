//! GM-only NPC and debris creation. IDs of GM-spawned NPCs are process-local;
//! ordinary exploration and mission spawns are never cleared by these commands.
//! NPC definitions and debris types come from the startup SDE snapshot.

use super::{args::Args, slash_error, CallError, Client};
use infinity_core::{consts::{CATEGORY_ENTITY, GROUP_WRECK}, messages};
use crate::{space::{dist, now_ms, V3}};
use std::{collections::HashSet, sync::{Mutex, OnceLock}};

/// SDE invGroups: Cargo Container.
const GROUP_CONTAINER: i32 = 12;

fn spawned() -> &'static Mutex<HashSet<i64>> {
    static GM_SPAWNED: OnceLock<Mutex<HashSet<i64>>> = OnceLock::new();
    GM_SPAWNED.get_or_init(Default::default)
}

async fn position(c: &Client) -> Result<(i64, V3), CallError> {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else {
        return Err(slash_error(messages::GM_UNDOCK_FIRST.into()));
    };
    let pos = c.world.with(system, move |s| s.sample_position(ship)).await.flatten()
        .ok_or_else(|| slash_error(messages::GM_UNDOCK_FIRST.into()))?;
    Ok((system, pos))
}

fn distance(arg: Option<&String>, default: f64) -> Option<f64> {
    let value = match arg { Some(value) => value.parse::<f64>().ok()?, None => default };
    (value.is_finite() && value >= 0.0).then_some(value)
}

pub(super) async fn command(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    match command {
        "npc" | "entity" => spawn(c, command, args).await,
        "npcclear" => clear(c, args).await,
        "wreck" | "container" => debris(c, command, args).await,
        _ => unreachable!(),
    }
}

async fn spawn(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    let usage = || slash_error(messages::gm::GM_NPC_USAGE.into());
    let parsed = Args::new(args);
    let (kind, count, distance) = if command == "entity" {
        let [deploy, _, kind] = args else { return Err(usage()) };
        if deploy != "deploy" { return Err(usage()); }
        if parsed.i64(2).ok().and_then(|id| i32::try_from(id).ok()).is_none() { return Err(usage()); }
        (kind, parsed.i64(1).ok().and_then(|n| usize::try_from(n).ok()).ok_or_else(usage)?, crate::config::game().gm.spawn_distance_m)
    } else {
        if args.is_empty() || args.len() > 3 { return Err(usage()); }
        parsed.required(0).map_err(|_| usage())?;
        let count = if args.len() > 1 { parsed.i64(1).ok().and_then(|n| usize::try_from(n).ok()).ok_or_else(usage)? } else { 1 };
        (&args[0], count, distance(args.get(2), crate::config::game().gm.spawn_distance_m).ok_or_else(usage)?)
    };
    if count == 0 || count > crate::config::game().gm.max_spawn_count { return Err(usage()); }
    let group = if command == "npc" { kind.parse::<i32>().ok().map(|id| c.sde.types.group_types(id).iter().filter_map(|id| c.sde.types.get(*id)).filter(|t| t.category_id == Some(CATEGORY_ENTITY)).map(|t| t.type_id).collect::<Vec<_>>()).unwrap_or_default() } else { Vec::new() };
    let group = if group.is_empty() && command == "npc" {
        c.sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_ENTITY) && t.group_id.and_then(|id| c.sde.types.group(id)).and_then(|g| g.name.as_deref()).is_some_and(|name| name.eq_ignore_ascii_case(kind))).map(|t| t.type_id).collect::<Vec<_>>()
    } else { group };
    let types = if group.is_empty() {
        let (id, _) = parsed.type_id(c, if command == "entity" { 2 } else { 0 }).await?;
        if !c.sde.types.get(id).is_some_and(|t| t.category_id == Some(CATEGORY_ENTITY)) { return Err(usage()); }
        vec![id]
    } else { group };
    let (system, center) = position(c).await?;
    let corp = c.session.int("corpid").unwrap_or(0);
    let tick = now_ms();
    for _ in 0..count {
        let id = types[(super::super::rand01() * types.len() as f64) as usize];
        let direction = super::super::rand_dir();
        let pos = [0, 1, 2].map(|i| center[i] + direction[i] * distance);
        let data = c.sde.types.get(id).expect("resolved NPC type");
        let npc = super::super::combat::npc::spawn(&c.db, &c.world, system, data, corp, pos, tick, Some(crate::space::HOSTILE_TO_ALL)).await?;
        spawned().lock().unwrap().insert(npc);
    }
    Ok(messages::gm::gm_npc_spawned(count))
}

async fn clear(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    if args.len() > 1 { return Err(slash_error(messages::gm::GM_NPC_CLEAR_USAGE.into())); }
    let radius = if args.first().is_some_and(|a| a == "system") { None } else {
        Some(distance(args.first(), crate::config::game().gm.clear_radius_m).ok_or_else(|| slash_error(messages::gm::GM_NPC_CLEAR_USAGE.into()))?)
    };
    let (system, center) = position(c).await?;
    let ids: Vec<_> = spawned().lock().unwrap().iter().copied().collect();
    let mut removed = 0;
    for id in ids {
        let pos = c.world.with(system, move |s| s.sample_position(id)).await.flatten();
        if pos.is_some_and(|pos| radius.is_none_or(|r| dist(center, pos) <= r)) {
            if c.world.remove_ball(system, id).await.is_some() {
                super::super::combat::forget_gm_spawn(&c.world, id);
                spawned().lock().unwrap().remove(&id);
                removed += 1;
            }
        }
    }
    Ok(messages::gm::gm_npc_cleared(removed))
}

async fn debris(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    let usage = || slash_error(messages::gm::GM_DEBRIS_USAGE.into());
    if args.len() > 2 { return Err(usage()); }
    let group = if command == "wreck" { GROUP_WRECK } else { GROUP_CONTAINER };
    let type_id = match args.first() {
        Some(_) => {
            let parsed = Args::new(args);
            parsed.required(0).map_err(|_| usage())?;
            parsed.type_id(c, 0).await?.0
        }
        None => c.sde.types.group_types(group).iter().copied().min().ok_or_else(usage)?,
    };
    if !c.sde.types.get(type_id).is_some_and(|t| t.group_id == Some(group)) { return Err(usage()); }
    let distance = distance(args.get(1), crate::config::game().gm.spawn_distance_m).ok_or_else(usage)?;
    let (system, center) = position(c).await?;
    let pos = [center[0] + distance, center[1], center[2]];
    let owner = c.session.int("charid").ok_or_else(usage)?;
    let corp = c.session.int("corpid").unwrap_or(0);
    let id = super::super::combat::groups_health::spawn_wreck(&c.db, &c.world, system, type_id, owner, corp, pos, None).await?;
    Ok(messages::gm::gm_debris_spawned(id))
}

#[cfg(test)] mod tests;
