//! Actor-owned asteroid metadata and durable belt initialization.
//! The client sees ordinary destiny balls through `michelle.py`; generation uses SDE
//! celestial and type data and `game.asteroids` / `game.asteroid_fields`. Mining
//! settlement remains database-authoritative; actor quantities are committed projections.
pub(crate) mod generation;
use generation::generation_inputs;
use super::*;
use crate::client::{filetime_now, unix_ms_from_filetime_ceil};
use infinity_core::consts::GROUP_ASTEROID_BELT;
use crate::dao::asteroids as dao;
use crate::db::executor::Executor;
use crate::services::{db_err, CallError};
use std::collections::{HashMap, HashSet};

/// `inventorycommon/const.py` wormhole-space upper bound (exclusive).
const WORMHOLE_SYSTEM_END: i64 = 32_000_000;
/// Inherited server fallback for a stored rock without a radius.
const STORED_RADIUS_FALLBACK: f32 = 2_000.0;
/// Inherited server rigid asteroid ball mass.
const ASTEROID_MASS: f64 = 1e34;

/// What a field is made of.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Field { Ore, Ice, Gas }

/// Frozen random draws for a single field.
pub(crate) struct GenerationInputs { rocks: Vec<generation::Candidate>, center: V3 }

#[derive(Clone)]
pub(super) struct Rock { field: i64, type_id: i32, quantity: i64 }

struct Entry { ball: Ball, rock: Rock }
struct Deadline { field: i64, at: i64, center: V3 }
/// An entire committed batch. IDs and positions are never published before COMMIT.
pub(crate) struct Prepared { entries: Vec<Entry>, deadlines: Vec<Deadline> }

fn ball(id: i64, type_id: i32, pos: V3, radius: f32) -> Ball {
    Ball { id, type_id, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: None,
        signature_radius: None, mode: MODE_RIGID, radius, mass: ASTEROID_MASS, max_vel: 0.0,
        agility: 1.0, speed_fraction: 0.0, pos, vel: [0.0; 3], old_pos: pos,
        old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0,
        follow_range: 0.0, free: false, global: false, massive: true, interactive: false,
        warp: WarpState::default(), last_ms: now_ms(), interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![] }
}
fn entry(row: dao::Asteroid) -> Entry {
    Entry { ball: ball(row.id, row.type_id, [row.x, row.y, row.z], row.radius.unwrap_or(STORED_RADIUS_FALLBACK)),
        rock: Rock { field: row.field, type_id: row.type_id, quantity: row.quantity } }
}
fn regrowth() -> i64 {
    crate::config::game().mining.belt_regrowth_secs.saturating_mul(FILETIME_SECOND)
}
fn center(sde: &Sde, field: i64) -> Option<V3> {
    let b = sde.map.celestial(field).filter(|b| b.group_id == Some(GROUP_ASTEROID_BELT as i64))?;
    Some([b.x?, b.y?, b.z?])
}
fn band(sde: &Sde, system: i64) -> Result<(f64, bool), CallError> {
    let row = sde.map.system(system as i32 as i64).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing solar system {system}")))?;
    Ok((row.security.unwrap_or(0.0), (infinity_core::consts::WORMHOLE_SYSTEM_START..WORMHOLE_SYSTEM_END).contains(&system)))
}
/// Freeze candidate generation for every belt before entering the transaction.
pub(super) fn cold_inputs(sde: &Sde, system: i64) -> Result<Vec<(i64, V3, GenerationInputs)>, CallError> {
    let belts: Vec<_> = sde.map.celestials(system).filter(|b| b.group_id == Some(GROUP_ASTEROID_BELT as i64)).collect();
    if belts.is_empty() { return Ok(Vec::new()); }
    let (security, jspace) = band(sde, system)?;
    Ok(belts.into_iter().map(|b| {
        let pos = [b.x.expect("belt x"), b.y.expect("belt y"), b.z.expect("belt z")];
        (b.item_id, pos, generation_inputs(sde, pos, Field::Ore, security, jspace))
    }).collect())
}

