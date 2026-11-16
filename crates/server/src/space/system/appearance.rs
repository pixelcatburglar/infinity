//! Appearance fences and slim publication for persistent hull appearance
//! committed outside the actor. A fence captures one live incarnation's
//! identity: the checkpoint gate, persistent membership generation and the
//! physical ball fields. A post-commit patch can then never land on a replaced
//! actor or a reinserted same-ID hull. Patches only change `Ball::slim` fields;
//! condition, position and combat state are never touched, and publication
//! uses the system's ordered scene channel at the current destiny tick.
//! Unpersisted balls and Transition or Dying membership have no valid fence.
use super::*;
use crate::space::combat::CombatPhase;

/// Identity of one live persistent hull incarnation, captured inside its actor.
/// `actor_gate` weakly pins the checkpoint gate Arc so publication proves the
/// actor itself was not replaced, not just that a generation number matches.
#[derive(Clone)]
pub(crate) struct Fence {
    pub system: i64,
    pub ship: i64,
    pub owner: i64,
    pub type_id: i32,
    pub pilot: Option<i64>,
    pub generation: u64,
    pub actor_gate: std::sync::Weak<tokio::sync::Mutex<()>>,
}

/// Slim fields changed by one committed appearance write. Absent fields keep
/// their current values.
#[derive(Clone, Copy)]
pub(crate) struct Patch {
    pub dirt_timestamp: Option<i64>,
    /// New player-kill count; `Some(0)` clears the displayed marks.
    pub player_kills: Option<i64>,
}

impl System {
    /// Capture the live persistent incarnation of `ship`, or `None` when the
    /// ball is absent, unpersisted or not in `CombatPhase::Live`.
    pub(crate) fn capture_appearance(&self, ship: i64) -> Option<Fence> {
        let ball = self.balls.get(&ship)?;
        let generation = self.persistent_generation(ship)?;
        if self.combat_phase(ship) != Some(CombatPhase::Live) { return None; }
        Some(Fence {
            system: self.id,
            ship,
            owner: ball.owner_id,
            type_id: ball.type_id,
            pilot: ball.char_id,
            generation,
            actor_gate: std::sync::Arc::downgrade(&self.snapshot_context.as_ref()?.gate),
        })
    }

    /// Apply one committed patch on exactly the captured incarnation.
    /// Returns false when the actor, membership or physical identity no longer
    /// matches; the database keeps the committed value and a later real prepare
    /// recovers. An unchanged value counts as published without a broadcast.
    pub(crate) fn publish_appearance(&mut self, fence: &Fence, patch: Patch) -> bool {
        let Some(gate) = fence.actor_gate.upgrade() else { return false };
        if fence.system != self.id
            || !self.snapshot_context.as_ref().is_some_and(|context| std::sync::Arc::ptr_eq(&context.gate, &gate))
            || self.persistent_generation(fence.ship) != Some(fence.generation)
            || self.combat_phase(fence.ship) != Some(CombatPhase::Live) {
            return false;
        }
        let Some(ball) = self.balls.get_mut(&fence.ship) else { return false };
        if ball.owner_id != fence.owner || ball.type_id != fence.type_id || ball.char_id != fence.pilot { return false; }
        let mut changed = false;
        if let Some(stamp) = patch.dirt_timestamp {
            let value = crate::services::ship::appearance::dirt_timestamp_value(stamp);
            match ball.slim.iter_mut().find(|(key, _)| *key == "dirtTime") {
                Some((_, existing)) if *existing == value => {}
                Some((_, existing)) => { *existing = value; changed = true; }
                None => { ball.slim.push(("dirtTime", value)); changed = true; }
            }
        }
        if let Some(count) = patch.player_kills {
            let value = Value::Int(count);
            match ball.slim.iter_mut().find(|(key, _)| *key == "kills") {
                Some((_, existing)) if *existing == value => {}
                Some((_, existing)) => { *existing = value; changed = true; }
                None => { ball.slim.push(("kills", value)); changed = true; }
            }
        }
        if !changed { return true; }
        let slim = ball.slim_item();
        self.publish_destiny(fence.ship, |stamp| vec![event(stamp, "OnSlimItemChange", vec![Value::Int(fence.ship), slim])]);
        true
    }
}
