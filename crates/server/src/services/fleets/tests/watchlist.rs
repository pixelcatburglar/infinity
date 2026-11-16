use super::*;

#[test]
fn subscriptions_validate_membership_deduplicate_and_disable_without_erasing_list() {
    let mut st = State::default();
    let mut fleet = Fleet::new(1, vec![]);
    for id in [1, 2, 3] {
        fleet.members.insert(id, Member::new(1, 1, ROLE_MEMBER, JOB_NONE, 1));
        st.of.insert(id, 10);
    }
    st.fleets.insert(10, fleet);
    let list = |ids: &[i64]| Value::list(ids.iter().copied().map(Value::Int));
    let update = super::super::fleet_mgr::update_watchlist;
    st.fleets.get_mut(&10).unwrap().members.get_mut(&2).unwrap().reconnect = Some(ReconnectState { generation: 1, deadline: Instant::now(), offered_session: None });
    assert!(update(&mut st, 1, "AddToWatchlist", &[list(&[2, 2, 3]), Value::None]).is_ok());
    assert_eq!(st.fleets[&10].members[&1].watchlist, vec![2, 3]);
    assert!(update(&mut st, 1, "AddToWatchlist", &[list(&[4]), Value::None]).is_err());
    assert!(update(&mut st, 1, "AddToWatchlist", &[list(&[1]), Value::None]).is_err());
    assert!(update(&mut st, 1, "RegisterForDamageUpdates", &[list(&[3, 2])]).is_ok());
    assert!(st.fleets[&10].members[&1].damage_updates);
    assert!(update(&mut st, 1, "RegisterForDamageUpdates", &[Value::None]).is_ok());
    assert!(!st.fleets[&10].members[&1].damage_updates);
    assert_eq!(st.fleets[&10].members[&1].watchlist, vec![3, 2]);
    assert!(update(&mut st, 1, "RemoveFromWatchlist", &[Value::Int(3), Value::None]).is_ok());
    assert_eq!(st.fleets[&10].members[&1].watchlist, vec![2]);
    st.fleets.get_mut(&10).unwrap().members.get_mut(&1).unwrap().reconnect = Some(ReconnectState { generation: 2, deadline: Instant::now(), offered_session: None });
    assert!(update(&mut st, 1, "AddToWatchlist", &[list(&[2]), Value::None]).is_err());
}

#[test]
fn client_watchlist_deltas_reconcile_subscription_atomically() {
    let mut st = State::default();
    let mut fleet = Fleet::new(1, vec![]);
    for id in [1, 2, 3, 4] {
        fleet.members.insert(id, Member::new(1, 1, ROLE_MEMBER, JOB_NONE, 1));
        st.of.insert(id, 10);
    }
    st.fleets.insert(10, fleet);
    let mut other = Fleet::new(5, vec![]);
    other.members.insert(5, Member::new(1, 1, ROLE_MEMBER, JOB_NONE, 1));
    st.of.insert(5, 20);
    st.fleets.insert(20, other);
    let list = |ids: &[i64]| Value::list(ids.iter().copied().map(Value::Int));
    let update = super::super::fleet_mgr::update_watchlist;

    assert!(matches!(update(&mut st, 1, "AddToWatchlist", &[list(&[2]), list(&[2])]), Ok(Some(ids)) if ids == vec![2]));
    assert!(st.fleets[&10].members[&1].damage_updates);
    assert!(matches!(update(&mut st, 1, "AddToWatchlist", &[list(&[3]), list(&[3, 2, 3])]), Ok(Some(ids)) if ids == vec![3]));
    assert_eq!(st.fleets[&10].members[&1].watchlist, vec![3, 2]);

    for full in [list(&[2, 3, 4, 5]), list(&[2, 3]), list(&[2, 3, 4, 1])] {
        assert!(update(&mut st, 1, "AddToWatchlist", &[list(&[4]), full]).is_err());
        let member = &st.fleets[&10].members[&1];
        assert!(member.damage_updates);
        assert_eq!(member.watchlist, vec![3, 2]);
    }
    assert!(update(&mut st, 1, "RemoveFromWatchlist", &[Value::Int(3), list(&[3, 2])]).is_err());
    assert_eq!(st.fleets[&10].members[&1].watchlist, vec![3, 2]);
    assert!(st.fleets[&10].members[&1].damage_updates);

    assert!(matches!(update(&mut st, 1, "RemoveFromWatchlist", &[Value::Int(3), Value::None]), Ok(None)));
    assert!(!st.fleets[&10].members[&1].damage_updates);
    assert_eq!(st.fleets[&10].members[&1].watchlist, vec![2]);
    assert!(matches!(update(&mut st, 1, "AddToWatchlist", &[list(&[3]), Value::None]), Ok(None)));
    assert!(!st.fleets[&10].members[&1].damage_updates);
    assert!(matches!(update(&mut st, 1, "AddToWatchlist", &[list(&[4]), list(&[4, 3, 2])]), Ok(Some(ids)) if ids == vec![4, 3, 2]));
    assert!(matches!(update(&mut st, 1, "RemoveFromWatchlist", &[Value::Int(3), list(&[2, 4])]), Ok(None)));
    assert_eq!(st.fleets[&10].members[&1].watchlist, vec![2, 4]);
    assert!(st.fleets[&10].members[&1].damage_updates);
}

