//! Applies typed damage and repairs to actor-owned fitted health. Nonlethal
//! observations follow the actor mutation; player loss facts and notifications
//! commit atomically through the ship loss service. Live health is checkpointed
//! by the system snapshot worker. Flat, Breacher and DOT damage are unsupported.
//! `apply_damage_in_actor` is the shared in-actor entry; `apply_damage_guarded`
//! wraps it with an admission guard for callers outside the actor.
//! `apply_area_damage_in_actor` is the shared in-actor batch for one sampled
//! victim set; wrappers outside the actor run their admission guard first.
use super::*;
use crate::services::effects::online::OnlineChanged;
use crate::services::ship::loss::{destroy_ship_in_transaction, medical, publish_loss, LossInput, LossOutcome};

#[derive(Clone, Debug, Default)]
pub(crate) struct DamageSource {
    pub attacker_id: Option<i64>,
    pub attacker_ball_id: Option<i64>,
    pub source_ship_id: Option<i64>,
    pub source_ship_type: Option<i32>,
    pub weapon_type_id: Option<i32>,
    pub facts: Option<crate::space::combat_facts::CombatSource>,
    pub force_origin: Option<crate::lua::events::resources::FactOrigin>,
    pub illegal: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct DamageOutcome {
    pub dealt: f64,
    pub lethal: bool,
}
fn fact_refusal(system: i64, refusal: crate::space::combat_facts::Refusal) -> Option<CallError> {
    use crate::space::combat_facts::Refusal;
    match refusal {
        Refusal::Full => Some(super::capacity::refusal(system)),
        Refusal::Replay | Refusal::Busy => None,
        Refusal::Invalid | Refusal::Overflow =>
            Some(CallError::Internal(anyhow::anyhow!("combat fact refusal: {refusal:?}"))),
    }
}
/// The result of one in-actor area batch: `attempt_finished` marks that
/// `apply_area_damage` ran and finished the source attempt on every path it
/// takes; `fatal` is a refusal that aborts the batch; `first_error` keeps the
/// first per-target completion failure while the rest still apply. Loss
/// receipts belong to the caller's worker and must be awaited outside the
/// actor.
pub(crate) struct AreaCompletion {
    pub(crate) attempt_finished: bool,
    pub(crate) fatal: Option<CallError>,
    pub(crate) receipts: Vec<tokio::sync::oneshot::Receiver<Result<(), CallError>>>,
    pub(crate) first_error: Option<CallError>,
}

/// Shared in-actor area apply/finish for one sampled victim batch
/// `(target, pilot, membership, illegal, report)`. `valid_source` admits the
/// batch; a declined batch clears `victims` so no hit report is emitted.
/// `application` selects each accepted victim's raw vector: a fixed typed
/// vector for every victim, the Vorton missile formula sampled per target,
/// or a fitted weapon signature scale.
/// Damageable projectiles in range take the projectile path after source
/// admission and never enter ship plans. The caller keeps ownership of any
/// attempt token: this helper finishes the `facts` attempt only through
/// `admit_area_source`/`apply_area_damage`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_area_damage_in_actor(
    s: &mut crate::space::System, db: &Db, system: i64, weapon: i32,
    victims: &mut Vec<(i64, Option<i64>, Option<u64>, bool, Option<(i64, f64)>)>,
    application: crate::services::effects::aoe::DamageApplication,
    facts: crate::space::combat_facts::CombatSource,
    sde: &crate::sde::Sde, valid_source: bool,
) -> AreaCompletion {
    use crate::services::effects::aoe::DamageApplication;
    let ship = facts.entity.item_id;
    let attacker = facts.actual_actor_character;
    let mut completion = AreaCompletion { attempt_finished: false, fatal: None, receipts: Vec::new(), first_error: None };
    if !valid_source {
        victims.clear();
        return completion;
    }
    if let Err(error) = s.observe_combat_utc() {
        completion.fatal = Some(match error {
            crate::space::combat_facts::Refusal::Full => super::capacity::refusal(system),
            other => CallError::Internal(anyhow::anyhow!("area boundary refusal: {other:?}")),
        });
        return completion;
    }
    let at = s.combat_observed_time();
    victims.sort_unstable_by_key(|entry| entry.0);
    let mut targets = Vec::with_capacity(victims.len());
    let mut projectiles = Vec::new();
    for (target, pilot, membership, illegal, report) in victims.iter_mut() {
        let (target, pilot, membership, illegal) = (*target, *pilot, *membership, *illegal);
        if s.projectile_damageable(target) {
            let base = match application {
                DamageApplication::Fixed(raw) => raw,
                DamageApplication::Vorton(inputs) => inputs.raw,
                DamageApplication::Signature { raw, .. } => raw,
            };
            projectiles.push((target, base));
            continue;
        }
        if !s.ball(target).is_some_and(|ball| ball.char_id == pilot)
            || s.combat_membership_generation(target) != membership { continue; }
        if s.is_damage_immune(target) { continue; }
        let raw = match application {
            DamageApplication::Fixed(raw) => {
                *report = Some((crate::services::effects::weapons::hit_quality(1.0), 0.0));
                raw
            }
            DamageApplication::Vorton(inputs) => {
                let Some(victim) = s.sampled_ball(target) else { continue };
                *report = Some((crate::services::effects::weapons::hit_quality(1.0), 0.0));
                crate::services::effects::vorton::vorton_application(inputs.raw,
                    crate::services::effects::weapons::target_signature(sde, &victim),
                    crate::space::dist(victim.vel, [0.0; 3]), inputs.aoe_cloud, inputs.aoe_velocity, inputs.drf)
            }
            DamageApplication::Signature { raw, signature_radius } => {
                let factor = if signature_radius > 0.0 {
                    let Some(victim) = s.sampled_ball(target) else { continue };
                    (crate::services::effects::weapons::target_signature(sde, &victim) / signature_radius).clamp(0.0, 1.0)
                } else { 1.0 };
                if factor <= 0.0 { continue; }
                *report = Some((crate::services::effects::weapons::hit_quality(1.0), 0.0));
                raw.map(|value| value * factor)
            }
        };
        let source = DamageSource { attacker_id: attacker, attacker_ball_id: Some(ship),
            source_ship_id: Some(ship), source_ship_type: Some(facts.entity.type_id),
            weapon_type_id: Some(weapon), facts: Some(facts), force_origin: None, illegal };
        match s.plan_area_damage(target, raw, source, at) {
            Some(plan) => targets.push(plan),
            None if matches!(s.combat_phase(target), Some(crate::space::combat::CombatPhase::Live)) => {
                completion.fatal = Some(CallError::Internal(anyhow::anyhow!("area damage plan failed for live target")));
                return completion;
            }
            None => { *report = None; }
        }
    }
    targets.sort_unstable_by_key(|target| (target.target.item_id, target.target.incarnation));
    let applied = if targets.is_empty() { s.admit_area_source(facts, &mut [], at, |_| valid_source) }
        else { s.apply_area_damage(facts, &mut targets, at, |_| valid_source) };
    completion.attempt_finished = true;
    match applied {
        Ok(false) => return completion,
        Err(crate::space::combat_facts::Refusal::Full) => {
            completion.fatal = Some(super::capacity::refusal(system));
            return completion;
        }
        Err(crate::space::combat_facts::Refusal::Replay | crate::space::combat_facts::Refusal::Busy) => {
            victims.clear();
            return completion;
        }
        Err(error) => {
            completion.fatal = Some(CallError::Internal(anyhow::anyhow!("area combat refusal: {error:?}")));
            return completion;
        }
        Ok(true) => {}
    }
    for (target, projectile_raw) in projectiles { s.apply_projectile_damage(target, projectile_raw); }
    for target in &mut targets {
        let Some(outcome) = target.payload.outcome.take() else { continue };
        if let Ok(index) = victims.binary_search_by_key(&target.target.item_id, |entry| entry.0) {
            if let Some((_, dealt)) = victims[index].4.as_mut() { *dealt = outcome.dealt; }
        }
        match complete_damage_outcome(s, db, system, target.target.item_id, outcome) {
            Ok((_, Some(receipt))) => completion.receipts.push(receipt),
            Ok((_, None)) => {},
            Err(error) => if completion.first_error.is_none() { completion.first_error = Some(error); },
        }
    }
    completion
}




