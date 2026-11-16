use super::*;
use crate::services::combat::NEXT_LOCK;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::combat::tests::remote_repair_db;
use crate::services::effects::{projection::Projection, EffectKind, JammingType};
use crate::services::fit::attributes::tests::ship_fit;
use crate::sde::types::tests::placement_fixture;
use crate::space::system::tests::events::{install_projection_sde, publish_scene_prefix};
use crate::space::{Pilots, System};
use infinity_core::consts::{GROUP_SUN, GROUP_WRECK};

/// Synthetic placement type carrying a group the fixture marks global.
const GLOBAL_TARGET_TYPE: i32 = 7;

type NotifyRx = tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>;

fn channel() -> (NotifyTx, NotifyRx) {
    tokio::sync::mpsc::unbounded_channel()
}
/// Remove only this case's global lock entries, including on panic.
struct LockGuard { character: i64, ship: i64 }

impl Drop for LockGuard {
    fn drop(&mut self) {
        let mut st = state();
        st.targets.remove(&self.character);
        st.locking.retain(|&(ch, _), _| ch != self.character);
        st.announced_locks.retain(|&(ch, _), _| ch != self.character);
        st.capacitor.remove(&self.ship);
    }
}

fn placed_ball(id: i64, owner: Option<i64>, radius: f32, x: f64) -> Ball {
    let mut ball = remote_repair_db::ball(id, 1, owner.unwrap_or(0));
    ball.char_id = owner;
    ball.radius = radius;
    ball.pos = [x, 0.0, 0.0];
    ball
}

/// Install a resident fit whose only distinguishing output is the fitted
/// max-target range.
fn install_range(system: &mut System, ship: i64, range: f64) {
    let fit = ship_fit(&[(ATTR_MAX_TARGET_RANGE, range)]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(ship, Condition { now: stats.max, stats, shield_at: filetime_now() }, fit);
}

struct LockScene {
    system: System,
    system_id: i64,
    sde: Arc<Sde>,
    character: i64,
    ship: i64,
    target: i64,
    target_pilot: i64,
    owner: NotifyTx,
    owner_rx: NotifyRx,
    ui_rx: NotifyRx,
    target_rx: NotifyRx,
    attempt: u64,
    _guard: LockGuard,
}

/// A real system holding a stopped source and target with a pending lock
/// attempt; `range` is the installed resident fitted max-target range.
fn lock_scene(source_radius: f32, target_radius: f32, center: f64, range: f64, global: Option<(i32, i32)>) -> LockScene {
    crate::tests::init_config();
    let base = NEXT_LOCK.fetch_add(10, Ordering::Relaxed) as i64;
    let (ship, character, target, target_pilot) = (base, base + 1, base + 2, base + 3);
    let system_id = base + 4;
    let registry = Arc::new(Pilots::default());
    let (owner, owner_rx) = channel();
    let (target_tx, target_rx) = channel();
    registry.register_pilot(character, owner.clone());
    registry.register_pilot(target_pilot, target_tx);
    let mut system = System::new(system_id, registry, 1000);
    let groups: Vec<(i32, Option<i32>)> = global.into_iter().map(|(type_id, group)| (type_id, Some(group))).collect();
    let sde = placement_fixture(&groups);
    install_projection_sde(&mut system, sde.clone());
    system.insert(placed_ball(ship, Some(character), source_radius, 0.0));
    let mut target_ball = placed_ball(target, Some(target_pilot), target_radius, center);
    if let Some((type_id, _)) = global { target_ball.type_id = type_id; target_ball.global = true; }
    system.insert(target_ball);
    install_range(&mut system, ship, range);
    let (ui, ui_rx) = channel();
    let attempt = NEXT_LOCK.fetch_add(1, Ordering::Relaxed);
    let begun = state().begin_lock(ship, character, target, attempt, system.now_ms(), Some(ui));
    assert!(begun);
    LockScene { system, system_id, sde, character, ship, target, target_pilot, owner, owner_rx, ui_rx, target_rx,
        attempt, _guard: LockGuard { character, ship } }
}

/// Commit one real fitting patch between the two given resident ranges and
/// drain the queued actor actions plus the owner's fit notification.
fn republish_range(f: &mut LockScene, from: f64, to: f64) {
    let before = ship_fit(&[(ATTR_MAX_TARGET_RANGE, from)]);
    let patch = ship_fit(&[(ATTR_MAX_TARGET_RANGE, to)]).into_committed_fitting(&before);
    let generation = f.system.persistent_generation(f.ship).unwrap();
    assert!(f.system.publish_fitting(f.ship, f.character, generation, patch, vec![]));
    assert_eq!(f.system.with_combat_fit(f.ship, |fit, _, _| fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE)), Some(to));
    publish_scene_prefix(&mut f.system, false);
    while let Ok((event, _)) = f.owner_rx.try_recv() {
        assert_eq!(event, "OnModuleAttributeChanges");
    }
}

