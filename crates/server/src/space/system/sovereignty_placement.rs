//! Captures actor-owned deployment geometry for the sovereignty Hub.
//! The deploy component consumes sampled positions of the five relevant SDE
//! inventory groups; unrelated balls do not participate in geometry validation.
//! Persistence, authorization and placement decisions remain outside the actor.
use super::*;
use crate::services::sovereignty::{PlacementObstacle, PlacementProof};
use infinity_core::consts::{GROUP_CONTROL_TOWER, GROUP_SOVEREIGNTY_HUB, GROUP_STARGATE, GROUP_STATION, GROUP_SUN};

impl System {
    pub(crate) fn sovereignty_placement(&self, ship: i64, sde: &Sde) -> Option<PlacementProof> {
        let ball = self.ball(ship)?;
        let character = ball.char_id?;
        if self.pilot_ball(character).is_none_or(|pilot| pilot.id != ship)
            || ball.owner_id != character || ball.cloak != 0 || ball.mode == MODE_WARP
            || self.combat_phase(ship) != Some(combat::CombatPhase::Live)
            || !ball.health[2].is_finite() || ball.health[2] <= 0.0
            || !ball.radius.is_finite() || ball.radius < 0.0
            || !ball.vel.iter().all(|v| v.is_finite())
            || !sde.types.get(ball.type_id).is_some_and(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)) {
            return None;
        }
        let position = self.sample_position(ship)?;
        if !position.iter().all(|v| v.is_finite()) { return None; }
        let mut obstacles = Vec::new();
        for other in self.balls.values() {
            let Some(group_id) = sde.types.get(other.type_id).and_then(|ty| ty.group_id) else { continue };
            if !matches!(group_id, GROUP_SUN | GROUP_SOVEREIGNTY_HUB | GROUP_CONTROL_TOWER | GROUP_STARGATE) && i64::from(group_id) != GROUP_STATION { continue; }
            let position = self.sample_position(other.id)?;
            if !position.iter().all(|v| v.is_finite()) || !other.radius.is_finite() || other.radius < 0.0 {
                return None;
            }
            obstacles.push(PlacementObstacle { group_id, position });
        }
        Some(PlacementProof { ship_id: ship, character_id: character, position, obstacles })
    }
}

#[cfg(test)]
mod tests;
