//! Admission gate and failure-history behaviour with deterministic times.
use super::*;

#[test]
fn global_and_per_address_limits_release_on_drop() {
    let gate = Admission::new(2, 1);
    let a = gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(1u32))).unwrap();
    assert!(gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(1u32))).is_none(), "per-address bound");
    let b = gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(2u32))).unwrap();
    assert!(gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(3u32))).is_none(), "global bound");
    drop(b);
    assert!(gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(3u32))).is_some(), "released slot reusable");
    drop(a);
    assert!(gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(1u32))).is_some());
}

#[test]
fn sliding_window_cooldown_resets_and_leaves_others() {
    let gate = Admission::new(4, 4);
    let policy = ServerConfig { login_failure_threshold: 2, ..Default::default() };
    let now = Instant::now();
    let permit = gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(1u32))).unwrap();
    for _ in 0..policy.login_failure_threshold - 1 {
        permit.record_failure_with_clock(&policy, || now);
    }
    assert!(!permit.blocked_with_clock(&policy, || now), "below threshold stays open");
    let other = gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(2u32))).unwrap();
    assert!(!other.blocked_with_clock(&policy, || now), "other sources unaffected");
    permit.record_failure_with_clock(&policy, || now + Duration::from_secs(1));
    assert!(permit.blocked_with_clock(&policy, || now + Duration::from_secs(1)), "threshold starts cooldown");
    let after = now + Duration::from_secs(policy.login_failure_cooldown_secs + 2);
    assert!(!permit.blocked_with_clock(&policy, || after), "cooldown expired");
    permit.record_failure_with_clock(&policy, || after);
    let boundary = after + Duration::from_secs(policy.login_failure_window_secs);
    permit.record_failure_with_clock(&policy, || boundary);
    assert!(!permit.blocked_with_clock(&policy, || boundary), "a failure at the window edge expires");
}

#[test]
fn full_history_evicts_oldest_source_and_keeps_cooldowns() {
    let gate = Admission::new(MAX_FAILURE_ADDRESSES + 2, MAX_FAILURE_ADDRESSES + 2);
    let policy = ServerConfig::default();
    let now = Instant::now();
    let cooled = gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(0u32))).unwrap();
    for _ in 0..policy.login_failure_threshold {
        cooled.record_failure_with_clock(&policy, || now);
    }
    let oldest = gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(1u32))).unwrap();
    for _ in 0..policy.login_failure_threshold - 1 {
        oldest.record_failure_with_clock(&policy, || now + Duration::from_secs(1));
    }
    for n in 2..MAX_FAILURE_ADDRESSES as u32 {
        gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(n))).unwrap().record_failure_with_clock(&policy, || now + Duration::from_secs(2));
    }
    gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(u32::MAX))).unwrap().record_failure_with_clock(&policy, || now + Duration::from_secs(3));
    assert!(cooled.blocked_with_clock(&policy, || now + Duration::from_secs(3)), "eviction keeps cooldowns");
    for _ in 0..policy.login_failure_threshold - 1 {
        oldest.record_failure_with_clock(&policy, || now + Duration::from_secs(4));
    }
    assert!(!oldest.blocked_with_clock(&policy, || now + Duration::from_secs(4)), "an evicted source restarts empty");
}

#[test]
fn a_table_of_cooldowns_fails_open() {
    let gate = Admission::new(MAX_FAILURE_ADDRESSES + 1, MAX_FAILURE_ADDRESSES + 1);
    let policy = ServerConfig::default();
    let now = Instant::now();
    for n in 0..MAX_FAILURE_ADDRESSES as u32 {
        let permit = gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(n))).unwrap();
        for _ in 0..policy.login_failure_threshold {
            permit.record_failure_with_clock(&policy, || now);
        }
    }
    let newcomer = gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(u32::MAX))).unwrap();
    newcomer.record_failure_with_clock(&policy, || now);
    assert!(!newcomer.blocked_with_clock(&policy, || now), "fail-open keeps the credential check");
}

#[test]
fn failure_clocks_follow_the_state_lock() {
    let gate = Admission::new(3, 1);
    let policy = ServerConfig { login_failure_threshold: 3, ..Default::default() };
    let base = Instant::now();
    let first = gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(1u32))).unwrap();
    let other = gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(2u32))).unwrap();
    let interleaved = std::cell::Cell::new(false);
    first.record_failure_with_clock(&policy, || {
        if let Ok(state) = gate.state.try_lock() {
            drop(state);
            first.record_failure_with_clock(&policy, || base + Duration::from_secs(1));
            interleaved.set(true);
        }
        base
    });
    if !interleaved.get() { first.record_failure_with_clock(&policy, || base + Duration::from_secs(1)); }
    other.record_failure_with_clock(&policy, || base + Duration::from_secs(1));
    let boundary = base + Duration::from_secs(60);
    first.record_failure_with_clock(&policy, || boundary);
    assert!(!first.blocked_with_clock(&policy, || boundary), "an expired failure must not start cooldown");
    for n in 3..=MAX_FAILURE_ADDRESSES as u32 {
        gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(n))).unwrap().record_failure_with_clock(&policy, || boundary);
    }
    gate.try_acquire(IpAddr::V4(std::net::Ipv4Addr::from(u32::MAX))).unwrap().record_failure_with_clock(&policy, || boundary);
    let state = gate.state.lock().unwrap();
    assert!(state.failures.contains_key(&first.address));
    assert!(!state.failures.contains_key(&other.address));
}