/// The real admission request: the owner's notify handle and this case's UI channel.
fn request(f: &LockScene, ui_owner: Option<NotifyTx>) -> super::LockRequest {
    super::LockRequest { character: f.character, ship: f.ship, system: f.system_id, target: f.target,
        silent: false, owner: None, ui_owner, notify: f.owner.clone() }
}

/// Run the production completion callback with the incarnation tokens this
/// helper captures, the same ones the real timer captured at admission.
fn complete(f: &mut LockScene) {
    let request = request(f, None);
    let source = f.system.capture_ball(f.ship).unwrap();
    let target = f.system.capture_ball(f.target).unwrap();
    super::complete(&mut f.system, request, f.attempt, 5, source, target);
}

/// Turn the fixture's pending attempt into a completed lock, the precondition
/// the ongoing-loss cases need; `present` stands in for the admission path.
fn complete_state(f: &LockScene) {
    let result = state().complete_lock(f.ship, f.character, f.target, f.attempt, f.system.now_ms(), true);
    assert_eq!(result, (true, true));
    assert!(has_target(f.character, f.target));
}

fn expect_lock_refusal(f: &mut LockScene) {
    assert!(!has_target(f.character, f.target));
    assert_eq!(f.ui_rx.try_recv().unwrap(), crate::client::target_lock_failure(f.character, f.target, f.attempt));
    assert!(f.ui_rx.try_recv().is_err());
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
}

fn expect_add(f: &mut LockScene) {
    assert!(has_target(f.character, f.target));
    assert_eq!(f.owner_rx.try_recv().unwrap(), ("OnTarget".to_string(), vec![Value::str("add"), Value::Int(f.target)]));
    assert_eq!(f.target_rx.try_recv().unwrap(), ("OnTarget".to_string(), vec![Value::str("otheradd"), Value::Int(f.ship)]));
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
    assert!(f.ui_rx.try_recv().is_err());
}

/// Evaluate the real actor admission inside a scene built for the given
/// geometry and fitted range, not a copied distance formula.
fn admission(source_radius: f32, target_radius: f32, center: f64, range: f64) -> Result<(), CallError> {
    let mut f = lock_scene(source_radius, target_radius, center, range, None);
    admission_in_system(&mut f)
}

fn expect_range_refusal(result: Result<(), CallError>, gap: f64, range: f64) {
    let Err(CallError::User { msg, dict }) = result else { panic!("expected a lock range refusal") };
    assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".into(), Value::unicode(messages::target_lock_range(metres_to_km(gap), metres_to_km(range))))]);
}

/// Run the real actor admission after removing the fixture's artificial
/// pending attempt; a registered attempt is written back so the completion
/// driver stays bound to the actual attempt.
fn admission_in_system(f: &mut LockScene) -> Result<(), CallError> {
    state().locking.remove(&(f.character, f.target));
    let (ui, ui_rx) = channel();
    f.ui_rx = ui_rx;
    let request = request(f, Some(ui));
    let result = add_in_system(&mut f.system, f.sde.as_ref(), request);
    if let Some(lock) = state().locking.get(&(f.character, f.target)) { f.attempt = lock.attempt; }
    result.map(|_| ())
}

