use super::*;
use crate::space::{actor::{lifecycle::ActorState, Job, SystemHandle}, System, World};
use std::sync::Arc;

#[tokio::test]
async fn names_survive_world_wrappers() {
    crate::tests::init_config();
    let db = crate::db::Db::unconnected_publication_fixture(([127, 0, 0, 1], 1).into());
    let world = World::new(db.clone(), Arc::new(crate::sde::Sde::default()), 0);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Job>();
    world.systems.lock().unwrap().insert(1, SystemHandle { tx, gate: Arc::default(), state: Arc::new(ActorState::default()) });
    let mut s = System::new(1, Arc::default(), 0);
    Job::new(|_| {}).run(&mut s);
    let (result, ()) = tokio::join!(world.with(1, |s| { s.id = 2; 7 }),
        async { rx.recv().await.unwrap().run(&mut s); });
    assert_eq!(result, Some(7));
    world.schedule(1, 0, |s| s.id = 3);
    rx.try_recv().unwrap().run(&mut s); let start = Instant::now(); s.tick(1_000);
    assert!(s.attribution.phase_ns.iter().sum::<u128>() <= start.elapsed().as_nanos());
    assert_eq!((s.id, s.attribution.timers.values().map(|v| v.recent.count).sum::<u64>(),
        s.attribution.jobs.values().map(|v| v.recent.count).sum::<u64>()), (3, 1, 3));
    assert!(s.attribution.jobs.keys().chain(s.attribution.timers.keys()).all(|name|
        name.contains("names_survive_world_wrappers") && !name.contains("Box<dyn") && !name.contains("World::schedule")));
    db.assert_publication_fixture_unused();
}

#[test]
fn lateness_clamps_and_ranking_is_deterministic() {
    let t = Instant::now();
    for (end, own) in [(None, 0_u128), (Some(t), 0), (Some(t + Duration::from_millis(30)), 30), (Some(t + Duration::from_millis(70)), 50)] {
        let (a, b) = split_lateness(t, t + Duration::from_millis(50), end);
        assert_eq!((a.as_millis(), (a + b).as_millis()), (own, 50));
    }
    assert_eq!(top::<2, _>([("b", 4), ("a", 4), ("c", 9)].into_iter()), [Some(("c", 9)), Some(("a", 4))]);
}

#[test]
fn limits_count_each_rejected_candidate_once() {
    let (mut local, mut suppressed) = (Limiter::<1>::default(), 0);
    let t = Instant::now();
    assert!(!admit_slow(t, &mut local, &mut suppressed, || false));
    assert!(!admit_slow(t, &mut local, &mut suppressed, || panic!("local limit must run first")));
    assert_eq!(suppressed, 2);
    let mut process = Limiter::<1>::default();
    assert!(process.allow(t, Duration::from_secs(1)));
    assert!(!process.allow(t, Duration::from_secs(1)));
    assert!(process.allow(t + Duration::from_secs(1), Duration::from_secs(1)));
}

#[test]
fn late_local_windows_are_merged_after_the_first_report() {
    let t = Instant::now();
    let (mut process, mut a, mut b) = (Process::default(), Local::default(), Local::default());
    (a.started, b.started) = (Some(t), Some(t));
    a.window.pilots_max = 1; a.window.record(Duration::from_millis(2), Duration::ZERO);
    a.work("warm", Work::Mailbox, 3); a.work("warm_timer", Work::Timer, 4); a.phase(Phase::Actions, 5);
    b.window.record(Duration::from_millis(8), Duration::ZERO);
    b.work("cold", Work::Mailbox, 6); b.work("cold_timer", Work::Timer, 7); b.phase(Phase::Drones, 8);
    assert_eq!(process.merge(t + Duration::from_secs(60), 1, &mut a).unwrap().window.ticks, 1);
    assert!(process.merge(t + Duration::from_secs(60), 2, &mut b).is_none());
    assert_eq!((process.window.ticks, process.window.mailbox.count, process.window.timers.count), (1, 1, 1));
    a.window.pilots_max = 1; a.window.record(Duration::from_millis(3), Duration::ZERO);
    a.work("warm2", Work::Mailbox, 9); a.work("warm2_timer", Work::Timer, 10); a.phase(Phase::Actions, 11);
    let s = process.merge(t + Duration::from_secs(120), 1, &mut a).unwrap();
    assert_eq!((s.window.ticks, s.window.duration.percentile(s.window.ticks, P99).as_nanos()), (2, 8_000_000));
    assert_eq!((s.window.mailbox.count, s.window.mailbox.total_ns, s.window.timers.count, s.window.timers.total_ns), (2, 15, 2, 17));
    assert!(s.top_mailbox.iter().chain(s.top_timers.iter()).flatten().all(|(n, _)| !n.starts_with("cold")));
    assert!(s.top_systems.iter().flatten().all(|(id, _, _)| *id != 2) && s.top_phases.iter().flatten().all(|(p, _)| *p != Phase::Drones));
    assert_eq!((a.window.ticks, b.window.ticks, a.window.mailbox.count, b.window.mailbox.count,
        a.window.timers.count, b.window.timers.count), (0, 0, 0, 0, 0, 0));
}
