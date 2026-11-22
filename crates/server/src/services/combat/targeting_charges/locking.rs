//! Lock range checks, delayed lock completion and the per-tick drop of
//! completed locks for the parent's targeting path; holds no state.
use {super::*, crate::space::System};

/// The owned inputs one lock admission carries into the actor turn and its timer.
pub(super) struct LockRequest {
    pub(super) character: i64,
    pub(super) ship: i64,
    pub(super) system: i64,
    pub(super) target: i64,
    pub(super) silent: bool,
    pub(super) owner: Option<(i64, Arc<AtomicBool>)>,
    pub(super) ui_owner: Option<NotifyTx>,
    pub(super) notify: NotifyTx,
}

/// No current authority to decide, or a surface gap at or beyond the fresh range.
enum RangeRefusal {
    Unavailable,
    OutOfRange { gap: f64, max_range: f64 },
}

/// The one scene-and-range gate for admission, completion and the sweep: the
/// current pilot ship, a distinct visible target, sampled gap strictly inside
/// `max_range` read fresh in this same actor turn.
fn check(s: &System, character: i64, ship: i64, target: i64, max_range: f64) -> Result<(), RangeRefusal> {
    let Some((source, target_ball)) = s.ball(ship).zip(s.ball(target)) else { return Err(RangeRefusal::Unavailable) };
    if s.pilot_ball(character).is_none_or(|pilot| pilot.id != ship) || ship == target || !s.visible_to(character, target) {
        return Err(RangeRefusal::Unavailable);
    }
    let Some((from, to)) = s.sample_position(ship).zip(s.sample_position(target)) else { return Err(RangeRefusal::Unavailable) };
    let gap = (dist(from, to) - f64::from(source.radius) - f64::from(target_ball.radius)).max(0.0);
    if gap < max_range { Ok(()) } else { Err(RangeRefusal::OutOfRange { gap, max_range }) }
}

/// One actor turn of admission: the existing cloak rules, the shared predicate
/// against fresh fitted limits, then pending registration and the timer.
pub(super) fn add_in_system(s: &mut System, sde: &Sde, request: LockRequest) -> CallResult {
    let LockRequest { character, ship, system, target, silent, owner, ui_owner, notify } = request;
    if target == ship || s.ball(target).is_none_or(|b| b.cloak != 0)
        || s.ball(ship).is_none_or(|b| b.char_id != Some(character) || (b.cloak != 0 && b.cloak != crate::space::CLOAK_RESTORE)) {
        return Err(user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![]));
    }
    let Some((range, pilot_limit, ship_limit, scan_resolution)) = s.with_combat_fit(ship, |fit, _, _|
        (fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE), fit.value(Key::Char, ATTR_MAX_LOCKED_TARGETS),
        fit.value(Key::Ship, ATTR_MAX_LOCKED_TARGETS), fit.value(Key::Ship, ATTR_SCAN_RESOLUTION)))
    else { return Err(CallError::Internal(anyhow::anyhow!("no resident combat fit for ship {ship}"))) };
    match check(s, character, ship, target, range) {
        Err(RangeRefusal::OutOfRange { gap, max_range }) => {
            let text = messages::target_lock_range(metres_to_km(gap), metres_to_km(max_range));
            return Err(user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(text))]));
        }
        Err(RangeRefusal::Unavailable) => return Err(user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![])),
        Ok(()) => {}
    }
    if jamming::is_jammed(ship, s.now_ms()) { return Err(user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![])); }
    let max_targets = fitted_lock_limit(pilot_limit, ship_limit);
    let (target_type, target_radius) = s.ball(target).map(|b| (b.type_id, f64::from(b.radius))).unwrap_or((0, 0.0));
    let delay = lock_time_ms(scan_resolution, attr(sde, target_type, ATTR_SIGNATURE_RADIUS, target_radius));
    let attempt = NEXT_LOCK.fetch_add(1, Ordering::Relaxed);
    let Some((source, target_token)) = s.capture_ball(ship).zip(s.capture_ball(target)) else {
        return Err(user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![]));
    };
    let current = {
        let mut st = state();
        if !lock_owner_valid(&st, owner.as_ref()) {
            return Err(user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![]));
        }
        let current: Vec<_> = st.targets.get(&character).map(|targets| targets.iter().copied().collect()).unwrap_or_default();
        if let Some(lock) = st.locking.get_mut(&(character, target)) {
            if let Some(sender) = ui_owner { lock.ui_owner = Some(sender); }
            return Ok(Value::tuple([Value::Bool(true), Value::list(current.into_iter().map(Value::Int))]));
        }
        if current.contains(&target) {
            return Ok(Value::tuple([Value::Bool(false), Value::list(current.into_iter().map(Value::Int))]));
        }
        if current.len() >= max_targets.max(1) {
            return Err(user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(messages::target_limit(max_targets.max(1))))]));
        }
        if !st.begin_lock(ship, character, target, attempt, s.now_ms(), ui_owner) {
            return Err(user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![]));
        }
        current
    };
    s.drop_jump_cloak(ship);
    debug!("locking {target} in {delay}ms");
    let completion = LockRequest { character, ship, system, target, silent, owner, ui_owner: None, notify };
    s.schedule(delay, move |s| complete(s, completion, attempt, source, target_token));
    Ok(Value::tuple([Value::Bool(true), Value::list(current.into_iter().map(Value::Int))]))
}