/// Real admission; returns the pending attempt's absolute actor deadline from
/// the same lock-time inputs production used.
fn admit(f: &mut LockScene) -> i64 {
    assert!(admission_in_system(f).is_ok());
    let target = f.system.ball(f.target).unwrap();
    let signature = attr(f.sde.as_ref(), target.type_id, ATTR_SIGNATURE_RADIUS, f64::from(target.radius));
    let scan = f.system.with_combat_fit(f.ship, |fit, _, _| fit.value(Key::Ship, ATTR_SCAN_RESOLUTION)).unwrap();
    f.system.now_ms() + i64::try_from(lock_time_ms(scan, signature)).unwrap_or(i64::MAX)
}

/// One fence-table row: admit a real pending attempt, apply the mutation,
/// advance to the deadline and expect only the original UI failure.
fn fenced(center: f64, range: f64, change: impl FnOnce(&mut LockScene)) {
    let mut f = lock_scene(10.0, 20.0, center, range, None);
    let due = admit(&mut f);
    change(&mut f);
    f.system.tick(due);
    expect_lock_refusal(&mut f);
}

#[test]
fn stationary_range_shrink_drops_on_existing_tick() {
    let mut f = lock_scene(10.0, 20.0, 130.0, 200.0, None);
    complete_state(&f);
    republish_range(&mut f, 200.0, 100.0);
    f.system.tick(1100);
    assert_eq!(f.system.current_time(), 1);
    assert!(!has_target(f.character, f.target));
    assert_eq!(f.owner_rx.try_recv().unwrap(), ("OnTarget".to_string(), vec![Value::str("lost"), Value::Int(f.target), Value::None]));
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
    f.system.tick(1200);
    assert!(f.owner_rx.try_recv().is_err());
    republish_range(&mut f, 100.0, 200.0);
    f.system.tick(1300);
    assert!(!has_target(f.character, f.target));
    assert!(f.owner_rx.try_recv().is_err());
}

#[test]
fn stationary_range_shrink_notifies_announced_target_once() {
    let mut f = lock_scene(10.0, 20.0, 130.0, 200.0, None);
    complete_state(&f);
    state().announced_locks.insert((f.character, f.target), (f.ship, f.target_pilot));
    republish_range(&mut f, 200.0, 100.0);
    f.system.tick(1100);
    assert!(!has_target(f.character, f.target));
    assert_eq!(f.owner_rx.try_recv().unwrap(), ("OnTarget".to_string(), vec![Value::str("lost"), Value::Int(f.target), Value::None]));
    assert_eq!(f.target_rx.try_recv().unwrap(), ("OnTarget".to_string(), vec![Value::str("otherlost"), Value::Int(f.ship), Value::None]));
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
    f.system.tick(1200);
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
}

#[test]
fn admission_counts_source_radius_in_surface_gap() {
    assert!(admission(10.0, 20.0, 129.0, 100.0).is_ok());
}

#[test]
fn admission_refuses_surface_equality() {
    expect_range_refusal(admission(0.0, 20.0, 120.0, 100.0), 100.0, 100.0);
}

#[test]
fn admission_refuses_overlap_without_range() {
    expect_range_refusal(admission(10.0, 20.0, 0.0, 0.0), 0.0, 0.0);
}

#[test]
fn admission_refuses_beyond_surface_range() {
    let Err(CallError::User { msg, .. }) = admission(10.0, 20.0, 131.0, 100.0) else { panic!("expected a lock range refusal") };
    assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
}

#[test]
fn completion_counts_source_radius_in_surface_gap() {
    let mut f = lock_scene(10.0, 20.0, 129.0, 100.0, None);
    complete(&mut f);
    expect_add(&mut f);
}

#[test]
fn completion_refuses_surface_equality() {
    let mut f = lock_scene(0.0, 20.0, 120.0, 100.0, None);
    complete(&mut f);
    expect_lock_refusal(&mut f);
}

#[test]
fn completion_uses_fresh_fitted_range_not_snapshot() {
    let mut f = lock_scene(10.0, 20.0, 130.0, 200.0, None);
    republish_range(&mut f, 200.0, 100.0);
    complete(&mut f);
    expect_lock_refusal(&mut f);
}

