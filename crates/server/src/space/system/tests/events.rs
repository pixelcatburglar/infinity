//! Test-only access to collision ticks, raw warp callbacks and segment samples.

use crate::space::{Ball, System, V3, MODE_GOTO, MODE_WARP};
use crate::space::system::warp::WarpIntent;

pub(crate) fn tick_with_collisions(system: &mut System, ms: i64, collisions: bool) {
    system.tick_with_collisions(ms, collisions);
}

pub(crate) fn install_progress_registry(system: &mut System, db: &crate::db::Db) {
    system.progress_registry = db.progress_registry.clone();
}

impl System {
    /// Queue cancellation of any previous landing intent before a new warp request.
    pub(in crate::space::system) fn clear_warp_exit(&mut self, id: i64) {
        self.actions.push_back((self.current_time, Box::new(move |s, _| { s.detach_warp_arrival(id); })));
    }

    /// Attach a raw native warp exit callback at the next boundary.
    pub(crate) fn on_warp_exit(&mut self, id: i64, f: impl FnOnce(&mut System) + Send + 'static) {
        let generation = (self.persistent_generation(id), self.combat_membership_generation(id));
        self.actions.push_back((self.current_time, Box::new(move |s, _| {
            if (s.persistent_generation(id), s.combat_membership_generation(id)) != generation { return; }
            match s.balls.get(&id).map(|b| b.mode) {
                Some(MODE_WARP) => {
                    s.detach_warp_arrival(id);
                    if s.warp_exits.get(&id).is_some_and(|intent| !intent.full) { return; }
                    let arrival = s.register_warp_arrival(Box::new(f));
                    if let Some(intent) = s.warp_exits.get_mut(&id) {
                        intent.arrival = Some(arrival);
                    } else {
                        s.warp_exits.insert(id, WarpIntent { generation, ordinary: false, pending: false, full: true, arrival: Some(arrival) });
                    }
                }
                Some(MODE_GOTO) => f(s),
                _ => {}
            }
        })));
    }
}

pub(crate) fn completed_segment_samples(ball: &Ball) -> [(f64, V3, V3); 5] {
    [(0.0, 0), (0.1, 100), (0.5, 500), (0.9, 900), (1.0, 1000)].map(|(fraction, offset)| {
        let time = ball.interpolation_anchor_ms.checked_add(offset).expect("segment sample time overflow");
        let (pos, vel) = ball.state_at(time);
        (fraction, pos, vel)
    })
}

pub(crate) fn encoded_ball_for_test(ball: &Ball, collisions: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    ball.write(&mut bytes, collisions);
    bytes
}

pub(crate) fn fitted_capacitor_curve_for_capture(system: &System, ship: i64) -> Option<(u64, f64, f64)> {
    use infinity_core::consts::dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE};
    use crate::services::fit::Key;
    let ball = system.ball(ship)?;
    let character = ball.char_id?;
    if ball.owner_id != character || system.pilot_ball(character).map(|b| b.id) != Some(ship) { return None; }
    let record = system.actor_combat.get(&ship)?;
    if record.phase != crate::space::combat::CombatPhase::Live || record.fit_key != Key::Ship
        || system.persistent_generation(ship) != Some(record.membership_generation) {
        return None;
    }
    Some((record.membership_generation, record.fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY),
        record.fit.value(Key::Ship, ATTR_RECHARGE_RATE)))
}
