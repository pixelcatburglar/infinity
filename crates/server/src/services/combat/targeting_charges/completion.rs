//! Put loaded charges in their modules and tell the client.
use super::*;

pub(super) async fn apply_loads(db: &Db, world: &World, notify: &NotifyTx, ship: i64, mut loads: Vec<PendingLoad>) -> Result<(), CallError> {
    loads.sort_by_key(|load| load.reload_ids.first().copied());
    let mut owners: Vec<_> = loads.iter().map(|load| load.owner).collect();
    owners.sort_unstable();
    owners.dedup();
    let fences = if owners.is_empty() { None } else { Some(online::capture_online_fences(world, &owners).await) };
    let engaged = fences.as_ref().and_then(|fences| fences.membership(ship)).map(|(system, _)| (system, ship)).into_iter().collect::<Vec<_>>();
    let armed = crate::services::combat::loaded::LoadedHolds::arm(world, &engaged, notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    // The completion root follows the same arm, market, gate order as every
    // other business root, including automatic reloads it runs for.
    let _market = crate::services::market::lock().await;
    let mut holds = crate::services::combat::loaded::LoadedHolds::lock(world, armed, notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let settle = holds.settle_rows();
    let sde = world.sde().clone();
    holds.may_execute();
    let result = db.transaction(
        |tx, outbox: &mut Outbox<OnlineChanged>| {
            let loads = loads.clone();
            let notify = notify.clone();
            let fences = fences.clone();
            let sde = sde.clone();
            let settle = settle.clone();
            Box::pin(async move {
                let mut locations: Vec<_> = loads.iter().flat_map(|load| load.external_sources.iter().copied())
                    .filter(|&location| location != ship && !is_static_location(&sde, location)).collect();
                let external = !locations.is_empty();
                let exists = if external {
                    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
                    locations.sort_unstable();
                    locations.dedup();
                    let mut lineages = Vec::new();
                    let mut requests = vec![LockRequest { item_id: ship, strength: LockStrength::Update }];
                    for &location in &locations {
                        let ancestry = crate::dao::items::deliveries::ancestry(tx, location).await.map_err(db_err)?;
                        requests.extend(destination::parent_requests(&sde, ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?);
                        if ancestry.iter().any(|row| row.item_id == location) {
                            requests.push(LockRequest { item_id: location, strength: LockStrength::Update });
                        }
                        lineages.push((location, ancestry));
                    }
                    let locks = destination::lock_items(tx, &sde, &requests, &[]).await?;
                    for (location, ancestry) in &lineages {
                        if crate::dao::items::deliveries::ancestry(tx, *location).await.map_err(db_err)? != *ancestry {
                            return Err(refuse());
                        }
                    }
                    locks.is_write_locked(ship)
                } else {
                    crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?
                };
                // The per-pending `loaded` read below derives `have` from the settled absolute.
                crate::services::combat::loaded::settle_in(tx, &settle).await?;
                let mut entries: Vec<(i64, ReservedLoad)> = Vec::new();
                for pending in loads {
                    let mut quantity = 0_i128;
                    let mut owner = None;
                    for id in pending.reload_ids {
                        match crate::dao::items::pending_reloads::take(tx, id).await.map_err(db_err)? {
                            Some(row) => {
                                if row.owner_id != pending.owner || row.ship_id != ship
                                    || row.module_id != pending.load.0 || row.flag_id != pending.load.1 || row.type_id != pending.load.2
                                    || (external && ((row.source_location_id != ship && !is_static_location(&sde, row.source_location_id)
                                        && !pending.external_facts.iter().any(|fact| fact == &row))
                                    || pending.external_facts.iter().find(|fact| fact.reload_id == id).is_some_and(|fact| *fact != row))) {
                                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                                }
                                if row.source_location_id != ship && !is_static_location(&sde, row.source_location_id)
                                    && !pending.external_sources.contains(&row.source_location_id) {
                                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                                }
                                quantity = quantity.checked_add(i128::from(row.quantity)).ok_or_else(|| CallError::Internal(anyhow::anyhow!("reload quantity overflow")))?;
                                owner = Some(row.owner_id);
                            }
                            None if external => return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)),
                            None => {}
                        }
                    }
                    if !exists || quantity == 0 { continue; }
                    let (module, flag, type_id, _, _) = pending.load;
                    let current = crate::dao::items::loading::load_items(tx, crate::dao::items::loading::ItemQuery::IdAt(module, ship)).await.map_err(db_err)?.into_iter().next();
                    if !current.is_some_and(|item| item.item_id == module && item.owner_id == pending.owner && item.location_id == ship && item.flag_id == flag && crate::services::inventory::is_fitting_flag(item.flag_id)) {
                        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                    }
                    let have = match loaded(tx, ship, flag).await? {
                        None => 0,
                        Some((kind, quantity)) if kind == type_id => quantity,
                        Some(_) => return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)),
                    };
                    let total = i128::from(have).checked_add(quantity).ok_or_else(|| CallError::Internal(anyhow::anyhow!("reload quantity overflow")))?;
                    let now = i64::try_from(total).map_err(|_| CallError::Internal(anyhow::anyhow!("reload quantity overflow")))?;
                    entries.push((owner.unwrap(), (module, flag, type_id, now, have)));
                }
                if !entries.is_empty() {
                    entries.sort_by_key(|(_, load)| load.1);
                    let actual_owner = entries[0].0;
                    let before = if entries.iter().any(|(_, load)| load.4 == 0) {
                        Some(online::fitting::capture(tx, &sde, fences.as_ref().expect("cold candidate fences"), actual_owner, ship).await?)
                    } else { None };
                    let loads: Vec<ReservedLoad> = entries.iter().map(|(_, load)| *load).collect();
                    crate::dao::items::combat::apply_loads(tx, ship, &loads).await.map_err(db_err)?;
                    let touched: Vec<_> = entries.iter().map(|(_, load)| (load.0, load.1)).collect();
                    for (owner, (module, flag, type_id, now, have)) in entries {
                        if have == 0 {
                            outbox.notify_sender(notify.clone(), "OnGodmaPrimeItem", vec![Value::Int(ship), crate::services::dogma::info::charge_info(&sde, ship, flag, type_id, now)]);
                        } else {
                            outbox.notify_sender(notify.clone(), "OnModuleAttributeChanges", vec![quantity_change(owner, ship, flag, type_id, now as i64, have as i64)]);
                        }
                        debug!("loaded {now} x {type_id} into module {module}");
                    }
                    let seeds = crate::services::combat::loaded::seed_entries(tx, ship, &touched).await?;
                    if let Some(before) = before {
                        let final_fit = Fit::load(tx, sde.clone(), actual_owner, ship).await?;
                        online::fitting::finish(tx, before, final_fit, outbox).await?;
                    }
                    return Ok(Some((actual_owner, seeds)));
                }
                Ok(None)
            })
        },
        |value, outbox| async move {
            let character = value.as_ref().map(|(character, _)| *character);
            online::dispatch_mixed_entries(value, outbox, world, Ok, |_| unreachable!(),
                |n| online::dispatch_notification(n, world, notify, character))
        },
    ).await;
    let seeds = match &result { Ok(Some((_, seeds))) => vec![(ship, seeds.clone())], _ => Vec::new() };
    holds.finish(db, world, result, &seeds).await.map(|_| ())
}