#[test]
fn completion_refuses_beyond_surface_range() {
    let mut f = lock_scene(10.0, 20.0, 131.0, 100.0, None);
    complete(&mut f);
    expect_lock_refusal(&mut f);
}

#[test]
fn admission_refuses_target_outside_scene() {
    let mut f = lock_scene(10.0, 20.0, 100_000_000.0, 200_000_000.0, None);
    assert!(!f.system.same_bubble(f.ship, f.target) && !f.system.visible_to(f.character, f.target));
    let Err(CallError::User { msg, .. }) = admission_in_system(&mut f) else { panic!("expected a targeting refusal") };
    assert_eq!(msg, messages::MSG_TARGETING_ATTEMPT_CANCELLED);
}

#[test]
fn cross_bubble_lock_drops_on_existing_tick() {
    let mut f = lock_scene(10.0, 20.0, 100_000_000.0, 200_000_000.0, None);
    assert!(!f.system.same_bubble(f.ship, f.target) && !f.system.visible_to(f.character, f.target));
    complete_state(&f);
    f.system.tick(1100);
    assert!(!has_target(f.character, f.target));
    assert_eq!(f.owner_rx.try_recv().unwrap(), ("OnTarget".to_string(), vec![Value::str("lost"), Value::Int(f.target), Value::None]));
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
}

#[test]
fn authorized_global_target_locks_across_bubbles() {
    let mut f = lock_scene(10.0, 20.0, 100_000_000.0, 200_000_000.0, Some((GLOBAL_TARGET_TYPE, GROUP_SUN)));
    assert!(!f.system.same_bubble(f.ship, f.target) && f.system.visible_to(f.character, f.target));
    assert!(admission_in_system(&mut f).is_ok());
    complete(&mut f);
    expect_add(&mut f);
    f.system.tick(1100);
    assert!(has_target(f.character, f.target));
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
}

#[test]
fn admitted_lock_completes_on_schedule_then_drops_after_shrink() {
    let mut f = lock_scene(10.0, 20.0, 130.0, 200.0, None);
    let due = admit(&mut f);
    let mid = (f.system.now_ms() + due) / 2;
    f.system.tick(mid);
    republish_range(&mut f, 200.0, 300.0);
    f.system.tick(due - 1);
    assert!(!has_target(f.character, f.target));
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
    assert!(f.ui_rx.try_recv().is_err());
    f.system.tick(due);
    expect_add(&mut f);
    republish_range(&mut f, 300.0, 100.0);
    f.system.tick(due + 1);
    assert!(!has_target(f.character, f.target));
    assert_eq!(f.owner_rx.try_recv().unwrap(), ("OnTarget".to_string(), vec![Value::str("lost"), Value::Int(f.target), Value::None]));
    assert_eq!(f.target_rx.try_recv().unwrap(), ("OnTarget".to_string(), vec![Value::str("otherlost"), Value::Int(f.ship), Value::None]));
    assert!(f.owner_rx.try_recv().is_err());
}

#[test]
fn pending_attempt_dies_when_its_admission_world_changes() {
    fenced(130.0, 200.0, |f| {
        f.system.insert(placed_ball(f.ship, Some(f.character), 10.0, 0.0));
        install_range(&mut f.system, f.ship, 200.0);
    });
    fenced(130.0, 200.0, |f| {
        f.system.insert(placed_ball(f.target, Some(f.target_pilot), 20.0, 130.0));
    });
    fenced(130.0, 200.0, |f| {
        f.system.command(f.target, |b, _, _| { b.pos = [231.0, 0.0, 0.0]; vec![] });
        publish_scene_prefix(&mut f.system, false);
        assert!(f.system.same_bubble(f.ship, f.target));
    });
    fenced(130.0, 200_000_000.0, |f| {
        f.system.command(f.target, |b, _, _| { b.pos = [100_000_000.0, 0.0, 0.0]; vec![] });
        publish_scene_prefix(&mut f.system, false);
        assert!(!f.system.visible_to(f.character, f.target));
    });
    fenced(130.0, 200.0, |f| { assert!(f.system.remove_ball(f.target).is_some()); });
    fenced(130.0, 200.0, |f| {
        f.system.command(f.target, |b, _, _| { b.cloak = 1; vec![] });
        publish_scene_prefix(&mut f.system, false);
        assert!(!f.system.visible_to(f.character, f.target));
    });
}