#[test]
fn damage_ballpark_update_requires_live_observer_and_uses_destination_stamp() {
    use crate::space::{Ball, Pilots, WarpState, MODE_STOP, SPECIAL_FX_UNSPECIFIED_DURATION, event, special_fx};
    use crate::space::system::System;
    use std::sync::Arc;
    crate::tests::init_config();
    let pilots = Arc::new(Pilots::default());
    let (notify, mut notifications) = tokio::sync::mpsc::unbounded_channel();
    pilots.register_pilot(1, notify);
    let mut system = System::new(20, pilots, 42_000);
    let fixture = Ball {
        id: 100, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: Some(1),
        signature_radius: Some(1.0), mode: MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 100.0,
        agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: true,
        massive: true, warp: WarpState::default(), last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new() };
    system.insert(Ball { id: 200, char_id: None, ..fixture.clone() });
    system.insert(fixture);
    let health = Value::tuple([Value::Int(1), Value::Int(2)]);
    system.fleet_damage_update(1, 10, 200, health.clone());
    assert!(notifications.try_recv().is_err());
    let connection = 1_000_000_000_001;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (completed_1, mut done_1) = tokio::sync::oneshot::channel();
    assert!(system.observe(connection, 999, tx, completed_1));
    crate::space::system::tests::events::publish_scene_prefix(&mut system, crate::space::collision::enabled());
    assert_eq!(done_1.try_recv(), Ok(true));
    crate::space::system::tests::events::initial_state(&mut rx);
    crate::space::system::tests::events::fleet_snapshot(&mut rx);
    system.fleet_damage_update(1, 10, 200, health.clone());
    assert!(rx.try_recv().is_err());
    system.unobserve(connection);
    drop(rx);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (completed_2, mut done_2) = tokio::sync::oneshot::channel();
    assert!(system.observe(connection, 100, tx, completed_2));
    crate::space::system::tests::events::publish_scene_prefix(&mut system, crate::space::collision::enabled());
    assert_eq!(done_2.try_recv(), Ok(true));
    crate::space::system::tests::events::initial_state(&mut rx);
    crate::space::system::tests::events::fleet_snapshot(&mut rx);
    system.fleet_damage_update(1, 20, 200, health.clone());
    assert!(rx.try_recv().is_err());
    system.fleet_damage_update(1, 10, 200, health.clone());
    assert_eq!(crate::space::system::tests::events::destiny(rx.try_recv().unwrap()), vec![event(42, "OnFleetDamageStateChange", vec![Value::Int(200), health])]);
    // Ball 200 takes the same real module-cloak entry as the own-ego ball;
    // the foreign ball's scene clients remove it themselves, so its FX, one
    // CloakBall and the fleet clear arrive and nothing follows them.
    assert!(system.begin_module_cloak(200, -2, Arc::new(std::sync::atomic::AtomicBool::new(false)), 0, 0, |_| true, None));
    crate::space::system::tests::events::publish_scene_prefix(&mut system, crate::space::collision::enabled());
    assert_eq!(crate::space::system::tests::events::destiny(rx.try_recv().unwrap()), vec![
        special_fx(42, 200, None, None, None, "effects.Cloak", false, 1, 0,
            Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None),
        event(42, "CloakBall", vec![Value::Int(200)])]);
    crate::space::system::tests::events::fleet_snapshot(&mut rx);
    assert!(rx.try_recv().is_err());
    // The own-ego ball cloaks through the real module-cloak entry; its
    // scene CloakBall is drained as itself before the covered-ego update.
    assert!(system.begin_module_cloak(100, -1, Arc::new(std::sync::atomic::AtomicBool::new(false)), 0, 0, |_| true, None));
    crate::space::system::tests::events::publish_scene_prefix(&mut system, crate::space::collision::enabled());
    assert_eq!(crate::space::system::tests::events::destiny(rx.try_recv().unwrap()), vec![
        special_fx(42, 100, None, None, None, "effects.Cloak", false, 1, 0,
            Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None),
        event(42, "CloakBall", vec![Value::Int(100)])]);
    crate::space::system::tests::events::fleet_snapshot(&mut rx);
    system.fleet_damage_update(1, 20, 100, Value::None);
    assert!(rx.try_recv().is_err());
    system.fleet_damage_update(1, 20, 200, Value::None);
    assert!(rx.try_recv().is_ok());
    assert!(notifications.try_recv().is_err());
    drop(rx);
    system.fleet_damage_update(1, 10, 200, Value::None);
    assert!(notifications.try_recv().is_err());
    system.unobserve(connection);
    system.fleet_damage_update(1, 10, 200, Value::None);
    assert!(notifications.try_recv().is_err());
}
