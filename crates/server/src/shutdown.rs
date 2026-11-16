//! In-process planned shutdown, driven by the GM `/shutdown` command.
//! The main loop awaits `wait()` beside the OS-signal future so a due plan takes
//! the one existing teardown path; `rejecting` refuses new logins inside the
//! countdown window and `catch_up` re-notifies a session that selected a
//! character after the broadcast. Broadcast, cancel and catch-up all enqueue on
//! the client's notify channel, keeping one FIFO. The broadcasts mirror the
//! client's `/EVE-Universe/ClusterBroadcast` localization entries (messageIDs
//! 280382 and 281154). The plan is memory-only: a restart starts without one,
//! and nothing reads tables, scheduler jobs or polling.

use crate::client::filetime_now;
use crate::space::World;
use infinity_core::consts::{FILETIME_MINUTE, SECONDS_PER_MINUTE};
use infinity_core::session::OnlineSession;
use marshal::Value;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

/// Client localization path of the initiated broadcast (messageID 280382).
const INITIATED_LABEL: &str = "/EVE-Universe/ClusterBroadcast/UnscheduledDowntime";
/// Client localization path of the cancelled broadcast (messageID 281154).
const CANCELLED_LABEL: &str = "/EVE-Universe/ClusterBroadcast/ShutdownCancelled";
/// `OnClusterShutdownInitiated` takes a maintenance duration in minutes; this
/// command promises none.
const SHUTDOWN_DURATION_MINUTES: i64 = 0;
const EVENT_INITIATED: &str = "OnClusterShutdownInitiated";
const EVENT_CANCELLED: &str = "OnClusterShutdownCancelled";

/// One scheduled deadline; `triggered` stays until teardown so due always wins
/// over a late cancel or reschedule.
#[derive(Clone, Copy)]
struct Plan { when: i64, deadline: Instant, triggered: bool }

#[derive(Default)]
struct State {
    plan: Option<Plan>,
    /// Bumped on every replacement so a stale timer never claims a new plan.
    generation: u64,
    /// (character, session) pairs already told about the current plan; cleared
    /// on replacement or cancel and pruned to live sessions on each catch-up.
    delivered: HashSet<(i64, i64)>,
}

#[derive(Default)]
struct Controller { state: Mutex<State>, wake: Notify }
static CONTROLLER: OnceLock<Controller> = OnceLock::new();
fn controller() -> &'static Controller { CONTROLLER.get_or_init(Controller::default) }
#[derive(Debug)]
pub(crate) enum StartError { Invalid, Due }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CancelResult { None, Cancelled, Due }
#[derive(Debug)]
pub(crate) enum Status { None, Remaining(Duration), Triggered }

fn initiated_args(when: i64) -> Vec<Value> {
    vec![Value::str(INITIATED_LABEL), Value::Int(when), Value::Int(SHUTDOWN_DURATION_MINUTES)]
}
fn cancelled_args() -> Vec<Value> { vec![Value::str(CANCELLED_LABEL)] }

/// Both clocks are sampled inside the state lock; a failure leaves the old plan.
fn schedule(state: &mut State, minutes: i64) -> Result<i64, StartError> {
    if minutes <= 0 { return Err(StartError::Invalid); }
    let now = Instant::now();
    let when = filetime_now().checked_add(minutes.checked_mul(FILETIME_MINUTE).ok_or(StartError::Invalid)?)
        .ok_or(StartError::Invalid)?;
    let deadline = minutes.checked_mul(SECONDS_PER_MINUTE).and_then(|s| u64::try_from(s).ok())
        .and_then(|s| now.checked_add(Duration::from_secs(s))).ok_or(StartError::Invalid)?;
    if state.plan.as_ref().is_some_and(|p| p.triggered || now >= p.deadline) { return Err(StartError::Due); }
    state.plan = Some(Plan { when, deadline, triggered: false });
    state.generation += 1;
    state.delivered.clear();
    Ok(when)
}

