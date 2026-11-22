//! Transient NPC wreck and persistent GM container balls, with the existing
//! NPC loot selection. The client reads wreck contents through invCache and
//! the slim item (`isEmpty`, and `launcherID` when a source ship is linked).
//! Types, prices and radii come from the startup SDE snapshot.
//! Natural kills never persist the wreck or its loot: the ordinary draw is
//! frozen once into `PreparedNpcWreck` inside the death-context install, bound
//! to reserved item IDs before any death transaction, and published by the
//! system actor's transient `npc_wrecks` state at finalize. GM `/container`
//! stays a normal persistent item.

use super::*;
use crate::lua::events::GameEvent;
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_MODULE};

/// Existing server policy: wreck loot candidates are priced from the loot minimum up to
/// the bounty, but never below this ceiling, so a tiny bounty still samples a modest range.
const NPC_LOOT_MINIMUM_CEILING: f64 = 50_000.0;
/// Existing server policy: a wreck type with no SDE radius uses this ball radius.
pub(crate) const DEBRIS_DEFAULT_RADIUS: f32 = 100.0;
/// Existing server policy: every debris ball uses this mass.
const DEBRIS_MASS: f64 = 10_000.0;
/// Existing server policy: loot draws this many stacks, plus a roll across this width.
const LOOT_PICKS_BASE: i64 = 1;
const LOOT_PICKS_WIDTH: f64 = 2.0;
/// Existing server policy: a charge stack draws this quantity, plus a roll across this width.
const LOOT_AMMO_BASE: i64 = 20;
const LOOT_AMMO_WIDTH: f64 = 100.0;

/// One frozen ordinary-loot draft for a dying NPC. The wreck mapping, owner,
/// sampled position and stack contents are decided once inside the first
/// death-context actor install and are reused by every retry; `ids` binds the
/// reserved root+stack IDs at most once and never re-draws them.
pub(crate) struct PreparedNpcWreck {
    pub(crate) wreck_type: i32,
    pub(crate) owner: i64,
    pub(crate) corp: i64,
    pub(crate) pos: [f64; 3],
    pub(crate) radius: f32,
    pub(crate) source_ship: i64,
    pub(crate) loot: Vec<(i32, i64)>,
    pub(crate) ids: std::sync::OnceLock<Vec<i64>>,
}

/// Freeze the ordinary wreck draw for one dying NPC inside the death-context
/// actor install: wreck mapping first (a missing mapping is a defined `None`,
/// not an unprepared draft), then the original SDE/filter/picks/shuffle/
/// quantity order with the death ball's already sampled position. The killer
/// in the frozen death event keeps the original owner choice.
pub(crate) fn prepare_npc_wreck(sde: &Sde, event: &GameEvent, ball: &Ball) -> Option<PreparedNpcWreck> {
    let npc_type = sde.types.get(ball.type_id)?;
    let wreck_type = npc_type.group_id.and_then(|group| sde.wrecks.get(group, npc_type.race_id.unwrap_or(0)))?;
    let owner = match event {
        GameEvent::NpcDestroyed { killer_id, .. } => *killer_id,
        _ => None,
    }.unwrap_or(ball.owner_id);
    let bounty = attr(sde, ball.type_id, ATTR_ENTITY_KILL_BOUNTY, 0.0);
    let loot = draw_loot(sde, Some(bounty));
    let radius = sde.types.get(wreck_type).and_then(|t| t.radius).unwrap_or(f64::from(DEBRIS_DEFAULT_RADIUS)) as f32;
    Some(PreparedNpcWreck {
        wreck_type, owner, corp: ball.corp_id, pos: ball.pos, radius,
        source_ship: ball.id, loot, ids: std::sync::OnceLock::new(),
    })
}

