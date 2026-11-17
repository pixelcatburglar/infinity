use std::future::Future;
use super::*;

#[test]
fn window_summary_uses_elapsed_time_and_bucket_bounds() {
    let (mut stats, t0) = (Stats::default(), Instant::now());
    let (a, b) = (Location::caller(), Location::caller());
    assert!(stats.on_acquire(t0).is_none());
    stats.record(a, Duration::from_millis(2), Duration::from_secs(90));
    stats.record(b, Duration::from_millis(8), Duration::from_secs(30));
    assert!(stats.on_acquire(t0 + Duration::from_secs(59)).is_none());
    let s = stats.on_acquire(t0 + Duration::from_secs(180)).unwrap();
    assert_eq!((s.count, s.window, s.hold_total), (2, Duration::from_secs(180), Duration::from_secs(120)));
    assert_eq!((s.wait_p50, s.wait_p99, s.wait_max), (Duration::from_millis(2), Duration::from_millis(8), Duration::from_millis(8)));
    assert_eq!((s.hold_p50, s.hold_p99, s.hold_max), (Duration::from_secs(30), Duration::from_secs(90), Duration::from_secs(90)));
    assert_eq!(s.utilization(), 2.0 / 3.0);
    let top: Vec<_> = s.top_sites.iter().flatten().collect();
    assert!(s.top_sites_complete && top.len() == 2);
    assert_eq!((top[0].location, top[0].hold), (a, Duration::from_secs(90)));
    assert_eq!((top[1].location, top[1].hold), (b, Duration::from_secs(30)));
    assert_eq!((stats.count, stats.hold_total, stats.start), (0, Duration::ZERO, Some(t0 + Duration::from_secs(180))));
}

#[tokio::test(start_paused = true)]
async fn hold_starts_at_acquisition_and_ends_at_drop() {
    let gate: &tokio::sync::Mutex<()> = Box::leak(Box::new(tokio::sync::Mutex::new(())));
    let stats: &Mutex<Stats> = Box::leak(Box::new(Mutex::new(Stats::default())));
    let blocker = gate.lock().await;
    let mut pending = std::pin::pin!(acquire(gate, stats, Location::caller()));
    tokio::time::advance(Duration::from_secs(7)).await;
    assert!(pending.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())).is_pending());
    tokio::time::advance(Duration::from_secs(11)).await;
    let guard = { drop(blocker); pending.await };
    assert_eq!(stats.lock().unwrap().count, 0);
    tokio::time::advance(Duration::from_secs(3)).await;
    drop(guard);
    let s = stats.lock().unwrap();
    assert_eq!((s.count, s.wait.max, s.hold.max, s.hold_total), (1, Duration::from_secs(11), Duration::from_secs(3), Duration::from_secs(3)));
    assert!(gate.try_lock().is_ok());
}

#[tokio::test(start_paused = true)]
async fn overdue_holder_reports_acquisition_site_until_release() {
    static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    static STATS: std::sync::OnceLock<Mutex<Stats>> = std::sync::OnceLock::new();
    let (gate, stats) = (&GATE, STATS.get_or_init(Default::default));
    let site = Location::caller();
    let guard = acquire(gate, stats, site).await;
    tokio::time::advance(HOLDER_REPORT_PERIOD).await;
    assert!(stats.lock().unwrap().overdue(Instant::now()).is_none());
    tokio::time::advance(Duration::from_millis(1)).await;
    let (reported, held) = stats.lock().unwrap().overdue(Instant::now()).unwrap();
    assert!(std::ptr::eq(reported, site) && held > HOLDER_REPORT_PERIOD);
    assert_eq!((reported.file(), reported.line()), (site.file(), site.line()));
    tokio::time::advance(HOLDER_REPORT_PERIOD - Duration::from_millis(1)).await;
    assert!(stats.lock().unwrap().overdue(Instant::now()).is_none());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(stats.lock().unwrap().overdue(Instant::now()).unwrap().0, site);
    drop(guard);
    assert!(gate.try_lock().is_ok());
    assert!(stats.lock().unwrap().overdue(Instant::now()).is_none());
    let next_site = Location::caller();
    let next_guard = acquire(gate, stats, next_site).await;
    assert!(stats.lock().unwrap().overdue(Instant::now()).is_none());
    tokio::time::advance(HOLDER_REPORT_PERIOD + Duration::from_millis(1)).await;
    assert_eq!(stats.lock().unwrap().overdue(Instant::now()).unwrap().0, next_site);
    drop(next_guard);
    assert!(stats.lock().unwrap().overdue(Instant::now()).is_none());
}
