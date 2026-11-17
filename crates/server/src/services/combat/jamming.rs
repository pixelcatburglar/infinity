//! Runtime ECM lockout and actor-owned expiration.
//! `eve/client/script/environment/godma.py` and
//! `eve/client/script/ui/inflight/shipHud/shipUI.py` consume jam timers.
//! Records live in combat runtime State and expire on system actor timers.
//! State is memory-only, cleared when the victim leaves space, and not resumed
//! after restart.

use super::{state, State, NEXT_LOCK};
use crate::services::effects::notify;
use crate::space::System;
use marshal::Value;
use std::sync::atomic::Ordering;

pub(super) struct Jam {
    until: i64,
    recipients: [Option<i64>; 2],
    started: i64,
    generation: u64,
}

pub(super) fn jammed_in(st: &State, ship: i64, now_ms: i64) -> bool {
    st.jammed.get(&ship).is_some_and(|records| records.values().any(|record| record.until > now_ms))
}

pub(crate) fn is_jammed(ship: i64, now_ms: i64) -> bool {
    jammed_in(&state(), ship, now_ms)
}

pub(crate) fn clear(s: &mut System, ship: i64) {
    let records = state().jammed.remove(&ship);
    if let Some(records) = records {
        for ((source, module), record) in records {
            for pilot in record.recipients.into_iter().flatten() {
                let (event, args) = notify::jam(source, module, ship, false, record.started, 0.0);
                s.notify_pilot(pilot, event, args);
                let (event, args) = notify::ewar(source, module, ship, "electronic", false);
                s.notify_pilot(pilot, event, args);
            }
        }
    }
}

/// Apply a successful roll inside the victim's system actor.
pub(crate) fn apply(s: &mut System, source: i64, module: i64, target: i64, duration_ms: f64) {
    if !duration_ms.is_finite() || duration_ms <= 0.0 { return; }
    let Some(victim) = s.ball(target) else { return };
    let victim_pilot = victim.char_id;
    let source_pilot = s.ball(source).and_then(|ball| ball.char_id);
    let recipients = [source_pilot, victim_pilot.filter(|pilot| Some(*pilot) != source_pilot)];
    let now = s.now_ms();
    let started = crate::client::filetime_from_unix_ms(now);
    let generation = NEXT_LOCK.fetch_add(1, Ordering::Relaxed);
    let (lost, notices, until) = {
        let mut st = state();
        let records = st.jammed.entry(target).or_default();
        let until = now.saturating_add(duration_ms.ceil() as i64)
            .max(records.get(&(source, module)).map_or(now, |record| record.until));
        records.insert((source, module), Jam { until, generation, recipients, started });
        let (lost, notices) = if let Some(pilot) = victim_pilot {
            // Pending attempts publish through their own UI failure path and
            // never join the completed-lock loss set.
            super::targeting_charges::clear_character_lock_state(&mut st, pilot)
        } else { (Default::default(), Vec::new()) };
        (lost, notices, until)
    };
    s.drop_npc_targets(target);
    s.drone_jammed(target);
    if let Some(pilot) = victim_pilot {
        for locked in lost {
            s.remove_projections_between(target, locked);
            s.notify_pilot(pilot, "OnTarget", vec![Value::str("lost"), Value::Int(locked), Value::None]);
        }
    }
    for (ship, pilot) in notices {
        s.notify_pilot(pilot, "OnTarget", vec![Value::str("otherlost"), Value::Int(ship), Value::None]);
    }
    for pilot in recipients.into_iter().flatten() {
        let (event, args) = notify::jam(source, module, target, true, started, (until - now) as f64);
        s.notify_pilot(pilot, event, args);
        let (event, args) = notify::ewar(source, module, target, "electronic", true);
        s.notify_pilot(pilot, event, args);
    }
    s.schedule_at(until, move |s| {
        let expired = {
            let mut st = state();
            let Some(records) = st.jammed.get_mut(&target) else { return };
            if !records.get(&(source, module)).is_some_and(|record| record.generation == generation && record.until <= s.now_ms()) { return; }
            records.remove(&(source, module));
            if records.is_empty() { st.jammed.remove(&target); }
            true
        };
        if expired {
            for pilot in recipients.into_iter().flatten() {
                let (event, args) = notify::jam(source, module, target, false, started, 0.0);
                s.notify_pilot(pilot, event, args);
                let (event, args) = notify::ewar(source, module, target, "electronic", false);
                s.notify_pilot(pilot, event, args);
            }
        }
    });
}

#[cfg(all(test, feature = "tests-combat"))] mod tests;
