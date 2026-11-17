use super::*;
use std::future::Future;
use tokio::sync::mpsc::unbounded_channel;

fn online(character_id: i64, session_id: i64) -> OnlineSession {
    OnlineSession { character_id, user_id: None, session_id, ship_id: None, station_id: None, system_id: None,
        corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }
}

#[test]
fn wire_args_carry_label_future_when_and_zero_duration() {
    let mut state = State::default();
    let before = filetime_now();
    let when = schedule(&mut state, 5).unwrap();
    assert!((before + 5 * FILETIME_MINUTE..=filetime_now() + 5 * FILETIME_MINUTE).contains(&when));
    assert_eq!(initiated_args(when), vec![Value::str("/EVE-Universe/ClusterBroadcast/UnscheduledDowntime"), Value::Int(when), Value::Int(0)]);
    assert_eq!(cancelled_args(), vec![Value::str("/EVE-Universe/ClusterBroadcast/ShutdownCancelled")]);
}

#[test]
fn invalid_and_overflow_minutes_keep_the_old_plan() {
    let mut state = State::default();
    let when = schedule(&mut state, 10).unwrap();
    for minutes in [0, -1, i64::MAX] {
        assert!(matches!(schedule(&mut state, minutes), Err(StartError::Invalid)));
    }
    assert_eq!(state.plan.unwrap().when, when);
}

#[tokio::test(start_paused = true)]
async fn replacement_clears_delivered_and_due_blocks_all_changes() {
    let mut state = State::default();
    assert_eq!(cancel_locked(&mut state), CancelResult::None);
    schedule(&mut state, 10).unwrap();
    assert_eq!(cancel_locked(&mut state), CancelResult::Cancelled);
    assert!(state.plan.is_none() && !rejecting_locked(&state, 120));
    schedule(&mut state, 10).unwrap();
    state.delivered.insert((7, 3));
    let first = state.generation;
    schedule(&mut state, 20).unwrap();
    assert!(state.generation > first && state.delivered.is_empty());
    tokio::time::advance(Duration::from_secs(21 * 60)).await;
    assert_eq!(cancel_locked(&mut state), CancelResult::Due);
    assert!(state.plan.is_some(), "a due plan is never cancelled");
    assert!(matches!(schedule(&mut state, 5), Err(StartError::Due)));
}

#[tokio::test(start_paused = true)]
async fn status_and_rejection_window_cover_the_boundary() {
    let mut state = State::default();
    assert!(matches!(status_locked(&mut state), Status::None));
    assert!(!rejecting_locked(&state, 120));
    schedule(&mut state, 10).unwrap();
    assert!(matches!(status_locked(&mut state), Status::Remaining(_)));
    assert!(!rejecting_locked(&state, 60) && rejecting_locked(&state, 600));
    tokio::time::advance(Duration::from_secs(8 * 60)).await;
    assert!(rejecting_locked(&state, 120), "equality still refuses");
    tokio::time::advance(Duration::from_secs(3 * 60)).await;
    assert!(matches!(status_locked(&mut state), Status::Triggered));
    assert!(rejecting_locked(&state, 0), "triggered always refuses");
    assert_eq!(cancel_locked(&mut state), CancelResult::Due);
}

#[tokio::test(start_paused = true)]
async fn catch_up_prunes_delivered_and_resends_once_per_session() {
    let mut state = State::default();
    schedule(&mut state, 10).unwrap();
    let (tx, mut rx) = unbounded_channel::<(String, Vec<Value>)>();
    state.delivered.insert((9, 9));
    let live = || vec![online(7, 3), online(8, 1)];
    deliver(&mut state, live(), 7, 3, |e, a| tx.send((e.to_owned(), a)).unwrap());
    assert_eq!(rx.try_recv().unwrap().0, EVENT_INITIATED);
    assert!(!state.delivered.contains(&(9, 9)));
    deliver(&mut state, live(), 7, 3, |_, _| panic!("already delivered"));
    deliver(&mut state, live(), 7, 4, |e, a| tx.send((e.to_owned(), a)).unwrap());
    assert_eq!(rx.try_recv().unwrap().0, EVENT_INITIATED);
}

#[test]
fn shared_channel_keeps_broadcast_then_cancel_and_cancel_then_catch_up() {
    let mut state = State::default();
    schedule(&mut state, 10).unwrap();
    let (tx, mut rx) = unbounded_channel::<(String, Vec<Value>)>();
    broadcast(&mut state, vec![online(7, 3)], |_, e, a| tx.send((e.to_owned(), a)).unwrap());
    assert!(state.delivered.contains(&(7, 3)));
    tx.send((EVENT_CANCELLED.to_owned(), cancelled_args())).unwrap();
    assert_eq!(rx.try_recv().unwrap().0, EVENT_INITIATED);
    assert_eq!(rx.try_recv().unwrap().0, EVENT_CANCELLED);
    tx.send((EVENT_CANCELLED.to_owned(), cancelled_args())).unwrap();
    deliver(&mut state, vec![online(8, 1)], 8, 1, |e, a| tx.send((e.to_owned(), a)).unwrap());
    assert_eq!(rx.try_recv().unwrap().0, EVENT_CANCELLED);
    assert_eq!(rx.try_recv().unwrap().0, EVENT_INITIATED);
}

#[tokio::test(start_paused = true)]
async fn wait_claims_due_once_and_a_stale_timer_cannot_claim_replacement() {
    let controller = controller();
    *controller.state.lock().unwrap() = State::default();
    schedule(&mut controller.state.lock().unwrap(), 2).unwrap();
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut waiter = std::pin::pin!(wait());
    assert!(waiter.as_mut().poll(&mut cx).is_pending());
    tokio::time::advance(Duration::from_secs(60)).await;
    schedule(&mut controller.state.lock().unwrap(), 5).unwrap();
    tokio::time::advance(Duration::from_secs(70)).await;
    assert!(waiter.as_mut().poll(&mut cx).is_pending(), "stale timer must not claim the replacement");
    tokio::time::advance(Duration::from_secs(230)).await;
    assert!(waiter.as_mut().poll(&mut cx).is_ready(), "due plan must resolve the waiter");
    assert!(controller.state.lock().unwrap().plan.unwrap().triggered);
    assert!(matches!(status(), Status::Triggered));
    *controller.state.lock().unwrap() = State::default();
}