/// Persist one field atomically inside the caller's transaction. Existing populated
/// or initialized-empty fields are returned unchanged; only a due belt may regrow.
pub(crate) async fn prepare_field(ex: &(impl Executor + ?Sized), system: i64, field: i64,
    now: i64, inputs: &GenerationInputs, allow_due: bool) -> Result<Prepared, CallError> {
    let rows = dao::load(ex, system).await.map_err(db_err)?;
    let mut marker = dao::field(ex, field).await.map_err(db_err)?;
    if matches!(dao::field_status(marker.as_ref(), system, false), dao::FieldStatus::WrongSystem { .. }) {
        return Err(CallError::Internal(anyhow::anyhow!("asteroid field {field} belongs to another system")));
    }
    if marker.is_none() && rows.iter().any(|row| row.field == field) {
        if !dao::grown_batch(ex, system, &[field], now).await.map_err(db_err)? {
            return Err(CallError::Internal(anyhow::anyhow!("asteroid field {field} belongs to another system")));
        }
        marker = dao::field(ex, field).await.map_err(db_err)?;
    }
    let mut prepared = Prepared { entries: Vec::new(), deadlines: Vec::new() };
    let mut recovered = false;
    for row in rows.into_iter().filter(|row| row.field == field) {
        if row.quantity > 0 { prepared.entries.push(entry(row)); continue; }
        recovered = true;
        if let dao::Depletion::Deleted { deadline: Some(at), .. } =
            dao::deplete_scoped(ex, system, field, row.id, now, allow_due.then(regrowth)).await.map_err(db_err)? {
            prepared.deadlines.push(Deadline { field, at, center: inputs.center });
        }
    }
    if recovered {
        marker = dao::field(ex, field).await.map_err(db_err)?;
        if marker.is_none() {
            if !dao::grown_batch(ex, system, &[field], now).await.map_err(db_err)? {
                return Err(CallError::Internal(anyhow::anyhow!("asteroid field {field} belongs to another system")));
            }
            marker = dao::field(ex, field).await.map_err(db_err)?;
        }
    }
    let generate = match dao::field_status(marker.as_ref(), system, !prepared.entries.is_empty()) {
        dao::FieldStatus::NeverInitialized => true,
        dao::FieldStatus::Depleted { depleted_at } => allow_due && depleted_at.saturating_add(regrowth()) <= now,
        dao::FieldStatus::WrongSystem { .. } => return Err(CallError::Internal(anyhow::anyhow!("asteroid field {field} belongs to another system"))),
        dao::FieldStatus::InitializedEmpty | dao::FieldStatus::Populated => false,
    };
    if generate {
        if !dao::grown_batch(ex, system, &[field], now).await.map_err(db_err)? {
            return Err(CallError::Internal(anyhow::anyhow!("asteroid field {field} belongs to another system")));
        }
        let rocks: Vec<_> = inputs.rocks.iter().map(|candidate| dao::NewAsteroid {
            field, type_id: candidate.type_id, quantity: candidate.quantity, radius: candidate.radius, pos: candidate.pos,
        }).collect();
        prepared.entries.extend(dao::insert_batch(ex, system, &rocks).await.map_err(db_err)?.into_iter().map(entry));
    }
    Ok(prepared)
}
impl Prepared {
    pub(crate) fn ids(&self) -> Vec<i64> { self.entries.iter().map(|e| e.ball.id).collect() }
}

