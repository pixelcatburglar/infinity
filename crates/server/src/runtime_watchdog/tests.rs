use super::*;

#[tokio::test]
async fn synchronous_poll_and_async_wait_are_distinguished() {
    let (output, busy) = measure(async {
        std::thread::sleep(Duration::from_millis(30));
        7
    }).await;
    assert_eq!((output, busy.polls), (7, 1));
    assert!(busy.slow() && busy.poll_max > SLOW_POLL && busy.total >= busy.poll_sum);
    assert_eq!(busy.poll_sum, busy.poll_max);
    let (_, waiting) = measure(tokio::time::sleep(Duration::from_millis(200))).await;
    assert!(waiting.polls >= 2 && waiting.poll_sum * 10 < waiting.total && waiting.poll_max < SLOW_POLL);
    let now = Instant::now();
    let mut window = RpcWindow::default();
    let light = Timing { total: Duration::from_millis(1), polls: 1,
        poll_sum: Duration::from_micros(10), poll_max: Duration::from_micros(10) };
    for id in 0..CAPACITY { let _ = window.record(now, (id.to_string(), "m".into()), light); }
    let _ = window.record(now, ("0".into(), "m".into()), light);
    assert_eq!(window.names[&("0".into(), "m".into())].poll_sum_ns, light.poll_sum.as_nanos() * 2);
    let heavy = Timing { total: Duration::from_secs(2), polls: 50, poll_sum: Duration::from_secs(1),
        poll_max: SLOW_POLL * 2 };
    for _ in 0..2 { let _ = window.record(now, ("extra".into(), "m".into()), heavy); }
    assert_eq!((window.names.len(), window.overflow_calls), (CAPACITY, 2));
    assert!(!window.names.contains_key(&("extra".into(), "m".into())));
    assert!(matches!(window.top(false)[0], Some((Name::Overflow("<overflow>"), _))));
    assert!(matches!(window.top(true)[0], Some((Name::Overflow("<overflow>"), _))));
    let (_, old) = window.record(now + WINDOW, ("next".into(), "m".into()), light).unwrap();
    assert_eq!((old.count, old.overflow_calls), (CAPACITY as u64 + 3, 2));
    assert_eq!(old.total.max, heavy.total);
    assert_eq!((window.count, window.names.len(), window.overflow_calls), (1, 1, 0));
    assert!(window.names.contains_key(&("next".into(), "m".into())));
}
