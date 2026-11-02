use super::*;
use std::time::Duration;

#[test]
fn reservations_keep_seat_and_reject_stale_sessions_without_renewal() {
    let mut st = State::default();
    let mut f = Fleet::new(1, vec![]);
    f.initialized = true;
    let mut m = Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_CREATOR, 42);
    m.active_session = Some(10);
    m.damage_updates = true;
    f.members.insert(1, m);
    st.fleets.insert(2, f);
    st.of.insert(1, 2);
    let now = Instant::now();
    let timeout = Duration::from_secs(30);
    assert!(lifecycle::reserve(&mut st, 1, 9, now, timeout).is_none());
    let (_, generation, deadline) = lifecycle::reserve(&mut st, 1, 10, now, timeout).unwrap();
    assert_eq!(deadline, now + timeout);
    assert!(st.has_member(2, 1));
    assert!(!st.is_member(2, 1));
    assert!(!st.fleets[&2].members[&1].damage_updates);
    st.fleets.get_mut(&2).unwrap().members.get_mut(&1).unwrap().reconnect.as_mut().unwrap().offered_session = Some(11);
    assert!(lifecycle::reserve(&mut st, 1, 10, now + timeout, timeout).is_none());
    let r = st.fleets[&2].members[&1].reconnect.as_ref().unwrap();
    assert_eq!((r.generation, r.deadline, r.offered_session), (generation, deadline, Some(11)));
    lifecycle::reserve(&mut st, 1, 11, now, timeout);
    assert_eq!(st.fleets[&2].members[&1].reconnect.as_ref().unwrap().offered_session, None);
    let m = st.fleets.get_mut(&2).unwrap().members.get_mut(&1).unwrap();
    m.reconnect = None;
    m.active_session = Some(11);
    m.recovered_generation = Some(generation);
    let (_, next, _) = lifecycle::reserve(&mut st, 1, 11, now, timeout).unwrap();
    assert!(next > generation);
    assert!(lifecycle::reserve(&mut st, 9, 11, now, timeout).is_none());
}

#[test]
fn disconnect_cleans_pending_without_touching_newer_session_ownership() {
    let mut st = State::default();
    let mut other = Fleet::new(2, vec![]);
    other.initialized = true;
    other.invites.insert(1, (1, 1, ROLE_MEMBER));
    other.join_requests.insert(1, Value::None);
    st.fleets.insert(2, other);
    let mut shell = Fleet::new(1, vec![]);
    shell.creator_session = Some(20);
    st.fleets.insert(3, shell);
    let now = Instant::now();
    let timeout = Duration::from_secs(30);
    lifecycle::reserve(&mut st, 1, 10, now, timeout);
    assert!(st.fleets.contains_key(&3));
    assert!(!st.fleets[&2].invites.contains_key(&1));
    st.fleets.get_mut(&2).unwrap().invites.insert(1, (1, 1, ROLE_MEMBER));
    lifecycle::reserve(&mut st, 1, 20, now, timeout);
    assert!(!st.fleets.contains_key(&3));
    assert!(!st.fleets[&2].invites.contains_key(&1));
    assert!(!st.fleets[&2].join_requests.contains_key(&1));

    let mut member = Member::new(NONE_ID, NONE_ID, ROLE_MEMBER, JOB_NONE, 0);
    member.active_session = Some(20);
    st.fleets.get_mut(&2).unwrap().members.insert(1, member);
    st.of.insert(1, 2);
    st.fleets.get_mut(&2).unwrap().invites.insert(1, (1, 1, ROLE_MEMBER));
    lifecycle::reserve(&mut st, 1, 10, now, timeout);
    assert!(st.fleets[&2].invites.contains_key(&1));
    let (_, generation, deadline) = lifecycle::reserve(&mut st, 1, 20, now, timeout).unwrap();
    let f = st.fleets.get_mut(&2).unwrap();
    f.invites.insert(1, (1, 1, ROLE_MEMBER));
    f.members.get_mut(&1).unwrap().reconnect.as_mut().unwrap().offered_session = Some(30);
    lifecycle::reserve(&mut st, 1, 20, now, timeout);
    assert!(st.fleets[&2].invites.contains_key(&1));
    lifecycle::reserve(&mut st, 1, 30, now + timeout, timeout);
    let reservation = st.fleets[&2].members[&1].reconnect.as_ref().unwrap();
    assert_eq!((reservation.generation, reservation.deadline, reservation.offered_session), (generation, deadline, None));
    assert!(!st.fleets[&2].invites.contains_key(&1));
}