/// Load all durable rocks and markers, recover zero rows and prepare missing/due belts.
pub(super) async fn prepare(ex: &(impl Executor + ?Sized), sde: &Sde, system: i64,
    now: i64, inputs: &[(i64, V3, GenerationInputs)]) -> Result<Prepared, CallError> {
    let rows = dao::load(ex, system).await.map_err(db_err)?;
    let requested: Vec<_> = inputs.iter().map(|(field, _, _)| *field).chain(rows.iter().map(|row| row.field)).collect();
    let states: HashMap<_, _> = dao::fields(ex, system, &requested).await.map_err(db_err)?
        .into_iter().map(|state| (state.field, state)).collect();
    for field in &requested {
        if states.get(field).is_some_and(|state| state.system != system) {
            return Err(CallError::Internal(anyhow::anyhow!("asteroid field {field} belongs to another system")));
        }
    }
    let mut populated = HashSet::new();
    let mut markers = HashSet::new();
    let mut rocks = Vec::new();
    let mut prepared = Prepared { entries: Vec::new(), deadlines: Vec::new() };
    for row in &rows {
        if row.quantity > 0 && !states.contains_key(&row.field) { markers.insert(row.field); }
    }
    for row in rows {
        if row.quantity > 0 { populated.insert(row.field); prepared.entries.push(entry(row)); continue; }
        let belt = center(sde, row.field);
        let result = dao::deplete_scoped(ex, system, row.field, row.id, now, belt.map(|_| regrowth())).await.map_err(db_err)?;
        if let (Some(pos), dao::Depletion::Deleted { field, deadline: Some(at) }) = (belt, result) {
            prepared.deadlines.push(Deadline { field, at, center: pos });
        }
    }
    for (field, pos, candidates) in inputs {
        let has_rocks = populated.contains(field);
        let state = states.get(field);
        if has_rocks { continue; }
        let depleted = state.and_then(|s| s.depleted_at);
        if let Some(at) = depleted.map(|t| t.saturating_add(regrowth())) {
            if at > now { prepared.deadlines.push(Deadline { field: *field, at, center: *pos }); continue; }
        }
        // A zero row deleted above may have just installed a deadline; never regrow it immediately.
        if prepared.deadlines.iter().any(|d| d.field == *field && d.at > now) { continue; }
        if state.is_some() && depleted.is_none() { continue; }
        markers.insert(*field);
        rocks.extend(candidates.rocks.iter().map(|rock| dao::NewAsteroid {
            field: *field, type_id: rock.type_id, quantity: rock.quantity, radius: rock.radius, pos: rock.pos,
        }));
    }
    let marker_fields: Vec<_> = markers.into_iter().collect();
    if !dao::grown_batch(ex, system, &marker_fields, now).await.map_err(db_err)? {
        return Err(CallError::Internal(anyhow::anyhow!("asteroid field belongs to another system")));
    }
    prepared.entries.extend(dao::insert_batch(ex, system, &rocks).await.map_err(db_err)?.into_iter().map(entry));
    Ok(prepared)
}

impl System {
    pub(crate) fn rock_type(&self, id: i64) -> Option<i32> {
        let rock = self.rocks.get(&id)?;
        let ball = self.ball(id)?;
        (rock.quantity > 0 && ball.cloak == 0 && ball.mode == MODE_RIGID && ball.type_id == rock.type_id).then_some(rock.type_id)
    }
    pub(crate) fn rock_field(&self, id: i64) -> Option<i64> { self.rocks.get(&id).map(|r| r.field) }
    pub(crate) fn apply_mining_delivery(&mut self, id: i64, left: i64) {
        if let Some(rock) = self.rocks.get_mut(&id) { rock.quantity = rock.quantity.min(left); }
    }
    pub(crate) fn rocks_in_range(&self, ship: i64, range: f64) -> Option<Vec<(i64, i32, i64)>> {
        let position = self.sample_position(ship)?;
        Some(self.rocks.iter().filter_map(|(&id, rock)| {
            self.rock_type(id)?;
            (dist(position, self.sample_position(id)?) <= range).then_some((id, rock.type_id, rock.quantity))
        }).collect())
    }
    fn install_asteroids(&mut self, prepared: Prepared, live: bool) {
        for entry in prepared.entries {
            let id = entry.ball.id;
            self.asteroid_timers.remove(&entry.rock.field);
            if live && self.rocks.contains_key(&id) { continue; }
            if live { self.add_ship(entry.ball); } else { self.insert(entry.ball); }
            self.rocks.insert(id, entry.rock);
        }
        for deadline in prepared.deadlines { self.asteroid_timer(deadline.field, deadline.at, deadline.center); }
    }
    pub(super) fn install_cold_asteroids(&mut self, prepared: Prepared) { self.install_asteroids(prepared, false); }
    fn asteroid_timer(&mut self, field: i64, at: i64, center: V3) {
        if self.asteroid_timers.get(&field).is_some_and(|(old, _)| *old == at) { return; }
        self.asteroid_timer_sequence = self.asteroid_timer_sequence.checked_add(1).expect("asteroid timer sequence exhausted");
        let token = self.asteroid_timer_sequence;
        self.asteroid_timers.insert(field, (at, token));
        self.schedule_at(unix_ms_from_filetime_ceil(at), move |s| {
            if s.asteroid_timers.get(&field) != Some(&(at, token)) { return; }
            s.asteroid_timers.remove(&field);
            let Some(context) = s.snapshot_context.as_ref() else { return };
            let sender = context.tx.clone();
            let world = s.world.clone();
            let db = context.db.clone();
            let system = s.id;
            s.defer(async move {
                let Some(world) = world.upgrade() else { return };
                let result = regrow(&db, &world, system, field, center, sender).await;
                if let Err(error) = result { tracing::warn!(system, field, ?error, "asteroid regrowth failed"); }
            });
        });
    }
}

