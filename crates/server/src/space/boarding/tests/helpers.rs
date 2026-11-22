use super::super::*;
use crate::space::WarpState;

pub(super) fn ball(id: i64, kind: i32, pilot: Option<i64>, x: f64) -> Ball {
    Ball {
        id, type_id: kind, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: Some(1.0), mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0],
        old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: true, interactive: pilot.is_some(), warp: WarpState::default(),
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

pub(super) fn delivered(s: &mut System, rx: &mut mpsc::UnboundedReceiver<crate::space::SceneDelivery>) -> Vec<Value> {
    s.apply_scene_prefix(crate::space::collision::enabled());
    let mut events = Vec::new();
    while let Ok(delivery) = rx.try_recv() {
        match delivery {
            crate::space::SceneDelivery::Destiny(batch) => events.extend(batch),
            crate::space::SceneDelivery::Notify { event, .. } => assert_eq!(event, "OnFleetStateChange"),
        }
    }
    events
}

pub(super) fn expected_add(s: &System, stamp: i64, ids: &[i64]) -> Value {
    let collisions = crate::space::collision::enabled();
    let mut ordered: Vec<i64> = ids.to_vec();
    ordered.sort_unstable();
    let mut stream = vec![1u8]; // DESTINY_BALLS kind byte
    stream.extend_from_slice(&(stamp as i32).to_le_bytes());
    let mut slims = Vec::new();
    let mut damage = Vec::new();
    for id in ordered {
        let ball = s.ball(id).unwrap();
        ball.write(&mut stream, collisions);
        slims.push(ball.slim_item());
        if ball.free { damage.push((Value::Int(id), ball.damage_state())); }
    }
    crate::space::event(stamp, "AddBalls", vec![Value::tuple([Value::str(stream), Value::list(slims), Value::Dict(damage)])])
}