/// The original ordinary-loot sampler, unchanged: one or two module or ammo
/// stacks that traded on Serenity, worth up to the bounty ceiling.
pub(crate) fn draw_loot(sde: &Sde, bounty: Option<f64>) -> Vec<(i32, i64)> {
    let picks = LOOT_PICKS_BASE + (rand01() * LOOT_PICKS_WIDTH) as i64;
    let mut loot: Vec<_> = bounty.into_iter()
        .flat_map(|bounty| sde.prices.average_between(crate::sde::prices::LOOT_MINIMUM_AVERAGE, loot_ceiling(bounty)))
        .filter_map(|row| sde.types.get(row.type_id).and_then(|t| loot_category(t.published, t.category_id).map(|category| (row.type_id, category))))
        .collect();
    for i in 0..(picks as usize).min(loot.len()) {
        let j = i + (rand01() * (loot.len() - i) as f64) as usize;
        loot.swap(i, j);
    }
    loot.truncate(picks as usize);
    loot.into_iter().map(|(type_id, category)| {
        let quantity: i64 = if category == CATEGORY_CHARGE { LOOT_AMMO_BASE + (rand01() * LOOT_AMMO_WIDTH) as i64 } else { 1 };
        (type_id, quantity)
    }).collect()
}

/// Create a transient actor-owned wreck, linking a destroyed source ship when
/// supplied, with the normal NPC loot selection. The database handle stays
/// unused: reserved actor IDs supply root and loot, and nothing is persisted.
/// The signature is unchanged for the remaining callers.
pub(crate) async fn spawn_wreck(_db: &Db, world: &World, system: i64, wreck_type: i32, owner: i64, corp: i64, pos: crate::space::V3, source_ship: Option<i64>, bounty: Option<f64>) -> Result<i64, CallError> {
    world.system(system).await.map_err(CallError::Internal)?;
    let sde = world.sde();
    let radius = sde.types.get(wreck_type).and_then(|t| t.radius).unwrap_or(f64::from(DEBRIS_DEFAULT_RADIUS)) as f32;
    let loot = draw_loot(sde, bounty);
    let stacks = loot.len();
    let wreck_id = world.with(system, move |s|
        s.npc_wreck_spawn(wreck_type, owner, corp, pos, radius, source_ship, loot)
    ).await.flatten()
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("npc wreck actor or reserved ids unavailable")))?;
    debug!("transient wreck {wreck_id} ({wreck_type}) with {stacks} loot stacks");
    Ok(wreck_id)
}

/// Rigid debris ball. `radius` is the SDE radius already resolved by the caller;
/// `at_ms` is the clock reading taken when the ball is built.
pub(crate) fn debris_ball(
    id: i64, type_id: i32, owner: i64, corp: i64,
    pos: crate::space::V3, radius: f32,
    source_ship: Option<i64>, empty: bool, at_ms: i64,
) -> Ball {
    let mut slim = Vec::with_capacity(1 + usize::from(source_ship.is_some()));
    slim.push(("isEmpty", Value::Bool(empty)));
    if let Some(source_ship) = source_ship { slim.push(("launcherID", Value::Int(source_ship))); }
    Ball {
        id,
        type_id,
        owner_id: owner,
        corp_id: corp,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: crate::space::MODE_RIGID,
        radius,
        mass: DEBRIS_MASS,
        max_vel: 0.0,
        agility: 1.0,
        speed_fraction: 0.0,
        pos,
        vel: [0.0; 3],
        old_pos: pos, old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: false,
        global: false,
        massive: true,
        interactive: false,
        warp: crate::space::WarpState::default(),
        last_ms: at_ms,
        interpolation_anchor_ms: 0,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim,
    }
}

/// Upper price bound of one NPC's loot candidates. A NaN static bounty stays NaN: prices
/// keep PostgreSQL float ordering, where NaN sorts above every number, so the range is open-ended.
fn loot_ceiling(bounty: f64) -> f64 {
    if bounty.is_nan() { bounty } else { bounty.max(NPC_LOOT_MINIMUM_CEILING) }
}

fn loot_category(published: Option<bool>, category: Option<i32>) -> Option<i32> {
    category.filter(|&id| published == Some(true) && (id == CATEGORY_MODULE || id == CATEGORY_CHARGE))
}

#[cfg(all(test, feature = "tests-combat"))]
#[path = "loot_tests.rs"]
mod loot_tests;
