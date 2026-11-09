use super::*;

fn setup(base: i64) -> State {
    let mut st = State::default();
    let mut fleet = Fleet::new(base, vec![]);
    for (offset, wing, squad, role) in [(0, -1, -1, ROLE_LEADER), (1, 10, -1, ROLE_WING_COMMANDER),
        (2, 10, 20, ROLE_SQUAD_COMMANDER), (3, 10, 20, ROLE_MEMBER),
        (4, 10, 21, ROLE_MEMBER), (5, 11, 22, ROLE_MEMBER)] {
        fleet.members.insert(base + offset, Member::new(wing, squad, role, JOB_NONE, 1));
        st.of.insert(base + offset, base);
    }
    st.fleets.insert(base, fleet);
    st
}

fn ids(order: &Order) -> Vec<i64> {
    let mut ids: Vec<_> = order.members.iter().map(|(id, _)| *id).collect();
    ids.sort_unstable();
    ids
}

#[test]
fn scope_consent_and_commit_fences() {
    let mut st = setup(1);
    assert_eq!(ids(&snapshot(&st, 1, 101).unwrap()), vec![2, 3, 4, 5, 6]);
    assert_eq!(ids(&snapshot(&st, 2, 102).unwrap()), vec![3, 4, 5]);
    let order = snapshot(&st, 3, 103).unwrap();
    assert_eq!(ids(&order), vec![4]);
    assert!(snapshot(&st, 4, 104).is_none());
    let position = order.members[0].1;
    assert!(current(&st, &order, 4, position));
    st.fleets.get_mut(&1).unwrap().members.get_mut(&4).unwrap().opt_outs.accepts_fleet_regroups = false;
    assert!(!current(&st, &order, 4, position));
    assert!(snapshot(&st, 3, 103).unwrap().members.is_empty());
    let member = st.fleets.get_mut(&1).unwrap().members.get_mut(&4).unwrap();
    member.opt_outs.accepts_fleet_regroups = true;
    member.squad = 21;
    assert!(!current(&st, &order, 4, position));
    st.fleets.get_mut(&1).unwrap().members.get_mut(&4).unwrap().squad = 20;
    st.of.remove(&4);
    assert!(!current(&st, &order, 4, position));
    st.of.insert(4, 1);
    st.fleets.get_mut(&1).unwrap().members.get_mut(&3).unwrap().role = ROLE_MEMBER;
    assert!(!current(&st, &order, 4, position));
}

fn ball(id: i64, pilot: i64, pos: [f64; 3]) -> space::Ball {
    space::Ball {
        id, type_id: 1, owner_id: pilot, corp_id: 1, alliance_id: -1,
        char_id: Some(pilot), mode: space::MODE_STOP, radius: 1.0, signature_radius: None,
        mass: 1_000_000.0, max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos, vel: [0.0; 3], old_pos: pos, old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: false, interactive: true,
        warp: space::WarpState::default(), last_ms: 1000,
        interpolation_anchor_ms: 1000, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: vec![],
    }
}

#[test]
fn actor_moves_only_live_consented_subordinates_and_rechecks_at_action() {
    crate::tests::init_config();
    let base = 8_923_001;
    let mut fixture = setup(base);
    let order = snapshot(&fixture, base + 2, base + 102).unwrap();
    {
        let mut global = state();
        global.of.extend(fixture.of.drain());
        global.fleets.insert(base, fixture.fleets.remove(&base).unwrap());
    }
    let mut s = space::System::new(1, std::sync::Arc::default(), 1000);
    s.insert(ball(base + 102, base + 2, [500.0, 0.0, 0.0]));
    s.insert(ball(base + 103, base + 3, [0.0; 3]));
    s.insert(ball(base + 104, base + 4, [0.0; 3]));
    enqueue(&mut s, order);
    { state().fleets.get_mut(&base).unwrap().members.get_mut(&(base + 3)).unwrap().opt_outs.accepts_fleet_regroups = false; }
    s.tick(2000);
    assert_eq!(s.ball(base + 103).unwrap().mode, space::MODE_STOP);
    { state().fleets.get_mut(&base).unwrap().members.get_mut(&(base + 3)).unwrap().opt_outs.accepts_fleet_regroups = true; }
    let order = snapshot(&state(), base + 2, base + 102).unwrap();
    enqueue(&mut s, order);
    s.tick(3000);
    assert_eq!(s.ball(base + 103).unwrap().mode, space::MODE_GOTO);
    assert_eq!(s.ball(base + 103).unwrap().goto, [500.0, 0.0, 0.0]);
    assert_eq!(s.ball(base + 102).unwrap().mode, space::MODE_STOP);
    assert_eq!(s.ball(base + 104).unwrap().mode, space::MODE_STOP);
    let mut global = state();
    global.fleets.remove(&base);
    for id in base..=base + 5 { global.of.remove(&id); }
}
