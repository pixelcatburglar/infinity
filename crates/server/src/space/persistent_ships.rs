//! Persistent unpiloted ship membership and system restoration. Destiny state is
//! consumed by `eve/client/script/remote/michelle.py`; hulls and their fittings
//! come from game items and the SDE. Condition is installed only for a published
//! hull, so duplicate restoration cannot replace live state. Boarding is separate.
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
        market: &tokio::sync::MutexGuard<'static, ()>) -> anyhow::Result<()> {
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

#[cfg(test)] mod tests;
