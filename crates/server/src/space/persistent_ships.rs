//! Persistent unpiloted ship membership and system restoration. Destiny state is
//! consumed by `eve/client/script/remote/michelle.py`; hulls and their fittings
//! come from game items and the SDE. Condition is installed only for a published
//! hull, so duplicate restoration cannot replace live state. Boarding is separate.
//! A restored stance-capable hull missing its state is initialized in one
//! hull-locked transaction before its ball is prepared; drift or a malformed
//! state aborts the load with the ship id instead of a silent skip.
use super::{Ball, System, World, MILLISECONDS_PER_SECOND};
use infinity_core::consts::CATEGORY_SHIP;

impl System {
    /// Add an unpiloted hull once, sharing the position snapshot identity map.
    pub(crate) fn add_persistent_ship(&mut self, ball: Ball) {
        if self.contains(ball.id) { return; }
        let id = ball.id;
        let pos = ball.pos;
        self.add_ship(ball);
        self.snapshot_generation = self.snapshot_generation.checked_add(1).expect("snapshot generation exhausted");
        self.snapshot_positions.insert(id, (self.snapshot_generation, pos));
        if self.snapshot_due == 0 {
            self.snapshot_due = self.ms.saturating_add(crate::config::game().space.position_snapshot_secs.saturating_mul(MILLISECONDS_PER_SECOND));
        }
    }
}

impl World {
    pub(super) async fn restore_persistent_ships(&self, system: i64) -> anyhow::Result<Vec<crate::services::ship::balls::PreparedShip>> {
        let rows = crate::dao::items::space_ships::unpiloted(&self.db, system).await?;
        let mut balls = Vec::new();
        for row in rows {
            if self.sde.types.get(row.type_id).and_then(|t| t.category_id) != Some(CATEGORY_SHIP) { continue; }
            // Only stance-capable hulls pay the initialization transaction;
            // every other ship type skips it through the in-memory table.
            if crate::sde::ship_stances::for_hull(row.type_id).is_some() {
                let sde = self.sde.clone();
                let world = self;
                self.db.transaction(
                    move |tx, outbox: &mut crate::db::outbox::Outbox| {
                        let sde = sde.clone();
                        Box::pin(async move {
                            // The caller already holds the market lifecycle
                            // guard; only the hull row lock is taken here.
                            if !crate::dao::items::ship::lock_reload_ship(tx, row.item_id)
                                .await.map_err(crate::services::db_err)?
                            {
                                return Err(crate::services::CallError::Internal(
                                    anyhow::anyhow!("persistent ship {} hull row missing", row.item_id)));
                            }
                            let hull = crate::services::items::load_items(tx, &sde,
                                    crate::dao::items::loading::ItemQuery::Id(row.item_id)).await
                                .map_err(crate::services::db_err)?.into_iter().next()
                                .ok_or_else(|| crate::services::CallError::Internal(
                                    anyhow::anyhow!("persistent ship {} hull row missing", row.item_id)))?;
                            if hull.owner_id != row.owner_id || hull.type_id != row.type_id
                                || hull.location_id != system || hull.flag_id != 0
                                || !hull.singleton || hull.quantity != 1
                            {
                                return Err(crate::services::CallError::Internal(
                                    anyhow::anyhow!("persistent ship {} hull drifted from the unpiloted row", row.item_id)));
                            }
                            crate::services::ship_stance::state::ensure_initialized_tx(tx, &sde, outbox, &hull).await
                        })
                    },
                    |_, outbox| async move { crate::db::outbox::dispatch_world((), outbox, world) },
                ).await.map_err(|error| match error {
                    crate::services::CallError::Internal(error) => anyhow::anyhow!("persistent ship {}: {error}", row.item_id),
                    crate::services::CallError::User { msg, .. } => anyhow::anyhow!("persistent ship {}: {msg}", row.item_id),
                })?;
            }
            let ball = crate::services::ship::balls::unpiloted(&self.db, self.sde.clone(), &row).await
                .map_err(|error| match error {
                    crate::services::CallError::Internal(error) => error,
                    crate::services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
                })?;
            balls.push(ball);
        }
        Ok(balls)
    }

    pub(crate) async fn add_persistent_ship_locked(&self, system: i64, ship: crate::services::ship::balls::PreparedShip,
        market: &crate::services::market::economy_lock::Guard) -> anyhow::Result<()> {
        self.system_locked(system, market).await?;
        let installed = self.with(system, move |s| {
            if s.contains(ship.ball.id) { return false; }
            ship.install(s, |s, ball| s.add_persistent_ship(ball));
            true
        }).await.ok_or_else(|| anyhow::anyhow!("system actor stopped"))?;
        if !installed { return Err(anyhow::anyhow!("ship already present in system")); }
        Ok(())
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
