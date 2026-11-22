//! Deadline registry behavior: registration arms it, the same channel does
//! not refresh it, replacement keeps the later deadline, commits record it
//! and session generations fence it. Pure registry state, with no actor or database.

use super::*;

fn session(character: i64, session_id: i64) -> infinity_core::session::OnlineSession {
    infinity_core::session::OnlineSession { character_id: character, user_id: Some(1), session_id,
        ship_id: Some(9), station_id: None, system_id: Some(3), corporation_id: None, alliance_id: None,
        faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }
}

fn live(pilots: &Pilots, character: i64, session_id: i64, now: i64) -> mpsc::UnboundedReceiver<(String, Vec<Value>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    pilots.register_pilot_clock(character, tx, now);
    pilots.set_online_session(character, Some(session(character, session_id)));
    rx
}

#[test]
fn registration_arms_the_client_delay_for_the_live_generation() {
    let pilots = Pilots::default();
    let admitted = 1_000;
    let _rx = live(&pilots, 7, 1, admitted);
    let deadline = ship_change_deadline(admitted);
    assert!(!pilots.ship_change_ready(7, 1, deadline - 1));
    assert!(!pilots.ship_change_ready(7, 2, deadline));
    assert!(!pilots.ship_change_ready(8, 1, i64::MAX));
    assert!(pilots.ship_change_ready(7, 1, deadline));
}

#[test]
fn same_channel_callback_keeps_deadline_and_session() {
    let pilots = Pilots::default();
    let admitted = 1_000;
    let (tx, rx) = mpsc::unbounded_channel();
    pilots.register_pilot_clock(7, tx.clone(), admitted);
    pilots.set_online_session(7, Some(session(7, 1)));
    let deadline = ship_change_deadline(admitted);
    pilots.register_pilot_clock(7, tx, admitted + FILETIME_SECOND);
    assert_eq!(pilots.online_session(7).unwrap().session_id, 1);
    assert!(!pilots.ship_change_ready(7, 1, deadline - 1));
    assert!(pilots.ship_change_ready(7, 1, deadline));
    drop(rx);
}

#[test]
fn replacement_keeps_the_later_deadline_and_clears_the_session() {
    let pilots = Pilots::default();
    let admitted = 1_000;
    let _rx = live(&pilots, 7, 1, admitted);
    let deadline = ship_change_deadline(admitted);
    let (tx, _later) = mpsc::unbounded_channel();
    let replaced = admitted + FILETIME_SECOND;
    pilots.register_pilot_clock(7, tx, replaced);
    assert!(pilots.online_session(7).is_none());
    pilots.set_online_session(7, Some(session(7, 1)));
    let later = ship_change_deadline(replaced);
    assert!(!pilots.ship_change_ready(7, 1, later - 1));
    assert!(pilots.ship_change_ready(7, 1, later));
    let (tx, _earlier) = mpsc::unbounded_channel();
    pilots.register_pilot_clock(7, tx, admitted - FILETIME_SECOND);
    pilots.set_online_session(7, Some(session(7, 1)));
    assert!(!pilots.ship_change_ready(7, 1, later - 1));
    assert!(deadline < later);
}

#[test]
fn commit_keeps_the_later_deadline_and_fences_the_generation() {
    let pilots = Pilots::default();
    let admitted = 1_000;
    let rx = live(&pilots, 7, 1, admitted);
    let deadline = ship_change_deadline(admitted);
    assert!(!pilots.commit_ship_change(7, 2, deadline + FILETIME_SECOND));
    assert!(!pilots.commit_ship_change(8, 1, deadline + FILETIME_SECOND));
    assert!(!pilots.ship_change_ready(7, 1, deadline - 1));
    let frozen = deadline + 5 * FILETIME_SECOND;
    assert!(pilots.commit_ship_change(7, 1, frozen));
    assert!(!pilots.ship_change_ready(7, 1, frozen - 1));
    assert!(pilots.ship_change_ready(7, 1, frozen));
    assert!(pilots.commit_ship_change(7, 1, deadline));
    assert!(pilots.commit_ship_change(7, 1, 0));
    assert!(!pilots.ship_change_ready(7, 1, frozen - 1));
    assert!(pilots.ship_change_ready(7, 1, frozen));
    drop(rx);
    assert!(!pilots.commit_ship_change(7, 1, frozen + FILETIME_SECOND));
    assert!(!pilots.ship_change_ready(7, 1, frozen));
}

#[test]
fn unregister_frees_the_record_and_a_new_registration_rearms() {
    let pilots = Pilots::default();
    let admitted = 1_000;
    let _rx = live(&pilots, 7, 1, admitted);
    let deadline = ship_change_deadline(admitted);
    assert!(pilots.commit_ship_change(7, 1, admitted));
    assert!(!pilots.ship_change_ready(7, 1, admitted));
    assert!(pilots.ship_change_ready(7, 1, deadline));
    assert!(pilots.unregister_pilot_generation(7, Some(1)));
    let rejoined = admitted + 60 * FILETIME_SECOND;
    let _rejoined = live(&pilots, 7, 2, rejoined);
    let deadline = ship_change_deadline(rejoined);
    assert!(!pilots.ship_change_ready(7, 2, deadline - 1));
    assert!(pilots.ship_change_ready(7, 2, deadline));
}

#[test]
fn actor_reads_the_registry_it_holds() {
    let pilots = Arc::new(Pilots::default());
    let admitted = 1_000;
    let _rx = live(&pilots, 7, 1, admitted);
    let system = System::new(1, pilots.clone(), 0);
    let deadline = ship_change_deadline(admitted);
    assert!(!system.ship_change_ready(7, 1, deadline - 1));
    assert!(system.ship_change_ready(7, 1, deadline));
    assert!(pilots.commit_ship_change(7, 1, deadline + FILETIME_SECOND));
    assert!(!system.ship_change_ready(7, 1, deadline));
}
