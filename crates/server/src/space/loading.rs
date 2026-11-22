//! Cold loading of a solar system into a running actor.
//!
//! Builds the `System` state the client sees in its initial scene
//! (`eve/client/script/remote/michelle.py`): SDE celestials and stargate jump
//! rows, drone specs and persistent unpiloted hulls restored from game items,
//! and asteroid batches prepared inside the load transaction. Loading runs
//! once when a system starts; there is no second initialization path and no
//! refresh of a live system's state.
use super::*;
use marshal::{DbRowDescriptor, DbType};
use tracing::info;

impl World {
    /// Start the destination before removing a jumping ship from its source.
    /// Construction is serialized with the live-handle check.
    pub async fn system(&self, system_id: i64) -> Result<()> {
        let admission = self.admission(system_id).map_err(|error| anyhow::anyhow!(error))?;
        if self.running(system_id).is_some() { return Ok(()); }
        let market = crate::services::market::lock().await;
        admission.check().map_err(|error| anyhow::anyhow!(error))?;
        self.system_locked(system_id, &market).await
    }

    /// Load a system while the caller holds the existing market guard.
    pub(crate) async fn system_locked(&self, system_id: i64, _market: &crate::services::market::economy_lock::Guard) -> Result<()> {
        if self.running(system_id).is_some() { return Ok(()); }
        {
            let systems = crate::poison::lock_or_exit(self.systems.lock());
            match systems.get(&system_id) {
                Some(handle) if handle.state.failed() => return Err(containment::ActorRetiring.into()),
                Some(handle) if handle.is_closed() && !handle.is_stopping() => return Err(containment::ActorRetiring.into()),
                _ => {}
            }
        }
        let stopping = {
            let systems = crate::poison::lock_or_exit(self.systems.lock());
            systems.get(&system_id).filter(|h| h.is_closed() && h.is_stopping()).cloned()
        };
        if let Some(handle) = stopping { handle.wait_exited().await?; }
        self.load_bounty_batches(_market).await?;
        let bounty_rows = crate::dao::npc_bounties::in_system(&self.db, system_id).await?;
        let restored = crate::services::drones::inventory::restore_specs(&self.db, self.sde.clone(), system_id).await
            .map_err(|error| match error {
                crate::services::CallError::Internal(error) => error,
                crate::services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
            })?;
        let persistent_ships = self.restore_persistent_ships(system_id).await?;
        let (sovereignty, policies, indices) = sovereignty::cold(self, system_id).await?;
        let inputs = Arc::new(asteroids::cold_inputs(&self.sde, system_id).map_err(|error| match error {
            crate::services::CallError::Internal(error) => error,
            crate::services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
        })?);
        let now = crate::client::filetime_now();
        let asteroid_batch = self.db.transaction::<_, _, crate::services::CallError, crate::db::outbox::Outbox<()>, _, _, _>(
            |tx, _| {
                let inputs = inputs.clone();
                let sde = self.sde.clone();
                Box::pin(async move { asteroids::prepare(tx, &sde, system_id, now, &inputs).await })
            },
            |batch, _| async move { Ok(batch) },
        ).await.map_err(|error| match error {
            crate::services::CallError::Internal(error) => error,
            crate::services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
        })?;
        // Transient NPC wreck ID reservation: one committed block must be in
        // hand before the actor exists; an error or unknown commit burns the
        // whole block and fails this load rather than publish a partial pool.
        let npc_wreck_ids = self.db.transaction::<_, _, crate::services::CallError, crate::db::outbox::Outbox<()>, _, _, _>(
            |tx, _| {
                let block = crate::config::game().combat.npc_wreck_id_block_size;
                Box::pin(async move {
                    crate::dao::items::npc_loot::reserve_ids(tx, block).await.map_err(crate::services::CallError::from)
                })
            },
            |ids, _| async move { Ok::<Vec<i64>, crate::services::CallError>(ids) },
        ).await.map_err(|error| match error {
            crate::services::CallError::Internal(error) => error,
            crate::services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
        })?;
        let mut systems = crate::poison::lock_or_exit(self.systems.lock());
        match systems.get(&system_id) {
            Some(handle) if !handle.is_closed() => return Ok(()),
            // A failed tombstone, or a closed survivor without an explicit stop
            // in flight, is refused rather than silently replaced.
            Some(handle) if handle.state.failed() || !handle.is_stopping() => return Err(containment::ActorRetiring.into()),
            // An explicit-stop survivor its supervisor has not unregistered
            // yet; it removes its own handle, so loading reports busy instead.
            Some(_) => anyhow::bail!("space actor stop in progress"),
            None => {}
        }
        let tick = now_ms();
        let mut system = System::new(system_id, self.pilots.clone(), tick);
        system.progress_registry = self.progress_registry.clone();
        system.world = self.weak.clone();
        system.projection_sde = Some(self.sde.clone());
        system.notice_hub = Some(self.notice_hub.clone());
        system.npc_wreck_install_ids(npc_wreck_ids);
        system.restore_bounties(&bounty_rows);
        for r in self.sde.map.celestials(system_id) {
            let Some(type_id) = r.type_id.and_then(|id| i32::try_from(id).ok()) else { continue };
            if self.sde.types.get(type_id).is_none() { continue; }
            let id = r.item_id;
            let pos: V3 = [r.x.expect("NULL celestial x"), r.y.expect("NULL celestial y"), r.z.expect("NULL celestial z")];
            let group = r.group_id.unwrap_or(0) as i32;
            let category = r.group_id.and_then(|id| i32::try_from(id).ok())
                .and_then(|id| self.sde.types.group(id)).and_then(|g| g.category_id).unwrap_or(0);
            let radius = self.sde.celestial_radius(r);
            let owner = self.sde.map.station(id).and_then(|s| s.owner_id).unwrap_or(1);
            if let Some(point) = warpin::warp_in_point(id, category, group, pos, f64::from(radius)) {
                system.set_warp_in(id, point);
            }
            let mut slim = Vec::new();
            let gate = self.sde.map.stargate(id);
            if let (Some(dest_gate), Some(dest_sys)) = (gate.and_then(|g| g.dest_stargate_id), gate.and_then(|g| g.dest_solar_system_id)) {
                // A row: unpacked as (destGate, destSystem) by subwaySvc.GetPriorityLoads,
                // read as .toCelestialID / .locationID elsewhere.
                let jump = DbRowDescriptor::new([("toCelestialID", DbType::I4), ("locationID", DbType::I4)])
                    .row(&[Value::Int(dest_gate), Value::Int(dest_sys)]);
                slim.push(("jumps", Value::list([jump])));
                // clientControllerRegistry.StargateSetup indexes these (stage, maximum) pairs.
                let pair = || Value::tuple([Value::Int(0), Value::Int(1)]);
                slim.push(("destinationCorruptionStageAndMaximum", pair()));
                slim.push(("localCorruptionStageAndMaximum", pair()));
                slim.push(("destinationSuppressionStageAndMaximum", pair()));
                slim.push(("localSuppressionStageAndMaximum", pair()));
            }
            system.insert(
                Ball {
                    id,
                    type_id,
                    owner_id: owner,
                    corp_id: owner,
                    alliance_id: -1,
                    char_id: None,
                    signature_radius: None,
                    mode: MODE_RIGID,
                    radius,
                    mass: CELESTIAL_STATIC_MASS,
                    max_vel: 0.0,
                    agility: 1.0,
                    speed_fraction: 0.0,
                    pos,
                    vel: V3::default(),
                    old_pos: pos,
                    old_vel: V3::default(),
                    segment_acceleration: None,
                    goto: V3::default(),
                    follow_id: 0,
                    follow_range: 0.0,
                    free: false,
                    global: true,
                    massive: true,
                    interactive: false,
                    warp: WarpState::default(),
                    last_ms: tick,
                    interpolation_anchor_ms: tick,
                    effect_stamp: 0,
                    cloak: 0,
                    cloak_until: 0,
                    health: [1.0; 3],
                    launcher: 0,
                    slim,
                },
            );
        }
        sovereignty::initialize(&mut system, sovereignty, policies, self.sovereignty_ready.clone(), indices)?;
        let effects = crate::services::drones::worker::effects(self.db.clone(), self.weak.clone(), system_id);
        for prepared in restored { system.restore_drone(prepared, effects.clone()); }
        for ship in persistent_ships {
            if !system.contains(ship.ball.id) { ship.install(&mut system, |s, ball| s.add_persistent_ship(ball)); }
        }
        info!("loaded solar system {system_id} with {} celestials", system.balls().count());
        system.install_cold_asteroids(asteroid_batch);
        // A sleeping NPC cohort is verified and installed before the first
        // scene publication: the entry is only consumed after every check
        // passes, and a refusal keeps both the payload and the load failed.
        {
            let mut sleeping = crate::poison::lock_or_exit(self.sleeping_npcs.lock());
            if let Some(payload) = sleeping.get(&system_id) {
                if let Some(reason) = system.sleeping_npcs_install_blocked(payload) {
                    return Err(anyhow::anyhow!("sleeping NPCs restore blocked: {reason}"));
                }
                let payload = sleeping.remove(&system_id).expect("verified payload");
                let survivors = payload.survivors.len();
                system.install_sleeping_npcs(payload);
                info!("restored {survivors} sleeping NPCs in {system_id}");
            }
        }
        // Every restored ball lists at once; dynamic inserts queue births.
        system.commit_cold_scene();

        let handle = actor::start(system, system_id, Arc::downgrade(&self.systems), self.db.clone());
        systems.insert(system_id, handle);
        Ok(())
    }
}