/// Shared in-actor damage entry: validates the typed raw values and sampled
/// target position, applies the damage and completes the outcome in one actor
/// turn. Early refusals finish the source attempt here; once `s.apply_damage`
/// is reached it finishes the attempt on every return, so this helper never
/// finishes twice. A returned loss receipt belongs to the caller's worker and
/// must be awaited outside the actor.
pub(crate) fn apply_damage_in_actor(
    s: &mut crate::space::System, db: &Db, system: i64, target: i64,
    raw: [f64; 4], source: DamageSource, force_loss: bool,
) -> Result<Option<(DamageOutcome, Option<tokio::sync::oneshot::Receiver<Result<(), CallError>>>)>, CallError> {
    if raw.iter().any(|v| !v.is_finite() || *v < 0.0) {
        if let Some(facts) = source.facts { s.finish_combat_attempt(&facts); }
        return Err(CallError::Internal(anyhow::anyhow!("invalid typed damage")));
    }
    if s.sample_position(target).is_some_and(|pos| !pos.iter().all(|v| v.is_finite())) {
        if let Some(facts) = source.facts { s.finish_combat_attempt(&facts); }
        return Err(CallError::Internal(anyhow::anyhow!("non-finite combat target position")));
    }
    let outcome = match s.apply_damage(target, raw, source, force_loss) {
        Ok(outcome) => outcome,
        Err(refusal) => return match fact_refusal(system, refusal) {
            Some(error) => Err(error),
            None => Ok(None),
        },
    };
    let Some(outcome) = outcome else {
        if s.ball(target).is_some() && s.combat_membership_generation(target).is_none() {
            return Err(CallError::Internal(anyhow::anyhow!("combat target condition unavailable")));
        }
        return Ok(None);
    };
    complete_damage_outcome(s, db, system, target, outcome).map(Some)
}