#[test]
fn stale_timer_neither_cancels_nor_completes_a_relock() {
    let mut f = lock_scene(10.0, 20.0, 130.0, 200.0, None);
    let old_due = admit(&mut f);
    let old_attempt = f.attempt;
    cancel_target(f.character, f.target);
    assert_eq!(f.ui_rx.try_recv().unwrap(), crate::client::target_lock_failure(f.character, f.target, old_attempt));
    f.system.tick(old_due - 1);
    let new_due = admit(&mut f);
    assert_ne!(f.attempt, old_attempt);
    f.system.tick(old_due);
    assert!(state().locking.get(&(f.character, f.target)).is_some_and(|lock| lock.attempt == f.attempt));
    assert!(!has_target(f.character, f.target));
    assert!(f.ui_rx.try_recv().is_err());
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
    f.system.tick(new_due);
    expect_add(&mut f);
}

#[test]
fn commanded_motion_holds_then_loses_lock_at_strict_boundary() {
    let mut f = lock_scene(10.0, 20.0, 130.0, 200.0, None);
    complete_state(&f);
    for (center, kept) in [(229.0, true), (230.0, false)] {
        f.system.command(f.target, move |ball, _, _| { ball.pos = [center, 0.0, 0.0]; vec![] });
        publish_scene_prefix(&mut f.system, false);
        assert!(f.system.same_bubble(f.ship, f.target));
        f.system.tick(f.system.now_ms() + 100);
        assert_eq!(has_target(f.character, f.target), kept);
    }
    assert_eq!(f.owner_rx.try_recv().unwrap(), ("OnTarget".to_string(), vec![Value::str("lost"), Value::Int(f.target), Value::None]));
    f.system.tick(f.system.now_ms() + 100);
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
}

#[test]
fn global_targets_still_need_scene_range_not_the_flag() {
    let mut equal = lock_scene(10.0, 20.0, 100_000_000.0, 99_999_970.0, Some((GLOBAL_TARGET_TYPE, GROUP_SUN)));
    assert!(equal.system.visible_to(equal.character, equal.target));
    expect_range_refusal(admission_in_system(&mut equal), 99_999_970.0, 99_999_970.0);
    let mut wreck = lock_scene(10.0, 20.0, 100_000_000.0, 200_000_000.0, Some((GLOBAL_TARGET_TYPE, GROUP_WRECK)));
    assert!(!wreck.system.visible_to(wreck.character, wreck.target));
    let Err(CallError::User { msg, .. }) = admission_in_system(&mut wreck) else { panic!("expected a targeting refusal") };
    assert_eq!(msg, messages::MSG_TARGETING_ATTEMPT_CANCELLED);
}

#[test]
fn tick_sweep_removes_only_failed_locks_and_their_projections() {
    let mut f = lock_scene(10.0, 20.0, 130.0, 200.0, None);
    complete_state(&f);
    let (kept, pending) = (f.target + 3, f.target + 4);
    f.system.insert(placed_ball(kept, None, 20.0, 50.0));
    f.system.insert(placed_ball(pending, None, 20.0, 50.0));
    {
        let mut st = state();
        st.targets.entry(f.character).or_default().insert(kept);
        st.capacitor.insert(f.ship, (123.0, 456));
        assert!(st.begin_lock(f.ship, f.character, pending, NEXT_LOCK.fetch_add(1, Ordering::Relaxed), f.system.now_ms(), None));
    }
    let mut g = lock_scene(10.0, 20.0, 50.0, 200.0, None);
    complete_state(&g);
    f.system.upsert_projection(f.target, Projection { source_ship: f.ship, source_module: f.target + 5, source_type: 1, effect_id: 1,
        kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: i64::MAX, values: vec![], charge_values: vec![], charge_type: None, scramble: None });
    republish_range(&mut f, 200.0, 100.0);
    f.system.tick(1100);
    assert!(!has_target(f.character, f.target));
    assert!(has_target(f.character, kept));
    assert!(state().locking.contains_key(&(f.character, pending)));
    assert_eq!(state().capacitor.get(&f.ship), Some(&(123.0, 456)));
    assert!(f.system.projections_on(f.target).is_empty());
    assert!(has_target(g.character, g.target));
    assert_eq!(f.owner_rx.try_recv().unwrap(), ("OnTarget".to_string(), vec![Value::str("lost"), Value::Int(f.target), Value::None]));
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
    assert!(g.owner_rx.try_recv().is_err());
    assert!(g.target_rx.try_recv().is_err());
}

