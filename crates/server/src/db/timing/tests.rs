use super::*;

/// A distinct source identity per textual call site, column included.
#[track_caller]
fn at() -> &'static Location<'static> { Location::caller() }

#[test]
fn warn_threshold_is_strict() {
    let (mut stats, site, t0) = (Stats::default(), at(), Instant::now());
    let ns = Duration::from_nanos(1);
    assert!(stats.record_at(t0, Kind::Checkout, site, CHECKOUT_WARN).1.is_none());
    assert!(stats.record_at(t0, Kind::Checkout, site, CHECKOUT_WARN + ns).1.is_some());
    assert!(stats.record_at(t0, Kind::Statement, site, STATEMENT_WARN).1.is_none());
    assert!(stats.record_at(t0, Kind::Commit, site, COMMIT_WARN + ns).1.is_some());
    assert!(stats.record_at(t0, Kind::Transaction, site, TRANSACTION_WARN - ns).1.is_none());
}

#[test]
fn window_rolls_at_sixty_seconds_and_boundary_sample_opens_the_next() {
    let (mut stats, site, t0) = (Stats::default(), at(), Instant::now());
    for kind in Kind::ALL { stats.record_at(t0, kind, site, Duration::from_millis(1)); }
    assert!(stats.record_at(t0 + Duration::from_secs(59), Kind::Statement, site, Duration::from_millis(1)).0.is_none());
    let (summary, _) = stats.record_at(t0 + WINDOW, Kind::Checkout, site, Duration::from_millis(1));
    let summary = summary.unwrap();
    assert_eq!(summary.window, WINDOW);
    let k = &summary.kinds;
    assert_eq!([k[0].count, k[1].count, k[2].count, k[3].count], [1, 2, 1, 1]);
    assert_eq!((stats.start, stats.kinds[0].count), (Some(t0 + WINDOW), 1));
}

#[test]
fn site_kind_warn_throttle_persists_across_windows() {
    let (mut stats, site, t0) = (Stats::default(), at(), Instant::now());
    let slow = TRANSACTION_WARN + Duration::from_nanos(1);
    assert!(stats.record_at(t0, Kind::Transaction, site, slow).1.is_some());
    assert!(stats.record_at(t0 + Duration::from_secs(5), Kind::Transaction, site, slow).1.is_none());
    // A different kind at the same site has its own table and throttle.
    assert!(stats.record_at(t0 + Duration::from_secs(5), Kind::Statement, site, slow).1.is_some());
    assert!(stats.record_at(t0 + WARN_PERIOD, Kind::Transaction, site, slow).1.is_some());
    assert!(stats.record_at(t0 + Duration::from_secs(55), Kind::Transaction, site, slow).1.is_some());
    // The rollover sample would warn again if the throttle reset with counts.
    let (summary, warn) = stats.record_at(t0 + WINDOW, Kind::Transaction, site, slow);
    assert!(summary.is_some() && warn.is_none());
}

#[test]
fn site_table_tracks_512_identities_then_overflows() {
    let (mut stats, t0) = (Stats::default(), Instant::now());
    let sites = [
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
        at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(), at(),
    ];
    for site in sites { stats.record_at(t0, Kind::Statement, site, Duration::from_millis(1)); }
    let kind = Kind::Statement.index();
    assert_eq!(stats.kinds[kind].sites.occupied, SITES_CAPACITY);
    stats.record_at(t0, Kind::Statement, sites[0], Duration::from_millis(9));
    stats.record_at(t0, Kind::Statement, sites[1], Duration::from_millis(5));
    stats.record_at(t0, Kind::Statement, at(), Duration::from_secs(3));
    let k = &stats.kinds[kind];
    assert_eq!((k.count, k.overflow_count, k.overflow_total, k.overflow_max),
        (SITES_CAPACITY as u64 + 3, 1, Duration::from_secs(3), Duration::from_secs(3)));
    let (summary, _) = stats.record_at(t0 + WINDOW, Kind::Checkout, sites[0], Duration::ZERO);
    let statement = &summary.unwrap().kinds[kind];
    assert_eq!((statement.count, statement.overflow_count), (SITES_CAPACITY as u64 + 3, 1));
    let top: Vec<_> = statement.top.iter().flatten().collect();
    assert_eq!((top[0].location, top[0].count, top[0].total), (sites[0], 2, Duration::from_millis(10)));
    assert_eq!(top[1].location, sites[1]);
}

mod integration;