/// Guarded entry for callers outside the actor: the guard runs first inside
/// the actor turn, then `apply_damage_in_actor` performs validation, damage
/// and completion. The guard only admits; a committed hit is not reverted by
/// later state changes.
pub(crate) async fn apply_damage_guarded(
    db: &Db, world: &World, system: i64, target: i64, raw: [f64; 4],
    source: DamageSource, force_loss: bool,
    guard: impl FnOnce(&crate::space::System) -> bool + Send + 'static,
) -> Result<Option<DamageOutcome>, CallError> {
    let db = db.clone();
    let result = world.with(system, move |s| {
        if !guard(s) {
            if let Some(facts) = source.facts { s.finish_combat_attempt(&facts); }
            return Ok(None);
        }
        apply_damage_in_actor(s, &db, system, target, raw, source, force_loss)
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat actor unavailable")))??;
    let Some((outcome, receipt)) = result else { return Ok(None) };
    if let Some(receipt) = receipt {
        receipt.await.map_err(|_| CallError::Internal(anyhow::anyhow!("combat loss worker stopped")))??;
    }
    Ok(Some(outcome))
}
pub(crate) fn complete_damage_outcome(
    s: &mut crate::space::System, db: &Db, system: i64, target: i64,
    outcome: crate::space::combat::ActorDamageOutcome,
) -> Result<(DamageOutcome, Option<tokio::sync::oneshot::Receiver<Result<(), CallError>>>), CallError> {
        let result = DamageOutcome { dealt: outcome.dealt, lethal: outcome.lethal };
        if let Some(snapshot) = outcome.first_lethal {
            let Some(world) = s.combat_world() else {
                s.restore_known_rollback(target, snapshot.token, snapshot.generation);
                return Err(CallError::Internal(anyhow::anyhow!("combat world unavailable")));
            };
            let (tx, rx) = tokio::sync::oneshot::channel();
            let plan = snapshot.clone();
            let npc = snapshot.is_npc;
            let db = db.clone();
            let new = outcome.new;
            if !s.try_defer(async move {
                let result = if npc {
                    super::groups_health::settle_npc_loss(db, world, system, plan, new).await
                } else {
                    settle_loss(db, world, system, plan, new).await
                };
                let _ = tx.send(result);
            }) {
                s.restore_known_rollback(target, snapshot.token, snapshot.generation);
                return Err(CallError::Internal(anyhow::anyhow!("combat worker unavailable")));
            }
            if outcome.dealt > 0.0 {
                if let (Some(char_id), Some(attacker_ball_id)) = (snapshot.identity, outcome.source.attacker_ball_id) {
                    crate::progress::publish_live(s.progress_registry(), &crate::lua::events::GameEvent::Damaged { char_id, ship_id: target, attacker_ball_id, system_id: system });
                }
                npc_combat_contact(s, &outcome.source, snapshot.is_npc, snapshot.identity);
            }
            return Ok((result, Some(rx)));
        }
        if outcome.dealt > 0.0 {
            if let Some(attacker) = outcome.source.attacker_id {
                if s.is_npc(target) || s.drone(target).is_some() { s.add_aggressor(target, attacker); }
            }
            if let Some((max, _)) = s.combat_health_scalars(target) {
                let health = [0, 1, 2].map(|i| if max[i] > 0.0 { (outcome.new[i] / max[i]).clamp(0.0, 1.0) } else { 0.0 });
                s.set_health(target, health);
                if let Some(char_id) = s.ball(target).and_then(|ball| ball.char_id) {
                    let changes = health_changes(char_id, target,
                        [outcome.new[0], max[1] - outcome.new[1], max[2] - outcome.new[2]],
                        [outcome.old[0], max[1] - outcome.old[1], max[2] - outcome.old[2]]);
                    s.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
                    if let Some(attacker_ball_id) = outcome.source.attacker_ball_id {
                        crate::progress::publish_live(s.progress_registry(), &crate::lua::events::GameEvent::Damaged { char_id, ship_id: target, attacker_ball_id, system_id: system });
                    }
                }
            }
            npc_combat_contact(s, &outcome.source, s.is_npc(target), s.ball(target).and_then(|ball| ball.char_id));
        }
        Ok((result, None))
}

/// Report an accepted real hit to the pilot's crimewatch NPC timer
/// (`timers::npc_contact` does the mutation and pilot notice inside this
/// actor turn). The victim's character is contacted when an explicitly NPC
/// source dealt damage to a player; the attacking pilot is contacted when
/// damage landed on an explicit NPC target, preferring the facts' actual
/// actor so player drones keep their owner's attribution. Forced-loss
/// origins, refused and zero damage never count, and without the explicit
/// NPC source flag no NPC is guessed.
fn npc_combat_contact(s: &crate::space::System, source: &DamageSource, target_is_npc: bool, victim: Option<i64>) {
    if source.force_origin.is_some() { return; }
    let facts = source.facts;
    let source_is_npc = facts.is_some_and(|f| f.source_is_npc);
    if !source_is_npc && !target_is_npc { return; }
    let at = filetime_now();
    if source_is_npc {
        if let Some(character) = victim {
            crate::services::crimewatch::timers::npc_contact(s, character, at);
        }
    }
    // Only a non-NPC source can make contact on an NPC target: an explicitly
    // NPC source fighting an NPC must never mint a phantom player timer off
    // the legacy attacker attribution.
    if target_is_npc && !source_is_npc {
        if let Some(character) = facts.and_then(|f| f.actual_actor_character).or(source.attacker_id) {
            crate::services::crimewatch::timers::npc_contact(s, character, at);
        }
    }
}


fn loss_identity(system: i64, snapshot: &crate::space::combat::FirstLethalSnapshot) -> Option<Identity> {
    snapshot.identity.map(|char_id| Identity {
        char_id, ship_id: Some(snapshot.item_id), corp_id: snapshot.corp_id,
        station_id: None, system_id: Some(system),
    })
}

async fn settle_loss(
    db: Db, world: Arc<World>, system: i64, snapshot: Arc<crate::space::combat::FirstLethalSnapshot>, new: [f64; 3],
) -> Result<(), CallError> {
    let _market = super::super::market::lock().await;
    let identity = loss_identity(system, &snapshot);
    let sde = world.sde().clone();
    // Only a real piloted capsule-group hull captures the owner's online fences,
    // and only before any gate; ordinary and unpiloted losses keep the source
    // system gate alone.
    let piloted_capsule = identity.is_some() && snapshot.kind.is_some_and(|kind| medical::is_capsule(&sde, kind));
    let fences = if piloted_capsule {
        Some(crate::services::effects::online::capture_online_fences(&world, &[identity.unwrap().char_id]).await)
    } else { None };
    // Ordinary and unpiloted losses keep the single source gate; a real capsule
    // additionally gates every fence-membership system in ascending order.
    let mut _gates = Vec::new();
    let _gate;
    if let Some(fences) = &fences {
        _gate = None;
        let mut systems: Vec<i64> = fences.systems().collect();
        systems.push(system);
        systems.sort_unstable();
        systems.dedup();
        for id in systems {
            match world.lock_condition_snapshot(id).await {
                Ok(gate) => _gates.push(gate),
                Err(error) => {
                    release_loss(&world, system, &snapshot).await;
                    return Err(db_err(error));
                }
            }
        }
    } else {
        _gate = Some(match world.lock_condition_snapshot(system).await {
            Ok(gate) => gate,
            Err(error) => {
                release_loss(&world, system, &snapshot).await;
                return Err(db_err(error));
            }
        });
    }
    let (ship, token, generation) = (snapshot.item_id, snapshot.token, snapshot.generation);
    let piloted = identity.is_some();
    let result = world.with(system, move |s| {
        if let Some(context) = s.player_loss_context(ship, token, generation) { return Ok(context); }
        let checkpoint = if piloted {
            crate::space::snapshot::capture_fact_checkpoint(s)
                .map_err(|error| CallError::Internal(anyhow::anyhow!("loss checkpoint refused: {error:?}")))?
        } else { None };
        let context = Arc::new(crate::services::ship::loss::facts::PlayerLossContext::new(checkpoint, new));
        if !s.install_player_loss_context(ship, token, generation, context.clone()) {
            return Err(CallError::Internal(anyhow::anyhow!("loss context fence changed")));
        }
        Ok(context)
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("loss actor unavailable"))).and_then(|result| result);
    let pending = match result {
        Ok(context) => context,
        Err(error) => { release_loss(&world, system, &snapshot).await; return Err(error); }
    };
    let input = Arc::new(LossInput {
        position: snapshot.position, source: snapshot.source.clone(), lethal_at: snapshot.lethal_at,
        contributions: snapshot.contributions.clone(),
        victim: snapshot.victim, context: snapshot.victim_context, conflict: snapshot.conflict,
        pending: pending.clone(),
    });
    // The victim's freshest unheld loaded absolute, captured under the market
    // guard and system gates, rides the same transaction as the loss and
    // settles after its locks; a dying ship takes no business hold.
    let loaded = world.with(system, move |s| s.capture_dirty_loaded(&[ship])).await.unwrap_or_default();
    let result = db.transaction(
        |tx, outbox: &mut Outbox<OnlineChanged>| {
            let identity = identity;
            let snapshot = Arc::clone(&snapshot);
            let sde = Arc::clone(&sde);
            let input = Arc::clone(&input);
            let fences = fences.clone();
            let loaded = loaded.clone();
            Box::pin(async move {
                if let Some(identity) = identity.as_ref() {
                    // Serialize the piloted owner before the checkpoint and item locks.
                    crate::dao::characters::selection::lock_character(tx, identity.char_id).await.map_err(db_err)?;
                    let committed = if let Some(frame) = input.pending.checkpoint.as_ref() {
                        frame.is_committed(tx).await.map_err(db_err)?
                    } else { false };
                    let written = if committed {
                        if let Some(frame) = input.pending.checkpoint.as_ref() {
                            frame.append(outbox);
                            frame.condition_rows().map(|row| row.0).collect()
                        } else { Vec::new() }
                    } else {
                        let condition = crate::space::combat::ActorConditionSnapshot {
                            item_id: snapshot.item_id, generation: snapshot.generation,
                            revision: snapshot.before_revision, saved_revision: snapshot.before_revision.wrapping_add(1),
                            condition: snapshot.condition.clone(), contributions: snapshot.undo_contributions.clone(),
                            loaded: loaded.clone(),
                        };
                        super::checkpoint_conditions_in(tx, outbox, system, &[condition], input.pending.checkpoint.as_deref()).await?
                    };
                    let loss = destroy_ship_in_transaction(tx, outbox, identity, &sde, &input, fences.as_ref(), &loaded).await?;
                    if loss.is_some() {
                        if let Some(damage) = &snapshot.pending_damage { damage.append(outbox); }
                        let old = snapshot.condition.now;
                        let max = snapshot.condition.stats.max;
                        let changes = health_changes(identity.char_id, snapshot.item_id,
                            [new[0], max[1] - new[1], max[2] - new[2]],
                            [old[0], max[1] - old[1], max[2] - old[2]]);
                        outbox.notify_pilot(identity.char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
                    }
                    Ok((loss.is_some(), loss, written))
                } else {
                    Ok((super::unpiloted::destroy_in(tx, outbox, &sde, system, &snapshot, &input.pending, &loaded).await?, None, Vec::new()))
                }
            })
        },
        |(committed, loss, written), outbox| {
            let (db, world, sde, snapshot, identity, piloted_capsule) = (&db, &world, &sde, &snapshot, &identity, piloted_capsule);
            async move {
            if committed {
            if let Some(frame) = pending.checkpoint.as_ref() {
                let frame = frame.clone();
                let ids = written.into_iter().collect();
                world.with(system, move |s| frame.acknowledge(s, &ids)).await;
            }
            let mut stale_body = None;
            if let (Some(identity), Some(loss)) = (identity.as_ref(), loss) {
                let publication = publish_loss(identity, &db, &world, &sde, loss.clone(),
                    snapshot.token, snapshot.generation, snapshot.expected_online_session.clone()).await;
                let published = matches!(&publication, Ok(true));
                if published {
                    forget_target(&world, snapshot.item_id);
                    reset_pilot(&world, identity.char_id, Some((system, snapshot.item_id)));
                    let mut args = vec![Value::Int(snapshot.item_id), Value::Float(loss.position[0]), Value::Float(loss.position[1]), Value::Float(loss.position[2]), Value::Int(loss.replacement_ship)];
                    if let Some((a, b, c, d)) = loss.home { args.extend([Value::Int(a), Value::Int(b), Value::Int(c), Value::Int(d)]); }
                    world.notify_pilot(identity.char_id, "__ship_destroyed", args);
                } else if matches!(&publication, Ok(false)) {
                    if let Err(error) = retire_loss(&world, system, &snapshot).await {
                        tracing::warn!("committed loss retirement failed: {error:?}");
                        release_loss(&world, system, &snapshot).await;
                    }
                }
                if !published && piloted_capsule {
                    // A stale session CAS must not deliver the old body's refresh
                    // to the substitute connection; other recipients, durable
                    // events and generation-guarded online operations still run.
                    stale_body = Some(identity.char_id);
                }
            } else {
                if let Err(error) = retire_loss(&world, system, &snapshot).await {
                    tracing::warn!("committed loss retirement failed: {error:?}");
                    release_loss(&world, system, &snapshot).await;
                }
            }
            crate::scheduler::wake();
            let dispatched = crate::services::effects::online::dispatch_entries((), outbox, &world, |notification| {
                if matches!(&notification, crate::db::outbox::Notification::Pilot { char_id, .. } if Some(*char_id) == stale_body) {
                    return Ok(());
                }
                notification.dispatch(world, None)
            });
            if let Some(identity) = identity.as_ref() {
                if world.with(system, { let ship = snapshot.item_id; let token = snapshot.token; let generation = snapshot.generation;
                    move |s| s.combat_phase(ship) == Some(crate::space::combat::CombatPhase::Dying { token })
                        && s.combat_membership_generation(ship) == Some(generation) }).await.unwrap_or(false) {
                    tracing::warn!(character = identity.char_id, "committed combat loss awaits reconciliation");
                    release_loss(&world, system, &snapshot).await;
                }
            }
            dispatched
            } else {
                let result = reconcile_locked(&db, &world, system, &snapshot).await;
                if result.is_err() { release_loss(&world, system, &snapshot).await; }
                result
            }
            }
        },
    ).await;
    // A failed loss leaves its captured rows unproven: invalidate the exact
    // captured keys, then one fresh reseed under the gates still held above.
    if result.is_err() {
        let keys: Vec<_> = loaded.iter().map(|write| crate::dao::items::charges::LoadedSlotKey { ship: write.ship, module: write.module, flag: write.flag }).collect();
        if !keys.is_empty() {
            let invalidate = keys.clone();
            world.with(system, move |s| s.invalidate_held_loaded(&invalidate)).await;
            let _ = super::loaded::reseed_held(&db, &world, system, &keys).await;
        }
    }
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Err(reconcile_error) = reconcile_locked(&db, &world, system, &snapshot).await {
                tracing::warn!("combat loss reconciliation failed: {reconcile_error:?}");
                release_loss(&world, system, &snapshot).await;
            }
            Err(error)
        }
    }
}

async fn release_loss(world: &World, system: i64, snapshot: &crate::space::combat::FirstLethalSnapshot) {
    let (ship, token, generation) = (snapshot.item_id, snapshot.token, snapshot.generation);
    world.with(system, move |s| { s.release_pending_combat(ship, token, generation); }).await;
}

async fn retire_loss(world: &World, system: i64, snapshot: &crate::space::combat::FirstLethalSnapshot) -> Result<(), CallError> {
    let (ship, token, generation) = (snapshot.item_id, snapshot.token, snapshot.generation);
    let removed = world.with(system, move |s| {
        if s.combat_phase(ship) != Some(crate::space::combat::CombatPhase::Dying { token })
            || s.combat_membership_generation(ship) != Some(generation) { return false; }
        s.destroy_ball(ship);
        s.finalize_dying(ship, token, generation);
        true
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat actor unavailable during retirement")))?;
    if removed { forget_target(world, ship); }
    Ok(())
}

/// Reconcile against every original member, never against a withdrawn ID.
async fn recovery_proof(
    db: &Db, source: &crate::dao::repairable_ship::LossSource,
) -> Result<(bool, bool), CallError> {
    use crate::dao::repairable_ship::{SourceKind, Status};
    let proof = crate::dao::repairable_ship::source_proof(db, source).await.map_err(db_err)?;
    let Some(group) = proof.group else {
        return Ok((proof.physical.is_none(), proof.physical.as_ref() == Some(&source.shape)));
    };
    let expected_loss = (source.kind == SourceKind::PilotedShip).then_some(source.shape.item.get());
    if group.kind != source.kind || group.beneficiary != source.beneficiary || group.shape != source.shape
        || group.system != source.system || group.occurred_at != source.lethal_at
        || group.position != source.sampled_position.map(Some) || group.loss_id != expected_loss
        || proof.members.is_empty() {
        return Ok((false, false));
    }
    let ids: Vec<_> = proof.members.iter().map(|member| member.item).collect();
    let unique: std::collections::BTreeSet<_> = ids.iter().copied().collect();
    if unique.len() != ids.len() { return Ok((false, false)); }
    let physical = crate::dao::items::repairable_ship::capture_many(db, &ids).await.map_err(db_err)?;
    let rows: std::collections::BTreeMap<_, _> = physical.iter().map(|row| (row.item, row)).collect();
    for member in &proof.members {
        if member.group != group.id || member.original_owner != source.beneficiary { return Ok((false, false)); }
        if Status::ACTIVE.contains(&member.status) {
            let Some(row) = rows.get(&member.item) else { return Ok((false, false)) };
            if row.owner != infinity_core::consts::NPC_CORP_INTERBUS || row.location != group.id.get()
                || row.type_id != member.type_id || row.flag != member.flag || row.quantity != member.quantity
                || row.singleton != member.singleton || row.brain_slot.is_some() || row.jump_clone.is_some()
                || member.withdrawn_item.is_some() { return Ok((false, false)); }
            if row.item == source.shape.item && (row.x != source.shape.x || row.y != source.shape.y || row.z != source.shape.z) {
                return Ok((false, false));
            }
        } else {
            if rows.contains_key(&member.item) { return Ok((false, false)); }
            match member.status {
                Status::Withdrawn if member.withdrawn_item.is_some_and(|new| new != member.item) => {}
                Status::Abandoned | Status::Expired if member.withdrawn_item.is_none() => {}
                _ => return Ok((false, false)),
            }
        }
    }
    if !unique.contains(&source.shape.item) && proof.physical.is_some() { return Ok((false, false)); }
    Ok((true, false))
}

async fn reconcile_locked(
    db: &Db, world: &World, system: i64, snapshot: &crate::space::combat::FirstLethalSnapshot,
) -> Result<(), CallError> {
    use crate::dao::items::loading::{load_items, ItemQuery};
    let ship = snapshot.item_id;
    let (token, generation) = (snapshot.token, snapshot.generation);
    let pending = world.with(system, move |s| s.player_loss_context(ship, token, generation)).await
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("loss actor unavailable")))?;
    let frozen = pending.as_ref().and_then(|context| crate::poison::lock_or_exit(context.recovery_source.lock()).clone())
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("loss source unavailable")))?;
    let (physical_committed, original_live) = recovery_proof(db, &frozen).await?;
    if let Some(identity) = loss_identity(system, snapshot) {
        let char_id = identity.char_id;
        let source = db.transaction(
            |tx, _outbox: &mut Outbox| Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
                crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?;
                let key = crate::db::outbox::SourceKey::canonical("LossSettled", ship, 0);
                crate::dao::progress::source(tx, "business", key.as_str()).await.map_err(db_err)
            }),
            |source, _outbox| async move { Ok(source) },
        ).await?;
        let candidate = pending.as_ref().and_then(|context| crate::poison::lock_or_exit(context.prepared.lock()).clone());
        let committed = if let Some(source) = source {
            let prepared = candidate.as_ref().ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing prepared loss receipt")))?;
            let (occurred_at, context) = crate::lua::events::combat::metadata(&prepared.event)
                .ok_or_else(|| CallError::Internal(anyhow::anyhow!("invalid prepared loss receipt")))?;
            let stored_context: serde_json::Value = serde_json::from_str(&source.context).map_err(|e| CallError::Internal(e.into()))?;
            let expected_context = serde_json::to_value(context).map_err(|e| CallError::Internal(e.into()))?;
            if source.event != prepared.event || source.occurred_at != occurred_at || stored_context != expected_context
                || source.origin != context.origin.as_str() || source.ordinal != 0 || source.subject_character_id != Some(char_id) {
                return Err(CallError::Internal(anyhow::anyhow!("loss source collision")));
            }
            true
        } else { false };
        let victim_current = crate::dao::combat::victim_current(db, identity.char_id, ship, system).await.map_err(db_err)?;
        let killed = crate::dao::killmails::victim_exists(db, ship).await.map_err(db_err)?;
        let selected = crate::dao::characters::selection::ship_id(db, identity.char_id).await.map_err(db_err)?;
        if !committed && !killed && original_live && selected == Some(ship) && victim_current {
            let (token, generation) = (snapshot.token, snapshot.generation);
            let restored = world.with(system, move |s| s.restore_known_rollback(ship, token, generation))
                .await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat actor unavailable during rollback")))?;
            if !restored { return Err(CallError::Internal(anyhow::anyhow!("loss rollback fence unavailable"))); }
            return Ok(());
        }
        if committed && killed && physical_committed && !victim_current && selected != Some(ship) {
            if let Some(frame) = pending.as_ref().and_then(|p| p.checkpoint.as_ref()) {
                if !frame.is_committed(db).await.map_err(db_err)? {
                    return Err(CallError::Internal(anyhow::anyhow!("committed loss missing checkpoint facts")));
                }
                let ids = frame.condition_rows().map(|row| row.0).collect();
                let frame = frame.clone();
                world.with(system, move |s| frame.acknowledge(s, &ids)).await
                    .ok_or_else(|| CallError::Internal(anyhow::anyhow!("loss actor unavailable")))?;
            }
            let expected_session = snapshot.expected_online_session.as_ref().map(|session| session.session_id);
            let current_session = world.online_session(identity.char_id).map(|session| session.session_id);
            if expected_session != current_session {
                retire_loss(world, system, snapshot).await?;
                return Ok(());
            }
            let replacement_ship = selected.ok_or_else(|| CallError::Internal(anyhow::anyhow!("lost pilot has no selected ship")))?;
            let selected_items = load_items(db, ItemQuery::Id(replacement_ship)).await.map_err(db_err)?;
            let item = selected_items.first().ok_or_else(|| CallError::Internal(anyhow::anyhow!("selected ship unavailable")))?;
            if item.owner_id != identity.char_id {
                return Err(CallError::Internal(anyhow::anyhow!("selected ship owner changed")));
            }
            let locations = crate::dao::characters::routing::fleet_member_locations(db, &[identity.char_id]).await.map_err(db_err)?;
            let location = locations.first().ok_or_else(|| CallError::Internal(anyhow::anyhow!("selected character location unavailable")))?;
            let mut position = snapshot.position;
            let home = if location.station_id.is_none() && location.solar_system_id == Some(system) && item.location_id == system {
                let selected_space = crate::dao::characters::selection::space_ship(db, identity.char_id).await.map_err(db_err)?
                    .filter(|selected| selected.item_id == replacement_ship)
                    .ok_or_else(|| CallError::Internal(anyhow::anyhow!("selected space ship unavailable")))?;
                position = [selected_space.x, selected_space.y, selected_space.z].map(|v| v.unwrap_or(f64::NAN));
                if !position.iter().all(|v| v.is_finite()) {
                    return Err(CallError::Internal(anyhow::anyhow!("selected ship position unavailable")));
                }
                None
            } else {
                if location.station_id != Some(item.location_id) {
                    return Err(CallError::Internal(anyhow::anyhow!("selected character location changed")));
                }
                let station = world.sde().map.station(item.location_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("selected ship location unavailable")))?;
                Some((station.station_id, station.solar_system_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("station system unavailable")))?, station.constellation_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("station constellation unavailable")))?, station.region_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("station region unavailable")))?))
            };
            let outcome = LossOutcome { replacement_ship, home, position };
            let prepared = &candidate.as_ref().expect("committed receipt checked").outcome;
            let body = candidate.as_ref().expect("committed receipt checked").body;
            if prepared.replacement_ship != outcome.replacement_ship || prepared.home != outcome.home || prepared.position != outcome.position {
                return Err(CallError::Internal(anyhow::anyhow!("committed loss outcome changed")));
            }
            if !publish_loss(&identity, db, world, world.sde(), outcome.clone(), snapshot.token, snapshot.generation, snapshot.expected_online_session.clone()).await? {
                retire_loss(world, system, snapshot).await?;
                return Ok(());
            }
            forget_target(world, ship);
            reset_pilot(world, identity.char_id, Some((system, ship)));
            let mut args = vec![Value::Int(ship), Value::Float(outcome.position[0]), Value::Float(outcome.position[1]), Value::Float(outcome.position[2]), Value::Int(outcome.replacement_ship)];
            if let Some((a, b, c, d)) = outcome.home { args.extend([Value::Int(a), Value::Int(b), Value::Int(c), Value::Int(d)]); }
            world.notify_pilot(identity.char_id, "__ship_destroyed", args);
            // A verified body loss replays its refresh categories after the
            // session: the clone-destruction and changed-class notices, then the
            // forced skill refresh and brain recompile that reread them from the
            // database. No settlement, deletion or training work repeats.
            if let Some(body) = body {
                world.notify_pilot(identity.char_id, "OnCloneDestruction", vec![]);
                if body.implants_changed { world.notify_pilot(identity.char_id, "OnServerImplantsChanged", vec![]); }
                if body.boosters_changed { world.notify_pilot(identity.char_id, "OnServerBoostersChanged", vec![]); }
                world.notify_pilot(identity.char_id, "OnSkillForcedRefresh", vec![]);
                world.notify_pilot(identity.char_id, crate::client::BRAIN_REFRESH, vec![]);
            }
            if let Some(context) = pending.as_ref() {
                let new = context.new_condition;
                let old = snapshot.condition.now;
                let max = snapshot.condition.stats.max;
                let changes = health_changes(identity.char_id, ship,
                    [new[0], max[1] - new[1], max[2] - new[2]],
                    [old[0], max[1] - old[1], max[2] - old[2]]);
                world.notify_pilot(identity.char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
            }
            return Ok(());
        }
    } else {
        if physical_committed {
            retire_loss(world, system, snapshot).await?;
            return Ok(());
        } else if original_live {
            let (token, generation) = (snapshot.token, snapshot.generation);
            let restored = world.with(system, move |s| s.restore_known_rollback(ship, token, generation)).await
                .ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat actor unavailable during rollback")))?;
            if !restored { return Err(CallError::Internal(anyhow::anyhow!("loss rollback fence unavailable"))); }
        return Ok(());
        }
    }
    Err(CallError::Internal(anyhow::anyhow!("combat loss state is inconclusive")))
}

