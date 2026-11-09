//! Shared dispatch for actor-owned in-space effect sources.
use std::sync::Arc;
use crate::db::Db;
use crate::services::{CallError, fit::{Fit, Key}};
use crate::space::{Ball, System, World, drone::{DroneOrder, DronePhase, DroneRecord}};
use super::{EffectKind, registry::Effect};

/// An admitted native attempt remains owned even if an async reply is dropped.
pub(crate) struct PreparedCombatAttempt {
    facts: crate::space::combat_facts::CombatSource,
    world: std::sync::Weak<World>,
    system: i64,
    armed: bool,
    activation: Option<(i64, Arc<std::sync::atomic::AtomicBool>)>,
}

impl PreparedCombatAttempt {
    pub(crate) fn new(facts: crate::space::combat_facts::CombatSource, world: std::sync::Weak<World>, system: i64) -> Self {
        Self { facts, world, system, armed: true, activation: None }
    }

    pub(crate) fn facts(&self) -> crate::space::combat_facts::CombatSource { self.facts }

    pub(crate) fn with_activation(mut self, master: i64, generation: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.activation = Some((master, generation));
        self
    }

    /// Notify the frozen pilot and stop only the activation that launched this attempt.
    pub(crate) fn report_capacity_refusal(&self) {
        let Some(world) = self.world.upgrade() else { return };
        let character = self.facts.actual_actor_character;
        let ship = self.facts.context.ship_id;
        let activation = self.activation.clone();
        world.post(self.system, move |s| {
            if let Some(character) = character {
                s.notify_pilot(character, "OnRemoteMessage", vec![
                    marshal::Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY),
                    marshal::Value::dict([("notify", marshal::Value::unicode(infinity_core::messages::effects::COMBAT_CAPACITY_FULL))]),
                ]);
            }
            if let (Some(ship), Some((master, generation))) = (ship, activation) {
                if super::lifecycle::force_inactive(master, &generation) {
                    s.fitted_module_active(ship, master, false);
                    s.refresh_projection_target(ship);
                }
            }
        });
    }

    /// Disarm only after the actor has finished this attempt.
    pub(crate) fn disarm(&mut self) { self.armed = false; }
}

impl Drop for PreparedCombatAttempt {
    fn drop(&mut self) {
        if !self.armed { return; }
        if let Some(world) = self.world.upgrade() {
            let facts = self.facts;
            world.post(self.system, move |s| s.finish_combat_attempt(&facts));
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SourceGuard {
    Npc { source: i64, target: i64, generation: u64 },
    Drone { source: i64, target: i64, generation: u64 },
}

impl SourceGuard {
    pub(crate) fn valid(self, s: &System) -> bool {
        match self {
            Self::Npc { source, target, generation } => s.npc_projection_valid(source, target, generation),
            Self::Drone { source, target, generation } => {
                if !s.drone_valid(source, target, generation) { return false; }
                let Some(drone) = s.drone(source) else { return false };
                match drone.order {
                    DroneOrder::Assist(delegate) | DroneOrder::Guard(delegate) =>
                        crate::services::fleets::same_fleet(drone.owner, delegate)
                        && s.pilot_ball(delegate).is_some_and(|ball| {
                            let Some(sde) = s.sde() else { return false };
                            let capsule_group = sde.types.get(infinity_core::consts::TYPE_CAPSULE).and_then(|ty| ty.group_id);
                            sde.types.get(ball.type_id).is_some_and(|ty|
                                ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
                                && ty.group_id.is_some() && capsule_group.is_some() && ty.group_id != capsule_group)
                                && ball.mode != crate::space::MODE_WARP && ball.cloak == 0
                        }),
                    _ => true,
                }
            }
        }
    }
}

pub(crate) struct SourceContext<'a> {
    pub db: &'a Db,
    pub world: &'a Arc<World>,
    pub system: i64,
    pub source: &'a Ball,
    pub target: &'a Ball,
    pub module: i64,
    pub effect: &'a Effect,
    pub duration_ms: f64,
    pub fit: &'a Fit,
    pub key: Key,
    pub guard: SourceGuard,
    pub attacker: Option<i64>,
    pub drone: Option<&'a DroneRecord>,
    pub phase: DronePhase,
}

pub(crate) async fn apply(cx: &SourceContext<'_>, attempt: Option<PreparedCombatAttempt>) -> Result<(), CallError> {
    match cx.effect.kind {
        EffectKind::Turret | EffectKind::Missile => super::weapons::fire_source(cx, attempt).await,
        EffectKind::Projected { .. } => super::projected::apply_source(cx, attempt).await,
        EffectKind::Ecm => super::ecm::apply_source(cx).await,
        EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapTransfer => super::npc_capacitor::apply_source(cx).await,
        EffectKind::RemoteRepair { .. } => super::npc_repair::apply_source(cx, attempt).await,
        EffectKind::Mining if matches!(cx.phase, DronePhase::End) && cx.drone.is_some() =>
            crate::services::drones::mining::finish_cycle(cx.db, cx.world, cx.system, cx.drone.unwrap().clone(), cx.target.id, cx.fit).await,
        EffectKind::Mining if matches!(cx.phase, DronePhase::Begin) && cx.drone.is_some() => Ok(()),
        EffectKind::Smartbomb if matches!(cx.guard, SourceGuard::Npc { .. }) => {
            let SourceGuard::Npc { generation, .. } = cx.guard else { unreachable!() };
            let attempt = attempt.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing admitted combat attempt")))?;
            super::aoe::apply_npc(cx.db, cx.world, cx.system, cx.module, cx.source, cx.target, cx.effect, cx.duration_ms, generation, attempt).await
        }
        _ => {
            let id = cx.effect.id as i32;
            let name = cx.world.sde().dogma.effect(id).and_then(|row| row.name.as_deref()).unwrap_or("unknown");
            Err(super::unsupported::refusal(Some(id), name, cx.source.type_id))
        }
    }
}

#[cfg(test)] mod tests;
