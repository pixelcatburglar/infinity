//! Arrival reference and once-only completion tests.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn references_survive_partial_release_and_completion_runs_once() {
    let mut system = System::new(1, Arc::default(), 0);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let id = system.register_warp_arrival(Box::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); }));
    assert!(system.retain_warp_arrival(id));
    assert!(system.retain_warp_arrival(id));
    system.release_warp_arrival(id);
    system.release_warp_arrival(id);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    system.complete_warp_arrival(id);
    system.complete_warp_arrival(id);
    system.release_warp_arrival(id);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!system.retain_warp_arrival(id));
    assert!(system.warp_arrivals.is_empty());
}

#[test]
fn last_release_drops_unconsumed_callback() {
    let mut system = System::new(1, Arc::default(), 0);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let id = system.register_warp_arrival(Box::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); }));
    system.release_warp_arrival(id);
    system.complete_warp_arrival(id);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(system.warp_arrivals.is_empty());
}

#[test]
fn producer_drop_releases_on_its_pinned_weak_mailbox() {
    let mut system = System::new(1, Arc::default(), 0);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let id = system.register_warp_arrival(Box::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); }));
    let (tx, mut rx) = mpsc::unbounded_channel::<actor::Job>();
    let guard = WarpArrivalGuard { id, sender: tx.downgrade() };
    drop(guard);
    rx.try_recv().unwrap()(&mut system);
    assert!(system.warp_arrivals.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(tx);
    assert!(rx.try_recv().is_err());
}

#[test]
fn identifiers_do_not_alias_replacement_actors() {
    let mut first = System::new(1, Arc::default(), 0);
    let old = first.register_warp_arrival(Box::new(|_| {}));
    let mut replacement = System::new(1, Arc::default(), 0);
    let new = replacement.register_warp_arrival(Box::new(|_| {}));
    assert_ne!(old, new);
    assert!(!replacement.retain_warp_arrival(old));
    replacement.release_warp_arrival(old);
    assert!(replacement.retain_warp_arrival(new));
    replacement.release_warp_arrival(new);
    replacement.release_warp_arrival(new);
    assert!(replacement.warp_arrivals.is_empty());
}

#[tokio::test]
async fn cancelled_registration_reply_releases_the_producer_reference() {
    let (tx, mut rx) = mpsc::unbounded_channel::<actor::Job>();
    let handle = actor::SystemHandle {
        tx,
        gate: Arc::new(tokio::sync::Mutex::new(())),
        written: Arc::new(AtomicU64::new(0)),
    };
    let mailbox = handle.tx.clone();
    let weak = handle.tx.downgrade();
    let (registered_tx, registered_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let actor = std::thread::spawn(move || {
        let mut system = System::new(1, Arc::default(), 0);
        rx.blocking_recv().unwrap()(&mut system);
        rx.blocking_recv().unwrap()(&mut system);
        system.warp_arrivals.is_empty()
    });
    let waiter = tokio::spawn(async move {
        handle.with(move |system| {
            let id = system.register_warp_arrival(Box::new(|_| panic!("cancelled callback ran")));
            let guard = WarpArrivalGuard { id, sender: weak };
            registered_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            guard
        }).await
    });
    registered_rx.await.unwrap();
    waiter.abort();
    let _ = waiter.await;
    resume_tx.send(()).unwrap();
    assert!(tokio::task::spawn_blocking(move || actor.join().unwrap()).await.unwrap());
    drop(mailbox);
}

#[test]
fn stopped_mailbox_does_not_keep_actor_or_callback_alive() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut system = System::new(1, Arc::default(), 0);
    let id = system.register_warp_arrival(Box::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); }));
    let (tx, rx) = mpsc::unbounded_channel::<actor::Job>();
    let guard = WarpArrivalGuard { id, sender: tx.downgrade() };
    drop(rx);
    drop(tx);
    drop(system);
    drop(guard);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cancelled_unpolled_registration_enqueues_nothing() {
    struct DropCount(Arc<AtomicUsize>);
    impl Drop for DropCount {
        fn drop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); }
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<actor::Job>();
    let handle = actor::SystemHandle {
        tx,
        gate: Arc::new(tokio::sync::Mutex::new(())),
        written: Arc::new(AtomicU64::new(0)),
    };
    let drops = Arc::new(AtomicUsize::new(0));
    let marker = DropCount(drops.clone());
    let callback: Timer = Box::new(move |_| drop(marker));
    let future = async move { handle.with(move |s| s.register_warp_arrival(callback)).await };
    drop(future);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(rx.try_recv().is_err());
}