pub(crate) async fn reconcile_pending_loss(
    db: Db, world: Arc<World>, system: i64, ship: i64,
    snapshot: Arc<crate::space::combat::FirstLethalSnapshot>,
) {
    if snapshot.is_npc {
        if let Err(error) = super::groups_health::settle_npc_loss(db, world.clone(), system, snapshot.clone(), [0.0; 3]).await {
            tracing::warn!(ship, "pending NPC loss reconciliation failed: {error:?}");
            release_loss(&world, system, &snapshot).await;
        }
        return;
    }
    let _market = super::super::market::lock().await;
    let result = match world.lock_condition_snapshot(system).await {
        Ok(_gate) => reconcile_locked(&db, &world, system, &snapshot).await,
        Err(error) => Err(db_err(error)),
    };
    if let Err(error) = result {
        tracing::warn!(ship, "pending combat loss reconciliation failed: {error:?}");
        release_loss(&world, system, &snapshot).await;
    }
}

pub(crate) fn repair_target_in_system(s: &mut crate::space::System, target: i64, layer: usize, amount: f64, resistance_attribute: Option<i32>, source: crate::space::combat::RepairSource) -> Result<bool, crate::space::combat_facts::Refusal> {
    let Some((max, old)) = s.combat_health_scalars(target) else {
        if let crate::space::combat::RepairSource::Remote(facts) = source { s.finish_combat_attempt(&facts); }
        return Ok(false);
    };
    let Some(ball) = s.ball(target) else {
        if let crate::space::combat::RepairSource::Remote(facts) = source { s.finish_combat_attempt(&facts); }
        return Ok(false);
    };
    let pilot = ball.char_id;
    let amount = resistance_attribute.map_or(amount, |attribute| {
        s.with_combat_fit(target, |fit, key, _| amount * fit.value(key, attribute)).unwrap_or(amount)
    });
    if !s.repair_condition(target, layer, amount, source)? { return Ok(true); }
    let Some((_, new)) = s.combat_health_scalars(target) else { return Ok(false) };
    let health = [0, 1, 2].map(|i| if max[i] > 0.0 {
        (if i == 0 { new[i] } else { max[i] - new[i] } / max[i]).clamp(0.0, 1.0)
    } else { 0.0 });
    s.set_health(target, health);
    if let Some(char_id) = pilot {
        let changes = health_changes(char_id, target, new, old);
        s.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
    }
    Ok(true)
}

pub(crate) async fn repair_target(
    world: &World, system: i64, target: i64, layer: usize, amount: f64, resistance_attribute: Option<i32>, source: crate::space::combat::RepairSource,
    guard: impl FnOnce(&crate::space::System) -> bool + Send + 'static,
) -> Result<(), CallError> {
    if layer >= 3 || !amount.is_finite() || amount <= 0.0 {
        if let crate::space::combat::RepairSource::Remote(facts) = source {
            let _ = world.with(system, move |s| s.finish_combat_attempt(&facts)).await;
        }
        return Ok(());
    }
    let present = world.with(system, move |s| {
        if !guard(s) {
            if let crate::space::combat::RepairSource::Remote(facts) = source { s.finish_combat_attempt(&facts); }
            return Ok(true);
        }
        repair_target_in_system(s, target, layer, amount, resistance_attribute, source)
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat actor unavailable")))?;
    let present = match present {
        Ok(present) => present,
        Err(refusal) => match fact_refusal(system, refusal) {
            Some(error) => return Err(error),
            None => return Ok(()),
        },
    };
    if !present { return Err(CallError::Internal(anyhow::anyhow!("combat target condition unavailable"))); }
    Ok(())
}

#[cfg(all(test, feature = "tests-combat"))] mod tests;
