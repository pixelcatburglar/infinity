use super::*;

mod broadcasts;
mod finder;
mod watchlist;
mod reconnect;
mod reconnect_protocol;

#[test]
fn initialization_requires_creator_and_is_single_use() {
    let mut state = State::default();
    state.fleets.insert(10, Fleet::new(20, vec![Wing { id: 30, name: String::new(), squads: vec![Squad { id: 40, name: String::new() }] }]));
    assert!(state.initialize(10, 21, 50).is_none());
    let member = state.initialize(10, 20, 50).unwrap();
    assert_eq!((member.wing, member.squad, member.role, member.job, member.ship_type), (NONE_ID, NONE_ID, ROLE_LEADER, JOB_CREATOR, 50));
    assert!(state.is_member(10, 20));
    assert!(!state.is_member(11, 20));
    assert!(!state.is_member(10, 21));
    assert!(state.initialize(10, 20, 50).is_none());
    state.of.remove(&20);
    assert!(state.initialize(10, 20, 50).is_none());
    assert!(!state.is_member(10, 20));
}

#[test]
fn preferences_and_options_serialize_current_values() {
    let before = Member::new(1, 2, ROLE_MEMBER, JOB_NONE, 3);
    let mut after = before.clone();
    after.opt_outs.accepts_fleet_warp = false;
    after.opt_outs.accepts_fleet_regroups = false;
    let old = keyval([( "acceptsFleetWarp", Value::Bool(true)), ("acceptsConduitJumps", Value::Bool(true)), ("acceptsFleetRegroups", Value::Bool(true))]);
    let new = keyval([( "acceptsFleetWarp", Value::Bool(false)), ("acceptsConduitJumps", Value::Bool(true)), ("acceptsFleetRegroups", Value::Bool(false))]);
    assert_eq!(opt_outs_value(&before.opt_outs), old);
    assert_eq!(changed(5, 6, &before, &after, false)[6], old);
    assert_eq!(changed(5, 6, &before, &after, false)[11], new);
    assert_eq!(member_value(5, &after), keyval([("charID", Value::Int(5)), ("wingID", Value::Int(1)), ("squadID", Value::Int(2)), ("role", Value::Int(ROLE_MEMBER)), ("job", Value::Int(JOB_NONE)), ("shipTypeID", Value::Int(3)), ("memberOptOuts", new)]));
    let options = FleetOptions { free_move: true, registered: true, auto_join_squad: Some(40) };
    assert_eq!(options_value(&options), keyval([("isFreeMove", Value::Bool(true)), ("isRegistered", Value::Bool(true)), ("isVoiceEnabled", Value::Bool(false)), ("autoJoinSquadID", Value::Int(40))]));
    assert_eq!(options_value(&FleetOptions::default()), keyval([("isFreeMove", Value::Bool(false)), ("isRegistered", Value::Bool(false)), ("isVoiceEnabled", Value::Bool(false)), ("autoJoinSquadID", Value::None)]));
}

#[test]
fn fleet_and_member_runtime_defaults() {
    let member = Member::new(1, 2, ROLE_MEMBER, JOB_NONE, 3);
    assert!(member.watchlist.is_empty());
    assert!(!member.damage_updates);
    let fleet = Fleet::new(1, vec![]);
    assert_eq!(fleet.max_members, MAX_MEMBERS);
    assert!(fleet.join_requests.is_empty());
    assert!(fleet.advert.is_none());
}

#[test]
fn departing_member_references_are_removed_across_fleets() {
    let mut state = State::default();
    for fleet_id in [1, 2] {
        let mut fleet = Fleet::new(fleet_id, vec![]);
        let mut member = Member::new(1, 2, ROLE_MEMBER, JOB_NONE, 3);
        member.watchlist = vec![7, 8, 7];
        fleet.members.insert(fleet_id, member);
        fleet.join_requests.insert(7, Value::None);
        fleet.join_requests.insert(8, Value::None);
        state.fleets.insert(fleet_id, fleet);
    }
    state.clear_member_references(7);
    for fleet in state.fleets.values() {
        assert!(fleet.join_requests.contains_key(&7));
        assert!(fleet.join_requests.contains_key(&8));
        assert_eq!(fleet.members.values().next().unwrap().watchlist, vec![8]);
    }
}

#[test]
fn creation_and_departure_clean_uninitialized_and_pending_state() {
    let mut st = State::default();
    assert!(st.can_create(1));
    st.fleets.insert(10, Fleet::new(1, vec![]));
    assert!(!st.can_create(1));
    let mut other = Fleet::new(2, vec![]);
    other.initialized = true;
    other.invites.insert(1, (0, 0, ROLE_MEMBER));
    other.join_requests.insert(1, Value::None);
    st.fleets.insert(20, other);
    st.clear_pending(1);
    assert!(st.can_create(1));
    assert!(!st.fleets.contains_key(&10));
    assert!(!st.fleets[&20].invites.contains_key(&1));
    assert!(!st.fleets[&20].join_requests.contains_key(&1));
    st.of.insert(1, 20);
    assert!(!st.can_create(1));
}

#[test]
fn invitation_requires_live_other_character() {
    use super::fleet_object_handler::valid_invite_target;
    assert!(valid_invite_target(1, 2, true, true));
    assert!(!valid_invite_target(1, 1, true, true));
    assert!(!valid_invite_target(1, 0, true, true));
    assert!(!valid_invite_target(1, 2, false, true));
    assert!(!valid_invite_target(1, 2, true, false));
}

#[test]
fn rename_requires_existing_structure() {
    let mut fleet = Fleet::new(1, vec![Wing { id: 2, name: "wing".into(), squads: vec![Squad { id: 3, name: "squad".into() }] }]);
    assert!(!fleet.rename_structure(99, "missing", true));
    assert!(!fleet.rename_structure(99, "missing", false));
    assert_eq!(fleet.wings[0].name, "wing");
    assert_eq!(fleet.wings[0].squads[0].name, "squad");
    assert!(fleet.rename_structure(2, "new wing", true));
    assert!(fleet.rename_structure(3, "new squad", false));
    assert_eq!(fleet.wings[0].name, "new wing");
    assert_eq!(fleet.wings[0].squads[0].name, "new squad");
}
