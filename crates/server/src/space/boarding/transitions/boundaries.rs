//! Dock and transfer checkpoint settlements owned by `World`. Each entry
//! defers its committed transition onto the source system actor, then runs the
//! database transaction, publication and reconciliation from that boundary.

use super::*;

impl World {
    pub(crate) async fn checkpoint_dock(&self, db: Db, character: i64, ship: i64, source_system: i64,
        destination: station::Destination,
        transition: CombatTransition, market: crate::services::market::economy_lock::Guard,
        gate: tokio::sync::OwnedMutexGuard<()>, admission: crate::space::containment::ActorAdmission) -> Result<Item, CallError> {
        let world = transition.world().ok_or_else(|| CallError::Internal(anyhow::anyhow!("system world stopped")))?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let identity = crate::services::identity::Identity { char_id: character,
            ship_id: Some(ship), corp_id: 0, station_id: None, system_id: Some(source_system) };
        // Under the already-held source gate: resolve older bounty receipts,
        // then capture this character's dirty pool rows from the real actor.
        let bounties = if character == 0 { None } else {
            for receipt in self.with(source_system, move |s| s.bounty_pending_receipts(Some(character))).await.unwrap_or_default() {
                if let Ok(outcome) = crate::services::bounty::npc::resolve_receipt(&db, &receipt).await {
                    let _ = receipt.settle(&world, outcome).await;
                }
            }
            self.with(source_system, move |s| s.capture_npc_bounties(Some(character))).await
                .unwrap_or(Ok(None)).ok().flatten()
        };
        let plan = Arc::new(DockSettlement {
            world: Arc::downgrade(&world), db, character, ship, source_system, destination,
            token: transition.entries[0].0, generation: transition.entries[0].1.generation,
            facts: transition.facts.clone(), bounties,
            expected: self.online_session(character), receipt: Mutex::new(Some(tx)),
            admission,
        });
        let submitted = self.with(source_system, move |s| s.defer_condition_transition(transition, move |mut transition| async move {
            let snapshot = transition.entries[0].1.clone();
            // The guard arms before the transaction may execute; from then an
            // undelivered disposition invalidates instead of rolling back.
            let mut loaded = transition.held_loaded.take();
            if let Some(hold) = loaded.as_mut() { hold.may_execute(); }
            let result: Result<Option<Item>, CallError> = plan.db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| {
                    let snapshot = snapshot.clone();
                    let facts = plan.facts.clone();
                    let sde = world.sde().clone();
                    let plan = plan.clone();
                    Box::pin(async move {
                        // lock_hull locks the source character row and the hull
                        // at the space source; it keeps the anonymous headless
                        // branch without any character access.
                        let hull = station::lock_hull(tx, &sde, &identity, None).await?;
                        if character != 0 && !crate::dao::combat::victim_current(tx, character, ship, source_system).await.map_err(crate::services::db_err)? {
                            return Err(CallError::Internal(anyhow::anyhow!("ship is no longer available for docking")));
                        }
                        let written = crate::services::combat::checkpoint_conditions_in(tx, outbox, source_system, std::slice::from_ref(&snapshot), facts.as_deref()).await?;
                        if snapshot.revision != snapshot.saved_revision && !written.contains(&ship) {
                            return Err(CallError::Internal(anyhow::anyhow!("ship condition was not checkpointed")));
                        }
                        if let Some(bounties) = &plan.bounties {
                            bounties.write_in(tx).await?;
                        }
                        let committed = station::persist(tx, &identity, hull, destination).await?;
                        if character != 0 {
                            outbox.event(crate::lua::events::GameEvent::Docked { char_id: character, ship_id: ship, station_id: destination.station_id, system_id: destination.system_id });
                        }
                        Ok((written, committed))
                    })
                }, |committed, outbox| {
                    let plan = &plan;
                    let world = &world;
                    async move {
                    if let Some(bounties) = &plan.bounties {
                        let _ = bounties.acknowledge(world).await;
                    }
                    let (written, hull) = committed;
                    acknowledge_facts(world, source_system, plan.facts.clone(), written.into_iter().collect()).await;
                    let published = plan.publish(&world).await;
                    crate::db::outbox::dispatch_world(published.then_some(hull), outbox, world)
                    }
                },
            ).await;
            if let Ok(Some(hull)) = result {
                if let Some(hold) = loaded.take() {
                    crate::services::combat::loaded::commit_disposition(&world, source_system, hold).await;
                }
                drop(gate);
                drop(market);
                if character != 0 {
                    if let Err(error) = crate::services::corps::pending_kicks::trigger(&plan.db, &world, character).await {
                        tracing::warn!(?error, character, "completed dock pending kick failed");
                    }
                }
                plan.complete_receipt(Ok(hull));
                return;
            }
            if let Some(hold) = loaded.take() {
                crate::services::combat::loaded::unknown_disposition(&plan.db, &world, source_system, hold).await;
            }
            let callback = plan.callback();
            let (token, generation) = (plan.token, plan.generation);
            let pending = world.with(source_system, move |s| s.mark_pending_transition(ship, token, generation, callback)).await.unwrap_or(false);
            drop(gate);
            drop(market);
            if pending { plan.reconcile().await; }
            else if plan.admission.check().is_err() { plan.complete_receipt(Err(CallError::Internal(anyhow::anyhow!(crate::space::containment::ActorRetiring)))); }
            else { plan.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("system actor stopped during dock settlement")))); }
        })).await.unwrap_or(false);
        if !submitted { return Err(CallError::Internal(anyhow::anyhow!("system worker unavailable"))); }
        rx.await.map_err(|_| CallError::Internal(anyhow::anyhow!("dock worker unavailable")))?
    }
    pub(crate) async fn checkpoint_transfer(&self, db: Db, character: i64, ship: i64, source: i64, dest: i64,
        position: V3, ancestry: (i64, i64), prepared: PreparedShip, cloak_until: Option<i64>,
        transition: CombatTransition, market: crate::services::market::economy_lock::Guard,
        gates: Vec<tokio::sync::OwnedMutexGuard<()>>, admission: (crate::space::containment::ActorAdmission, Option<crate::space::containment::ActorAdmission>)) -> Result<(), CallError> {
        let world = transition.world().ok_or_else(|| CallError::Internal(anyhow::anyhow!("system world stopped")))?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        // Under the already-held source gate: resolve older bounty receipts,
        // then capture this character's dirty source-bucket rows from the
        // real actor; a transfer writes only source rows.
        let bounties = if character == 0 { None } else {
            for receipt in self.with(source, move |s| s.bounty_pending_receipts(Some(character))).await.unwrap_or_default() {
                if let Ok(outcome) = crate::services::bounty::npc::resolve_receipt(&db, &receipt).await {
                    let _ = receipt.settle(&world, outcome).await;
                }
            }
            self.with(source, move |s| s.capture_npc_bounties(Some(character))).await
                .unwrap_or(Ok(None)).ok().flatten()
        };
        let plan = Arc::new(TransferSettlement {
            world: Arc::downgrade(&world), db, character, ship, source, dest,
            original: prepared.ball.clone(), position, prior_position: Mutex::new(None),
            token: transition.entries[0].0, generation: transition.entries[0].1.generation,
            snapshot: { let mut snapshot = transition.entries[0].1.clone(); snapshot.loaded.clear(); snapshot },
            expected: self.online_session(character),
            facts: transition.facts.clone(), bounties,
            scope: transition.scope.filter(|(owner, _)| *owner == character).map(|(_, scope)| scope),
            cloak_until, receipt: Mutex::new(Some(tx)), admission,
        });
        let submitted = self.with(source, move |s| s.defer_condition_transition(transition, move |mut transition| async move {
            // The settlement keeps no long-term loaded batch: this attempt's
            // snapshot carries the transient capture the transaction settles.
            let snapshot = transition.entries[0].1.clone();
            let settled_loaded = snapshot.loaded.clone();
            let mut loaded = transition.held_loaded.take();
            let prior = crate::dao::characters::selection::space_ship(&plan.db, character).await;
            let prior = match prior {
                Ok(Some(row)) if row.item_id == ship => row.x.zip(row.y).zip(row.z).map(|((x, y), z)| [x, y, z]),
                _ => None,
            };
            // Early exits release the never-written capture but keep the dirty
            // bucket for the next checkpoint.
            if prior.is_none() {
                let (token, generation) = (plan.token, plan.generation);
                world.with(source, move |s| s.cancel_combat_transition(ship, token, generation)).await;
                if let Some(bounty_receipt) = &plan.bounties {
                    let _ = bounty_receipt.settle(&world, crate::services::bounty::npc::checkpoint::CheckpointOutcome::Unchanged).await;
                }
                plan.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("ship position unavailable before transfer"))));
                return;
            }
            *crate::poison::lock_or_exit(plan.prior_position.lock()) = prior;
            let occurrence = if cloak_until.is_some() {
                match crate::dao::map_telemetry::next_occurrence(&plan.db).await {
                    Ok(id) => Some((id, crate::client::filetime_now())),
                    Err(error) => {
                        let (token, generation) = (plan.token, plan.generation);
                        world.with(source, move |s| s.cancel_combat_transition(ship, token, generation)).await;
                        if let Some(bounty_receipt) = &plan.bounties {
                            let _ = bounty_receipt.settle(&world, crate::services::bounty::npc::checkpoint::CheckpointOutcome::Unchanged).await;
                        }
                        plan.complete_receipt(Err(crate::services::db_err(error)));
                        return;
                    }
                }
            } else { None };
            if let Some(hold) = loaded.as_mut() { hold.may_execute(); }
            let result: Result<bool, CallError> = plan.db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| {
                    let snapshot = snapshot.clone();
                    let facts = plan.facts.clone();
                    let plan = plan.clone();
                    Box::pin(async move {
                        crate::dao::characters::selection::lock_character(tx, character).await.map_err(crate::services::db_err)?;
                        if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(crate::services::db_err)?
                            || !crate::dao::combat::victim_current(tx, character, ship, source).await.map_err(crate::services::db_err)? {
                            return Err(CallError::Internal(anyhow::anyhow!("ship is no longer available for transfer")));
                        }
                        let written = crate::services::combat::checkpoint_conditions_in(tx, outbox, source, std::slice::from_ref(&snapshot), facts.as_deref()).await?;
                        if snapshot.revision != snapshot.saved_revision && !written.contains(&ship) {
                            return Err(CallError::Internal(anyhow::anyhow!("ship condition was not checkpointed")));
                        }
                        crate::dao::items::ship::position(tx, ship, dest, position).await.map_err(crate::services::db_err)?;
                        if let Some(bounties) = &plan.bounties {
                            bounties.write_in(tx).await?;
                        }
                        if source != dest {
                            crate::dao::characters::ship::location(tx, character, dest, ancestry.0, ancestry.1).await.map_err(crate::services::db_err)?;
                        }
                        if let Some((occurrence, now)) = occurrence {
                            if crate::dao::map_telemetry::append(tx, crate::dao::map_telemetry::EventSource::JumpOccurrence, occurrence, dest, now, crate::dao::map_telemetry::EventKind::Jump).await.map_err(crate::services::db_err)? {
                                crate::dao::map_telemetry::record_visit(tx, character, dest, now).await.map_err(crate::services::db_err)?;
                            }
                            outbox.event_from(crate::db::outbox::SourceKey::new("Jumped", occurrence),
                                crate::lua::events::GameEvent::Jumped { char_id: character, ship_id: ship, from_system_id: source, to_system_id: dest });
                        }
                        Ok(written)
                    })
                }, |written, outbox| {
                    let plan = &plan;
                    let world = &world;
                    let settled_loaded = settled_loaded.clone();
                    async move {
                    if let Some(bounties) = &plan.bounties { let _ = bounties.acknowledge(world).await; }
                    acknowledge_facts(world, source, plan.facts.clone(), written.iter().copied().collect()).await;
                    let mut snapshot = plan.snapshot.clone();
                    if written.contains(&ship) { snapshot.saved_revision = snapshot.revision; }
                    // A known commit publishes exactly the batch this save
                    // wrote; nothing older is replayed onto the new record.
                    let published = plan.publish(&world, prepared, snapshot, settled_loaded).await;
                    crate::db::outbox::dispatch_world(published, outbox, world)
                    }
                },
            ).await;
            if matches!(result, Ok(true)) {
                if let Some(hold) = loaded.take() {
                    crate::services::combat::loaded::commit_disposition(&world, source, hold).await;
                }
                plan.complete_receipt(Ok(()));
                return;
            }
            if let Some(hold) = loaded.take() {
                crate::services::combat::loaded::unknown_disposition(&plan.db, &world, source, hold).await;
            }
            let callback = plan.callback();
            let (token, generation) = (plan.token, plan.generation);
            let pending = world.with(source, move |s| s.mark_pending_transition(ship, token, generation, callback)).await.unwrap_or(false);
            drop(gates);
            drop(market);
            if pending { plan.reconcile().await; }
            else if plan.admission.0.check().is_err() || plan.admission.1.as_ref().is_some_and(|admission| admission.check().is_err()) {
                plan.complete_receipt(Err(CallError::Internal(anyhow::anyhow!(crate::space::containment::ActorRetiring))));
            }
            else { plan.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("system actor stopped during transfer settlement")))); }
        })).await.unwrap_or(false);
        if !submitted { return Err(CallError::Internal(anyhow::anyhow!("system worker unavailable"))); }
        rx.await.map_err(|_| CallError::Internal(anyhow::anyhow!("ship transfer worker unavailable")))?
    }
}
