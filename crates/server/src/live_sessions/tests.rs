use super::*;
use std::time::Duration;
use tokio::time::timeout;

fn contains(account_id: i64) -> bool {
    state().lock().unwrap().accounts.contains_key(&account_id)
}

async fn still_waiting(registration: &mut Registration) {
    assert!(timeout(Duration::from_millis(20), registration.wait_for_predecessor()).await.is_err());
}

#[tokio::test]
async fn replacement_cancels_and_waits_for_completion() {
    let account = -81001;
    let first = register(account, true).unwrap();
    let mut second = register(account, true).unwrap();
    assert!(first.is_cancelled());
    assert!(!second.is_cancelled());
    still_waiting(&mut second).await;
    drop(first);
    timeout(Duration::from_secs(1), second.wait_for_predecessor()).await.unwrap();
    drop(second);
    assert!(!contains(account));
}

#[tokio::test]
async fn pending_replacement_cannot_be_bypassed_even_when_dropped() {
    let account = -81002;
    let first = register(account, true).unwrap();
    let mut second = register(account, true).unwrap();
    let mut third = register(account, true).unwrap();
    assert!(second.is_cancelled());
    timeout(Duration::from_secs(1), second.cancelled()).await.unwrap();
    still_waiting(&mut second).await;
    still_waiting(&mut third).await;
    drop(second);
    still_waiting(&mut third).await;
    drop(first);
    timeout(Duration::from_secs(1), third.wait_for_predecessor()).await.unwrap();
    drop(third);
    assert!(!contains(account));
}

#[tokio::test]
async fn unrelated_accounts_do_not_wait_for_each_other() {
    let first = register(-81003, true).unwrap();
    let mut other = register(-81004, true).unwrap();
    timeout(Duration::from_secs(1), other.wait_for_predecessor()).await.unwrap();
    assert!(!first.is_cancelled());
    drop(first);
    drop(other);
    assert!(!contains(-81003));
    assert!(!contains(-81004));
}

#[tokio::test]
async fn old_drop_does_not_remove_newest_registration() {
    let account = -81005;
    let first = register(account, true).unwrap();
    let mut second = register(account, true).unwrap();
    drop(first);
    assert!(contains(account));
    timeout(Duration::from_secs(1), second.wait_for_predecessor()).await.unwrap();
    let mut third = register(account, true).unwrap();
    assert!(second.is_cancelled());
    still_waiting(&mut third).await;
    drop(second);
    assert!(contains(account));
    timeout(Duration::from_secs(1), third.wait_for_predecessor()).await.unwrap();
    drop(third);
    assert!(!contains(account));
}

#[tokio::test]
async fn dropped_newest_remains_a_barrier_until_predecessor_finishes() {
    let account = -81006;
    let first = register(account, true).unwrap();
    let second = register(account, true).unwrap();
    drop(second);
    assert!(contains(account));
    let mut third = register(account, true).unwrap();
    still_waiting(&mut third).await;
    drop(first);
    timeout(Duration::from_secs(1), third.wait_for_predecessor()).await.unwrap();
    drop(third);
    assert!(!contains(account));
}

#[test]
fn disabled_takeover_refuses_duplicates_without_cancelling() {
    let account = -81007;
    let first = register(account, false).unwrap();
    assert!(register(account, false).is_none());
    assert!(!first.is_cancelled());
    drop(first);
    assert!(!contains(account));
}

#[test]
fn disabled_takeover_respects_dropped_pending_barrier() {
    let account = -81008;
    let first = register(account, false).unwrap();
    let pending = register(account, true).unwrap();
    drop(pending);
    assert!(register(account, false).is_none());
    drop(first);
    let next = register(account, false).unwrap();
    drop(next);
    assert!(!contains(account));
}

#[test]
fn concurrent_disabled_takeovers_have_one_winner() {
    let account = -81009;
    let start = std::sync::Arc::new(std::sync::Barrier::new(3));
    let finish = std::sync::Arc::new(std::sync::Barrier::new(3));
    let threads: Vec<_> = (0..2).map(|_| {
        let start = start.clone();
        let finish = finish.clone();
        std::thread::spawn(move || {
            start.wait();
            let registration = register(account, false);
            finish.wait();
            registration.is_some()
        })
    }).collect();
    start.wait();
    finish.wait();
    assert_eq!(threads.into_iter().map(|thread| thread.join().unwrap()).filter(|won| *won).count(), 1);
    assert!(!contains(account));
}

#[tokio::test]
async fn sender_lifetime_and_disconnected_receiver() {
    let account = -81101;
    let registration = register(account, true).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let guard = registration.attach_sender(tx.clone()).unwrap();
    let attached = sender(account).unwrap();
    attached.send(("event".into(), vec![])).unwrap();
    assert_eq!(rx.recv().await.unwrap().0, "event");
    assert!(registration.attach_sender(tx.clone()).is_none());
    drop(guard);
    assert!(sender(account).is_none());
    drop(rx);
    assert!(attached.send(("event".into(), vec![])).is_err());
    drop(registration);
    assert!(sender(account).is_none());
}

#[tokio::test]
async fn sender_takeover_is_generation_safe() {
    let account = -81102;
    let first = register(account, true).unwrap();
    let (old_tx, mut old_rx) = tokio::sync::mpsc::unbounded_channel();
    let old_guard = first.attach_sender(old_tx.clone()).unwrap();
    let mut second = register(account, true).unwrap();
    let (new_tx, mut new_rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(second.attach_sender(new_tx.clone()).is_none());
    assert!(first.attach_sender(old_tx).is_none());
    sender(account).unwrap().send(("old".into(), vec![])).unwrap();
    assert_eq!(old_rx.recv().await.unwrap().0, "old");
    still_waiting(&mut second).await;
    drop(first);
    assert!(sender(account).is_none());
    second.wait_for_predecessor().await;
    let new_guard = second.attach_sender(new_tx).unwrap();
    drop(old_guard);
    sender(account).unwrap().send(("new".into(), vec![])).unwrap();
    assert_eq!(new_rx.recv().await.unwrap().0, "new");
    drop(second);
    assert!(sender(account).is_none());
    drop(new_guard);
    assert!(sender(account).is_none());
}

#[tokio::test]
async fn sender_takeover_after_old_guard_detaches() {
    let account = -81104;
    let first = register(account, true).unwrap();
    let (old_tx, _old_rx) = tokio::sync::mpsc::unbounded_channel();
    let old_guard = first.attach_sender(old_tx).unwrap();
    let mut second = register(account, true).unwrap();
    let (new_tx, mut new_rx) = tokio::sync::mpsc::unbounded_channel();
    drop(old_guard);
    assert!(sender(account).is_none());
    assert!(second.attach_sender(new_tx.clone()).is_none());
    drop(first);
    second.wait_for_predecessor().await;
    let _new_guard = second.attach_sender(new_tx).unwrap();
    sender(account).unwrap().send(("new".into(), vec![])).unwrap();
    assert_eq!(new_rx.recv().await.unwrap().0, "new");
}