/// A range failure that is still queued when an internal successor is admitted
/// must still deliver the original UI answer after that successor fails.
#[test]
fn range_failure_with_internal_successor_keeps_original_ui_answer() {
    let mut f = lock_scene(10.0, 20.0, 130.0, 200.0, None);
    let due = admit(&mut f);
    let delay = due - f.system.now_ms();
    let failed_attempt = f.attempt;
    republish_range(&mut f, 200.0, 100.0);
    f.system.tick(due);
    assert!(!state().locking.contains_key(&(f.character, f.target)));
    assert!(!has_target(f.character, f.target));
    republish_range(&mut f, 100.0, 200.0);
    let internal = request(&f, None);
    assert!(add_in_system(&mut f.system, f.sde.as_ref(), internal).is_ok());
    let successor = state().locking[&(f.character, f.target)].attempt;
    assert_ne!(successor, failed_attempt);
    assert!(state().locking[&(f.character, f.target)].ui_owner.is_none());
    let failure = f.ui_rx.try_recv().expect("original UI failure is queued");
    assert_eq!(failure, crate::client::target_lock_failure(f.character, f.target, failed_attempt));
    let failed = crate::client::TargetLockFailure {
        character: f.character,
        target: f.target,
        attempt: failed_attempt,
    };
    let mut emitted = 0;
    let delivered = with_pending_failure_current(failed, || emitted += 1);
    assert_eq!(state().locking[&(f.character, f.target)].attempt, successor);
    republish_range(&mut f, 200.0, 100.0);
    f.system.tick(f.system.now_ms() + delay);
    assert!(!state().locking.contains_key(&(f.character, f.target)));
    assert!(!has_target(f.character, f.target));
    assert!(f.ui_rx.try_recv().is_err());
    assert!(f.owner_rx.try_recv().is_err());
    assert!(f.target_rx.try_recv().is_err());
    assert!(delivered, "original UI failure suppressed by internal successor");
    assert_eq!(emitted, 1);
}

/// A UI request that dedupes onto an internal pending attempt keeps that
/// attempt's identity; a queued failure from the older attempt stays dropped.
#[test]
fn internal_successor_adopted_by_ui_keeps_its_attempt() {
    let mut f = lock_scene(10.0, 20.0, 130.0, 200.0, None);
    let old_attempt = f.attempt;
    state().locking.remove(&(f.character, f.target));
    let internal = request(&f, None);
    assert!(add_in_system(&mut f.system, f.sde.as_ref(), internal).is_ok());
    let attempt = state().locking[&(f.character, f.target)].attempt;
    assert_ne!(attempt, old_attempt);
    let (ui, ui_rx) = channel();
    f.ui_rx = ui_rx;
    let adopted = request(&f, Some(ui));
    assert!(add_in_system(&mut f.system, f.sde.as_ref(), adopted).is_ok());
    assert_eq!(state().locking[&(f.character, f.target)].attempt, attempt);
    assert!(state().locking[&(f.character, f.target)].ui_owner.is_some());
    let dedupe = request(&f, None);
    assert!(add_in_system(&mut f.system, f.sde.as_ref(), dedupe).is_ok());
    assert!(state().locking[&(f.character, f.target)].ui_owner.is_some());
    let stale = crate::client::TargetLockFailure {
        character: f.character,
        target: f.target,
        attempt: old_attempt,
    };
    assert!(!with_pending_failure_current(stale, || panic!("stale failure emitted")));
    assert!(f.ui_rx.try_recv().is_err());
    assert_eq!(state().locking[&(f.character, f.target)].attempt, attempt);
}
