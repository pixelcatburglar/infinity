//! Test-only access to collision ticks, raw warp callbacks and segment samples.

#[cfg(any(feature = "tests-lua", feature = "tests-runtime", feature = "tests-ships", feature = "tests-slash", feature = "tests-space"))] use crate::space::System;
#[cfg(any(feature = "tests-ships", feature = "tests-social"))] use crate::space::Ball;
#[cfg(feature = "tests-ships")] use crate::space::V3;
#[cfg(any(feature = "tests-space", feature = "tests-lua"))] use crate::space::{MODE_GOTO, MODE_WARP};
#[cfg(any(feature = "tests-space", feature = "tests-lua"))] use crate::space::system::warp::WarpIntent;

#[cfg(any(feature = "tests-combat", feature = "tests-effects", feature = "tests-lua", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
pub(crate) fn initial_state(rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::space::SceneDelivery>) -> marshal::Value {
    let crate::space::SceneDelivery::Destiny(mut batch) = rx.try_recv().expect("initial Destiny batch") else { panic!("initial state must be a Destiny batch") };
    assert_eq!(batch.len(), 1);
    let state = batch.pop().unwrap();
    assert_eq!(super::name(&state), b"SetState");
    state
}

/// Unwrap one expected Destiny batch; a scene Notify must fail loudly rather
/// than be mistaken for a native event batch.
#[cfg(any(feature = "tests-combat", feature = "tests-effects", feature = "tests-lua", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
pub(crate) fn destiny(delivery: crate::space::SceneDelivery) -> Vec<marshal::Value> {
    let crate::space::SceneDelivery::Destiny(batch) = delivery else { panic!("expected Destiny batch") };
    batch
}

/// Bound-check every actual damage shield sample and copy only that time into `expected`; accepts the AddBalls event or its damage Dict.
#[cfg(any(feature = "tests-space", feature = "tests-social"))]
pub(crate) fn reuse_checked_damage_time(actual: &marshal::Value, expected: &mut marshal::Value, bounds: std::ops::RangeInclusive<i64>) {
    use marshal::Value;
    let tuple: fn(&mut Value) -> &mut [Value] = |value| match value { Value::Tuple(items) => items, _ => panic!("expected tuple") };
    let actual = if matches!(actual, Value::Dict(_)) { actual } else { &actual.items().unwrap()[1].items().unwrap()[1].items().unwrap()[0].items().unwrap()[2] };
    let expected = if matches!(expected, Value::Dict(_)) { expected } else { &mut tuple(&mut tuple(&mut tuple(&mut tuple(expected)[1])[1])[0])[2] };
    let (Value::Dict(actual), Value::Dict(expected)) = (actual, expected) else { panic!("expected damage dictionaries") };
    for (id, state) in actual {
        let time = state.items().unwrap()[0].items().unwrap()[2].as_int().unwrap();
        assert!(bounds.contains(&time));
        let (_, expected) = expected.iter_mut().find(|(key, _)| key == id).expect("missing damage entry");
        tuple(&mut tuple(expected)[0])[2] = Value::Int(time);
    }
}

/// Consume the pending `OnFleetStateChange` snapshot at a defined scene
/// boundary (initial, uncloak or removal replay) and return its observed args.
/// `None` when the producer sent none: fixtures without a resolvable pilot
/// receive no snapshot, so callers must tolerate its absence without guessing.
#[cfg(any(feature = "tests-combat", feature = "tests-effects", feature = "tests-lua", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
pub(crate) fn fleet_snapshot(rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::space::SceneDelivery>) -> Option<Vec<marshal::Value>> {
    match rx.try_recv() {
        Ok(crate::space::SceneDelivery::Notify { event: "OnFleetStateChange", args }) => Some(args),
        Ok(crate::space::SceneDelivery::Notify { event, .. }) => panic!("unexpected scene notify {event}"),
        Ok(crate::space::SceneDelivery::Destiny(_)) => panic!("expected fleet snapshot, got Destiny batch"),
        Err(_) => None,
    }
}

#[cfg(all(test, any(feature = "tests-runtime", feature = "tests-social", feature = "tests-combat", feature = "tests-effects", feature = "tests-ships")))]
pub(crate) fn publish_scene_prefix(system: &mut crate::space::System, collisions: bool) {
    system.apply_scene_prefix(collisions);
}

#[cfg(any(feature = "tests-runtime", feature = "tests-ships", feature = "tests-lua"))]
pub(crate) fn tick_with_collisions(system: &mut System, ms: i64, collisions: bool) {
    system.tick_with_collisions(ms, collisions);
}

/// Install `system` as a running actor in `world` without loading: the fixture
/// owns the `System` and its handle joins `world.systems` like the cold loader.
#[cfg(any(feature = "tests-runtime", feature = "tests-slash"))]
pub(crate) fn install_publication_actor(world: &crate::space::World, system: System) {
    let id = system.id;
    let handle = crate::space::actor::start(system, id, std::sync::Arc::downgrade(&world.systems), world.db.clone());
    world.systems.lock().unwrap().insert(id, handle);
}

#[cfg(feature = "tests-lua")]
pub(crate) fn install_progress_registry(system: &mut System, db: &crate::db::Db) {
    system.progress_registry = db.progress_registry.clone();
}

/// Install the actor's static-data context so tests exercise the real global
/// roster group classification instead of the no-data admission fallback.
#[cfg(feature = "tests-combat")]
pub(crate) fn install_projection_sde(system: &mut crate::space::System, sde: std::sync::Arc<crate::sde::Sde>) {
    system.projection_sde = Some(sde);
}

#[cfg(any(feature = "tests-space", feature = "tests-lua"))]
impl System {
    /// Queue cancellation of any previous landing intent before a new warp request.
    #[cfg(feature = "tests-space")]
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

#[cfg(feature = "tests-ships")]
pub(crate) fn completed_segment_samples(ball: &Ball) -> [(f64, V3, V3); 5] {
    [(0.0, 0), (0.1, 100), (0.5, 500), (0.9, 900), (1.0, 1000)].map(|(fraction, offset)| {
        let time = ball.interpolation_anchor_ms.checked_add(offset).expect("segment sample time overflow");
        let (pos, vel) = ball.state_at(time);
        (fraction, pos, vel)
    })
}

#[cfg(any(feature = "tests-ships", feature = "tests-social"))]
pub(crate) fn encoded_ball_for_test(ball: &Ball, collisions: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    ball.write(&mut bytes, collisions);
    bytes
}

#[cfg(feature = "tests-ships")]
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