/// Mark every live session delivered and enqueue the initiated broadcast, all
/// inside the state lock so channel order follows lock order.
fn broadcast(state: &mut State, online: Vec<OnlineSession>, mut notify: impl FnMut(i64, &'static str, Vec<Value>)) {
    let Some(when) = state.plan.as_ref().map(|plan| plan.when) else { return };
    for session in online {
        state.delivered.insert((session.character_id, session.session_id));
        notify(session.character_id, EVENT_INITIATED, initiated_args(when));
    }
}

/// Only `now < deadline` may cancel; a due or triggered plan wins.
fn cancel_locked(state: &mut State) -> CancelResult {
    let now = Instant::now();
    match state.plan {
        Some(plan) if plan.triggered || now >= plan.deadline => CancelResult::Due,
        Some(_) => { state.plan = None; state.delivered.clear(); CancelResult::Cancelled }
        None => CancelResult::None,
    }
}

/// Reaching the deadline inside `status` claims the plan triggered once.
fn status_locked(state: &mut State) -> Status {
    let now = Instant::now();
    match &mut state.plan {
        Some(plan) if plan.triggered || now >= plan.deadline => { plan.triggered = true; Status::Triggered }
        Some(plan) => Status::Remaining(plan.deadline - now),
        None => Status::None,
    }
}

/// `duration_since` saturates to zero, covering triggered, due and the window.
fn rejecting_locked(state: &State, reject_secs: u64) -> bool {
    match &state.plan {
        Some(plan) => plan.triggered || plan.deadline.duration_since(Instant::now()) <= Duration::from_secs(reject_secs),
        None => false,
    }
}

/// Prune `delivered` to live sessions, then enqueue the broadcast once per
/// (character, session) for one late selection.
fn deliver(state: &mut State, online: Vec<OnlineSession>, char_id: i64, session_id: i64, send: impl FnOnce(&'static str, Vec<Value>)) {
    let Some(when) = state.plan.as_ref().filter(|plan| !plan.triggered).map(|plan| plan.when) else { return };
    let live: HashSet<(i64, i64)> = online.into_iter().map(|s| (s.character_id, s.session_id)).collect();
    state.delivered.retain(|key| live.contains(key));
    if state.delivered.insert((char_id, session_id)) { send(EVENT_INITIATED, initiated_args(when)); }
}

/// Schedule or replace the plan and broadcast to every online session.
pub(crate) fn initiate(world: &World, minutes: i64) -> Result<(), StartError> {
    if minutes <= 0 { return Err(StartError::Invalid); }
    let controller = controller();
    let mut state = crate::poison::lock_or_exit(controller.state.lock());
    schedule(&mut state, minutes)?;
    broadcast(&mut state, world.online_sessions(), |id, event, args| world.notify_pilot(id, event, args));
    drop(state);
    controller.wake.notify_waiters();
    Ok(())
}

/// Cancel a live plan and broadcast; idempotent with no plan. A due plan wins.
pub(crate) fn cancel(world: &World) -> CancelResult {
    let controller = controller();
    let mut state = crate::poison::lock_or_exit(controller.state.lock());
    let result = cancel_locked(&mut state);
    if result == CancelResult::Cancelled {
        for session in world.online_sessions() {
            world.notify_pilot(session.character_id, EVENT_CANCELLED, cancelled_args());
        }
    }
    drop(state);
    if result == CancelResult::Cancelled { controller.wake.notify_waiters(); }
    result
}

pub(crate) fn status() -> Status {
    let controller = controller();
    let mut state = crate::poison::lock_or_exit(controller.state.lock());
    let status = status_locked(&mut state);
    drop(state);
    if matches!(status, Status::Triggered) { controller.wake.notify_waiters(); }
    status
}

pub(crate) fn rejecting(reject_secs: u64) -> bool {
    rejecting_locked(&*crate::poison::lock_or_exit(controller().state.lock()), reject_secs)
}

/// After the final session change, enqueue the broadcast on the caller's own
/// notify channel if this session was missed by the snapshot.
pub(crate) fn catch_up(world: &World, char_id: i64, session_id: i64, send: impl FnOnce(&'static str, Vec<Value>)) {
    let mut state = crate::poison::lock_or_exit(controller().state.lock());
    if !matches!(&state.plan, Some(plan) if !plan.triggered) { return; }
    deliver(&mut state, world.online_sessions(), char_id, session_id, send);
}

/// Sleep until the plan is due or replaced; the timer claims `triggered` once
/// for the exact generation and deadline it slept on, else it re-reads state.
pub(crate) async fn wait() {
    let controller = controller();
    loop {
        // Register the wakeup before reading state so a change between the
        // read and the wait cannot be missed.
        let notified = controller.wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let next = {
            let mut state = crate::poison::lock_or_exit(controller.state.lock());
            let now = Instant::now();
            match &mut state.plan {
                Some(plan) if plan.triggered => return,
                Some(plan) if now >= plan.deadline => { plan.triggered = true; return; }
                Some(plan) => Some((plan.deadline, state.generation)),
                None => None,
            }
        };
        let Some((deadline, generation)) = next else {
            notified.await;
            continue;
        };
        tokio::select! {
            () = &mut notified => {}
            () = tokio::time::sleep_until(deadline) => {
                let mut state = crate::poison::lock_or_exit(controller.state.lock());
                if state.generation == generation && Instant::now() >= deadline {
                    if let Some(plan) = &mut state.plan {
                        if plan.deadline == deadline {
                            plan.triggered = true;
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "tests-runtime"))]
mod tests;
