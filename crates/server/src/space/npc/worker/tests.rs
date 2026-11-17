use super::*;

pub(in crate::space) fn controlled_probes() -> (
    Probes,
    mpsc::UnboundedReceiver<oneshot::Sender<bool>>,
    mpsc::UnboundedReceiver<oneshot::Sender<bool>>,
) {
    let (effects, effects_requests) = mpsc::unbounded_channel();
    let (deferred, deferred_requests) = mpsc::unbounded_channel();
    (Probes { effects, deferred }, effects_requests, deferred_requests)
}

#[tokio::test]
async fn probes_remain_responsive_and_workers_drain() {
    let mut system = System::default();
    let Workers::Controlled { probes, tasks } = start(&mut system, true) else { panic!("expected controlled workers") };
    assert_eq!(probes.busy_reason().await.unwrap(), None);
    let (deferred_release, deferred_rx) = oneshot::channel();
    system.deferred_jobs.as_ref().unwrap().send(Box::pin(async move {
        let _ = deferred_rx.await;
    })).ok().unwrap();
    let probe = || tokio::time::timeout(std::time::Duration::from_secs(1), probes.busy_reason());
    assert_eq!(probe().await.unwrap().unwrap(), Some("deferred_worker_busy"));
    let (effect_started, effect_started_rx) = oneshot::channel();
    let (effect_release, effect_rx) = oneshot::channel();
    system.effect_jobs.as_ref().unwrap().send(Box::pin(async move {
        let _ = effect_started.send(()); let _ = effect_rx.await;
    })).ok().unwrap();
    effect_started_rx.await.unwrap();
    assert_eq!(probe().await.unwrap().unwrap(), Some("npc_effects_worker_busy"));
    effect_release.send(()).unwrap();
    drop(system);
    deferred_release.send(()).unwrap();
    assert!(tasks.join().await);
}