/// Weak identity captured before live work; publication cannot target a replacement actor.
#[derive(Clone)]
pub(crate) struct ActorIdentity(mpsc::WeakUnboundedSender<actor::Job>);
impl World {
    pub(crate) fn asteroid_actor(&self, system: i64) -> Option<ActorIdentity> {
        self.running(system).map(|h| ActorIdentity(h.tx.downgrade()))
    }
}
impl ActorIdentity {
    /// Capture cleanup identity from the same actor whose mailbox will receive publication.
    pub(crate) async fn rock_field(&self, id: i64) -> Option<i64> {
        let sender = self.0.upgrade()?;
        let (reply, receipt) = tokio::sync::oneshot::channel();
        sender.send(Box::new(move |s| { let _ = reply.send(s.rock_field(id)); })).ok()?;
        receipt.await.ok().flatten()
    }
}

async fn publish_action(world: &World, system: i64, actor: &ActorIdentity,
    action: impl FnOnce(&mut System) + Send + 'static) -> bool {
    let Some(sender) = actor.0.upgrade() else { return false };
    let (ack, receipt) = tokio::sync::oneshot::channel();
    {
        let systems = world.systems.lock().unwrap();
        if !systems.get(&system).is_some_and(|h| sender.same_channel(&h.tx)) { return false; }
        if sender.send(Box::new(move |s| s.asteroid_action(move |s| {
            action(s);
            let _ = ack.send(());
        }))).is_err() { return false; }
    }
    receipt.await.is_ok()
}
/// Enqueue one action and wait until the entire committed batch is applied.
pub(crate) async fn publish(world: &World, system: i64, actor: &ActorIdentity, prepared: Prepared) -> bool {
    publish_action(world, system, actor, move |s| s.install_asteroids(prepared, true)).await
}
/// Reconcile a committed scoped deletion with its exact originating actor.
pub(crate) async fn publish_depletion(world: &World, system: i64, actor: &ActorIdentity,
    id: i64, field: i64, deadline: Option<i64>, center: Option<V3>) -> bool {
    publish_action(world, system, actor, move |s| {
        if s.rock_field(id).is_some_and(|actual| actual != field) { return; }
        if s.rock_field(id) == Some(field) { s.rocks.remove(&id); s.remove_ball(id); }
        if let (Some(at), Some(pos)) = (deadline, center) { s.asteroid_timer(field, at, pos); }
    }).await
}
async fn regrow(db: &Db, world: &World, system: i64, field: i64, pos: V3,
    sender: mpsc::WeakUnboundedSender<actor::Job>) -> Result<(), CallError> {
    let (security, jspace) = band(world.sde(), system)?;
    let inputs = Arc::new(generation_inputs(world.sde(), pos, Field::Ore, security, jspace));
    let now = filetime_now();
    let _market = crate::services::market::lock().await;
    let prepared = db.transaction::<_, _, CallError, crate::db::outbox::Outbox<()>, _, _, _>(
        |tx, _| {
            let inputs = inputs.clone();
            Box::pin(async move { prepare_field(tx, system, field, now, &inputs, true).await })
        },
        |batch, _| async move { Ok(batch) },
    ).await?;
    let _ = publish(world, system, &ActorIdentity(sender), prepared).await;
    Ok(())
}

#[cfg(test)] mod tests;