/// The timer's one-shot completion: incarnation tokens, cloak, jam and the
/// shared predicate against the range and lock limit fitted now must all still hold.
fn complete(s: &mut System, request: LockRequest, attempt: u64, source: crate::space::system::observers::BallToken, target: crate::space::system::observers::BallToken) {
    let valid = s.ball_token_current(source) && s.ball_token_current(target)
        && !jamming::is_jammed(request.ship, s.now_ms())
        && s.ball(request.ship).is_some_and(|b| b.cloak == 0)
        && s.ball(request.target).is_some_and(|b| b.cloak == 0)
        && s.combat_phase(request.target).is_none_or(|phase| !matches!(phase, crate::space::combat::CombatPhase::Transition { .. }))
        && s.with_combat_fit(request.ship, |fit, _, _| fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE))
            .is_some_and(|range| check(s, request.character, request.ship, request.target, range).is_ok());
    let limit = s.with_combat_fit(request.ship, |fit, _, _|
        fitted_lock_limit(fit.value(Key::Char, ATTR_MAX_LOCKED_TARGETS), fit.value(Key::Ship, ATTR_MAX_LOCKED_TARGETS)))
        .unwrap_or(1);
    let pilot = s.ball(request.target).and_then(|ball| ball.char_id);
    let (_, added) = {
        let mut st = state();
        let result = finish_lock(&mut st, request.ship, request.character, request.target, attempt, s.now_ms(), valid, limit, request.owner.as_ref());
        if result.1 && !request.silent {
            if let Some(pilot) = pilot { st.announced_locks.insert((request.character, request.target), (request.ship, pilot)); }
        }
        result
    };
    if added {
        if let Some(char_id) = pilot {
            crate::progress::publish_live(s.progress_registry(), &crate::lua::events::GameEvent::Targeted { char_id, by_ball_id: request.ship, system_id: request.system });
        }
        notify_lock(request.target, request.ship, request.silent,
            |args| { let _ = request.notify.send(("OnTarget".to_string(), args)); },
            |args| { if let Some(pilot) = pilot { s.notify_pilot(pilot, "OnTarget", args); } });
    }
}

/// The lock count admission and completion both read from the current fit.
/// A positive pilot limit caps the ship; a non-positive one leaves the ship
/// limit, and the count is never below one.
fn fitted_lock_limit(pilot_limit: f64, ship_limit: f64) -> usize {
    let limit = if pilot_limit > 0.0 { ship_limit.min(pilot_limit) } else { ship_limit };
    (limit as usize).max(1)
}

/// Remove one sweep-invalidated completed lock: conditional State removal
/// consumes the exact announced entry, then matching projections and payloads.
fn drop_completed(s: &mut System, character: i64, ship: i64, target: i64) {
    let announced = {
        let mut st = state();
        if !st.targets.get_mut(&character).is_some_and(|set| set.remove(&target)) { return; }
        st.announced_locks.remove(&(character, target))
    };
    remove_target_projections_in_system(s, ship, &[target]);
    s.notify_pilot(character, "OnTarget", vec![Value::str("lost"), Value::Int(target), Value::None]);
    if let Some((source, pilot)) = announced {
        s.notify_pilot(pilot, "OnTarget", vec![Value::str("otherlost"), Value::Int(source), Value::None]);
    }
}

/// The existing-tick sweep of this system's completed player locks; pending
/// attempts keep their own deadlines and other systems are never enumerated.
pub(crate) fn drop_invalid(s: &mut System) {
    let mut candidates = std::mem::take(&mut s.player_lock_candidates);
    {
        let st = state();
        for ball in s.pilots() {
            let Some(character) = ball.char_id else { continue };
            candidates.extend(st.targets.get(&character).into_iter().flatten().map(|&target| (character, ball.id, target)));
        }
    }
    let mut index = 0;
    while index < candidates.len() {
        let (character, ship, _) = candidates[index];
        let range = s.with_combat_fit(ship, |fit, _, _| fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE));
        while index < candidates.len() && candidates[index].0 == character {
            let target = candidates[index].2;
            index += 1;
            if !range.is_some_and(|range| check(s, character, ship, target, range).is_ok()) {
                drop_completed(s, character, ship, target);
            }
        }
    }
    candidates.clear();
    s.player_lock_candidates = candidates;
}
#[cfg(all(test, feature = "tests-combat"))]
mod tests;
