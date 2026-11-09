//! Applies typed damage and repairs to actor-owned fitted health. Nonlethal
//! observations follow the actor mutation; player loss facts and notifications
//! commit atomically through the ship loss service. Live health is checkpointed
//! by the system snapshot worker. Flat, Breacher and DOT damage are unsupported.
use super::*;
use crate::db::outbox::dispatch_world;
use crate::services::ship::loss::{destroy_ship_in_transaction, publish_loss, LossInput, LossOutcome};

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



pub(crate) async fn apply_damage_guarded(
    db: &Db, world: &World, system: i64, target: i64, raw: [f64; 4],
    source: DamageSource, force_loss: bool,
    guard: impl FnOnce(&crate::space::System) -> bool + Send + 'static,
) -> Result<Option<DamageOutcome>, CallError> {
    if raw.iter().any(|v| !v.is_finite() || *v < 0.0) {
        if let Some(facts) = source.facts {
            let _ = world.with(system, move |s| s.finish_combat_attempt(&facts)).await;
        }
        return Err(CallError::Internal(anyhow::anyhow!("invalid typed damage")));
    }
    let db = db.clone();
    let result = world.with(system, move |s| {
        if !guard(s) {
            if let Some(facts) = source.facts { s.finish_combat_attempt(&facts); }
            return Ok(None);
        }
        if s.sampled_ball(target).is_some_and(|ball| !ball.pos.iter().all(|v| v.is_finite())) {
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
        complete_damage_outcome(s, &db, system, target, outcome).map(Some)
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
                    let changes = vec![
                        attribute_change(char_id, Value::Int(target), ATTR_SHIELD_CHARGE, Value::Float(outcome.new[0]), Value::Float(outcome.old[0])),
                        attribute_change(char_id, Value::Int(target), ATTR_ARMOR_DAMAGE, Value::Float(max[1] - outcome.new[1]), Value::Float(max[1] - outcome.old[1])),
                        attribute_change(char_id, Value::Int(target), ATTR_HULL_DAMAGE, Value::Float(max[2] - outcome.new[2]), Value::Float(max[2] - outcome.old[2])),
                    ];
                    s.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
                    if let Some(attacker_ball_id) = outcome.source.attacker_ball_id {
                        crate::progress::publish_live(s.progress_registry(), &crate::lua::events::GameEvent::Damaged { char_id, ship_id: target, attacker_ball_id, system_id: system });
                    }
                }
            }
        }
        Ok((result, None))
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
    let _gate = match world.lock_condition_snapshot(system).await {
        Ok(gate) => gate,
        Err(error) => {
            release_loss(&world, system, &snapshot).await;
            return Err(db_err(error));
        }
    };
    let identity = loss_identity(system, &snapshot);
    let sde = world.sde().clone();
    let (ship, token, generation) = (snapshot.item_id, snapshot.token, snapshot.generation);
    let pending = if identity.is_some() {
        let result = world.with(system, move |s| {
            if let Some(context) = s.player_loss_context(ship, token, generation) { return Ok(context); }
            let checkpoint = crate::space::snapshot::capture_fact_checkpoint(s)
                .map_err(|error| CallError::Internal(anyhow::anyhow!("loss checkpoint refused: {error:?}")))?;
            let context = Arc::new(crate::services::ship::loss::facts::PlayerLossContext { checkpoint, prepared: std::sync::Mutex::new(None), new_condition: new });
            if !s.install_player_loss_context(ship, token, generation, context.clone()) {
                return Err(CallError::Internal(anyhow::anyhow!("loss context fence changed")));
            }
            Ok(context)
        }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("loss actor unavailable"))).and_then(|result| result);
        match result {
            Ok(context) => context,
            Err(error) => { release_loss(&world, system, &snapshot).await; return Err(error); }
        }
    } else {
        Arc::new(crate::services::ship::loss::facts::PlayerLossContext { checkpoint: None, prepared: std::sync::Mutex::new(None), new_condition: new })
    };
    let input = Arc::new(LossInput {
        position: snapshot.position, source: snapshot.source.clone(), lethal_at: snapshot.lethal_at,
        contributions: snapshot.contributions.clone(),
        victim: snapshot.victim, context: snapshot.victim_context, conflict: snapshot.conflict,
        pending: pending.clone(),
    });
    let result = db.transaction(
        |tx, outbox: &mut Outbox| {
            let identity = identity;
            let snapshot = Arc::clone(&snapshot);
            let sde = Arc::clone(&sde);
            let input = Arc::clone(&input);
            Box::pin(async move {
                if let Some(identity) = identity.as_ref() {
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
                        };
                        super::checkpoint_conditions_in(tx, outbox, system, &[condition], input.pending.checkpoint.as_deref()).await?
                    };
                    let loss = destroy_ship_in_transaction(tx, outbox, identity, &sde, &input).await?;
                    if loss.is_some() {
                        if let Some(damage) = &snapshot.pending_damage { damage.append(outbox); }
                        let old = snapshot.condition.now;
                        let max = snapshot.condition.stats.max;
                        let ship = Value::Int(snapshot.item_id);
                        let changes = vec![
                            attribute_change(identity.char_id, ship.clone(), ATTR_SHIELD_CHARGE, Value::Float(new[0]), Value::Float(old[0])),
                            attribute_change(identity.char_id, ship.clone(), ATTR_ARMOR_DAMAGE, Value::Float(max[1] - new[1]), Value::Float(max[1] - old[1])),
                            attribute_change(identity.char_id, ship, ATTR_HULL_DAMAGE, Value::Float(max[2] - new[2]), Value::Float(max[2] - old[2])),
                        ];
                        outbox.notify_pilot(identity.char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
                    }
                    Ok((loss.is_some(), loss, written))
                } else {
                    Ok((super::unpiloted::destroy_in(tx, &sde, system, &snapshot).await?, None, Vec::new()))
                }
            })
        },
        |(committed, loss, written), outbox| {
            let (db, world, sde, snapshot, identity) = (&db, &world, &sde, &snapshot, &identity);
            async move {
            if committed {
            if let Some(frame) = pending.checkpoint.as_ref() {
                let frame = frame.clone();
                let ids = written.into_iter().collect();
                world.with(system, move |s| frame.acknowledge(s, &ids)).await;
            }
            if let (Some(identity), Some(loss)) = (identity.as_ref(), loss) {
                let publication = publish_loss(identity, &db, &world, &sde, loss.clone(),
                    snapshot.token, snapshot.generation, snapshot.expected_online_session.clone()).await;
                let published = matches!(&publication, Ok(true));
                if published {
                    forget_target(&world, snapshot.item_id);
                    reset_pilot(&world, identity.char_id, Some((system, snapshot.item_id)));
                    let mut args = vec![Value::Int(snapshot.item_id), Value::Float(loss.position[0]), Value::Float(loss.position[1]), Value::Float(loss.position[2]), Value::Int(loss.capsule)];
                    if let Some((a, b, c, d)) = loss.home { args.extend([Value::Int(a), Value::Int(b), Value::Int(c), Value::Int(d)]); }
                    world.notify_pilot(identity.char_id, "__ship_destroyed", args);
                } else if matches!(&publication, Ok(false)) {
                    if let Err(error) = retire_loss(&world, system, &snapshot).await {
                        tracing::warn!("committed loss retirement failed: {error:?}");
                        release_loss(&world, system, &snapshot).await;
                    }
                }
            } else {
                if let Err(error) = retire_loss(&world, system, &snapshot).await {
                    tracing::warn!("committed loss retirement failed: {error:?}");
                    release_loss(&world, system, &snapshot).await;
                }
            }
            crate::scheduler::wake();
            let dispatched = dispatch_world((), outbox, &world);
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

async fn reconcile_locked(
    db: &Db, world: &World, system: i64, snapshot: &crate::space::combat::FirstLethalSnapshot,
) -> Result<(), CallError> {
    use crate::dao::items::loading::{load_items, ItemQuery};
    let ship = snapshot.item_id;
    if let Some(identity) = loss_identity(system, snapshot) {
        let (token, generation) = (snapshot.token, snapshot.generation);
        let pending = world.with(system, move |s| s.player_loss_context(ship, token, generation)).await
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("loss actor unavailable")))?;
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
        let candidate = pending.as_ref().map(|context| context.prepared.lock()
            .map(|prepared| prepared.clone())
            .map_err(|_| CallError::Internal(anyhow::anyhow!("loss receipt lock poisoned")))).transpose()?.flatten();
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
        let items = load_items(db, ItemQuery::Id(ship)).await.map_err(db_err)?;
        let killed = crate::dao::killmails::victim_exists(db, ship).await.map_err(db_err)?;
        let selected = crate::dao::characters::selection::ship_id(db, identity.char_id).await.map_err(db_err)?;
        if !committed && !killed && selected == Some(ship)
            && crate::dao::combat::victim_current(db, identity.char_id, ship, system).await.map_err(db_err)? {
            let (token, generation) = (snapshot.token, snapshot.generation);
            let restored = world.with(system, move |s| s.restore_known_rollback(ship, token, generation))
                .await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat actor unavailable during rollback")))?;
            if !restored { return Err(CallError::Internal(anyhow::anyhow!("loss rollback fence unavailable"))); }
            return Ok(());
        }
        if committed && killed && items.is_empty() {
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
            let capsule = selected.ok_or_else(|| CallError::Internal(anyhow::anyhow!("lost pilot has no selected ship")))?;
            let selected_items = load_items(db, ItemQuery::Id(capsule)).await.map_err(db_err)?;
            let item = selected_items.first().ok_or_else(|| CallError::Internal(anyhow::anyhow!("selected ship unavailable")))?;
            if item.owner_id != identity.char_id {
                return Err(CallError::Internal(anyhow::anyhow!("selected ship owner changed")));
            }
            let locations = crate::dao::characters::routing::fleet_member_locations(db, &[identity.char_id]).await.map_err(db_err)?;
            let location = locations.first().ok_or_else(|| CallError::Internal(anyhow::anyhow!("selected character location unavailable")))?;
            let mut position = snapshot.position;
            let home = if location.station_id.is_none() && location.solar_system_id == Some(system) && item.location_id == system {
                let selected_space = crate::dao::characters::selection::space_ship(db, identity.char_id).await.map_err(db_err)?
                    .filter(|selected| selected.item_id == capsule)
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
            let outcome = LossOutcome { capsule, home, position };
            let prepared = &candidate.as_ref().expect("committed receipt checked").outcome;
            if prepared.capsule != outcome.capsule || prepared.home != outcome.home || prepared.position != outcome.position {
                return Err(CallError::Internal(anyhow::anyhow!("committed loss outcome changed")));
            }
            if !publish_loss(&identity, db, world, world.sde(), outcome.clone(), snapshot.token, snapshot.generation, snapshot.expected_online_session.clone()).await? {
                retire_loss(world, system, snapshot).await?;
                return Ok(());
            }
            forget_target(world, ship);
            reset_pilot(world, identity.char_id, Some((system, ship)));
            let mut args = vec![Value::Int(ship), Value::Float(outcome.position[0]), Value::Float(outcome.position[1]), Value::Float(outcome.position[2]), Value::Int(outcome.capsule)];
            if let Some((a, b, c, d)) = outcome.home { args.extend([Value::Int(a), Value::Int(b), Value::Int(c), Value::Int(d)]); }
            world.notify_pilot(identity.char_id, "__ship_destroyed", args);
            if let Some(context) = pending.as_ref() {
                let new = context.new_condition;
                let old = snapshot.condition.now;
                let max = snapshot.condition.stats.max;
                let item = Value::Int(ship);
                let changes = vec![
                    attribute_change(identity.char_id, item.clone(), ATTR_SHIELD_CHARGE, Value::Float(new[0]), Value::Float(old[0])),
                    attribute_change(identity.char_id, item.clone(), ATTR_ARMOR_DAMAGE, Value::Float(max[1] - new[1]), Value::Float(max[1] - old[1])),
                    attribute_change(identity.char_id, item, ATTR_HULL_DAMAGE, Value::Float(max[2] - new[2]), Value::Float(max[2] - old[2])),
                ];
                world.notify_pilot(identity.char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
            }
            return Ok(());
        }
    } else {
        let items = load_items(db, ItemQuery::Id(ship)).await.map_err(db_err)?;
        if items.is_empty() {
            retire_loss(world, system, snapshot).await?;
            return Ok(());
        } else if items.iter().any(|item| item.location_id == system && item.owner_id == snapshot.owner_id
            && Some(item.type_id) == snapshot.kind && item.singleton && item.quantity == 1 && item.flag_id == 0) {
        let (token, generation) = (snapshot.token, snapshot.generation);
        world.with(system, move |s| {
            if !s.restore_known_rollback(ship, token, generation) {
                s.finalize_dying(ship, token, generation);
            }
        }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat actor unavailable during rollback")))?;
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

pub(crate) fn repair_target_in_system(s: &mut crate::space::System, target: i64, layer: usize, amount: f64, source: crate::space::combat::RepairSource) -> Result<bool, crate::space::combat_facts::Refusal> {
    let Some((max, old)) = s.combat_health_scalars(target) else {
        if let crate::space::combat::RepairSource::Remote(facts) = source { s.finish_combat_attempt(&facts); }
        return Ok(false);
    };
    let Some(ball) = s.ball(target) else {
        if let crate::space::combat::RepairSource::Remote(facts) = source { s.finish_combat_attempt(&facts); }
        return Ok(false);
    };
    let pilot = ball.char_id;
    if !s.repair_condition(target, layer, amount, source)? { return Ok(true); }
    let Some((_, new)) = s.combat_health_scalars(target) else { return Ok(false) };
    let health = [0, 1, 2].map(|i| if max[i] > 0.0 {
        (if i == 0 { new[i] } else { max[i] - new[i] } / max[i]).clamp(0.0, 1.0)
    } else { 0.0 });
    s.set_health(target, health);
    if let Some(char_id) = pilot {
        let changes = vec![
            attribute_change(char_id, Value::Int(target), ATTR_SHIELD_CHARGE, Value::Float(new[0]), Value::Float(old[0])),
            attribute_change(char_id, Value::Int(target), ATTR_ARMOR_DAMAGE, Value::Float(new[1]), Value::Float(old[1])),
            attribute_change(char_id, Value::Int(target), ATTR_HULL_DAMAGE, Value::Float(new[2]), Value::Float(old[2])),
        ];
        s.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
    }
    Ok(true)
}

pub(crate) async fn repair_target(
    world: &World, system: i64, target: i64, layer: usize, amount: f64, source: crate::space::combat::RepairSource,
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
        repair_target_in_system(s, target, layer, amount, source)
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

#[cfg(test)] mod tests;
