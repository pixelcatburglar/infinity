//! Manual and automatic ammunition reload transaction and scheduling.
//! Client-initiated manual reloads hold the market lock through dispatch, including GM reloads.
use super::*;
pub(super) async fn reload(db: &Db, world: &Arc<World>, notify: &NotifyTx, sde: Arc<Sde>, ship: i64,
    modules: Vec<i64>, charge_ids: Vec<i64>, charge_location: Option<i64>, active_ship: Option<i64>,
    station: Option<i64>, session_owner: Option<i64>, system: Option<i64>, automatic: bool,
    automatic_reload_ms: Option<f64>, generated: Option<(i32, i64)>, mut client: Option<&mut Client>) -> Result<usize, CallError> {
    let owner = session_owner.unwrap_or(0);
    let db = db.clone();
    let world = world.clone();
    let notify = notify.clone();
    let _market = if !automatic && client.is_some() {
        Some(crate::services::market::lock().await)
    } else { None };
    let selected = if !automatic && generated.is_none() {
        if let Some(client) = client.as_deref() {
            let identity = crate::services::identity::Identity { system_id: system, ..crate::services::identity::Identity::of_client(client) };
            let rows = load_items(&db, &sde, ItemQuery::Ids(&charge_ids)).await.map_err(db_err)?;
            let mut endpoints: Vec<_> = rows.iter().map(|item| crate::services::inventory::reachability::Endpoint { location: item.location_id, flag: item.flag_id }).collect();
            endpoints.push(crate::services::inventory::reachability::Endpoint { location: ship, flag: FLAG_CARGO });
            let reach = crate::services::inventory::reachability::Reachability::capture(&identity, &db, &world, system, &endpoints).await?;
            let audience = crate::services::inventory::notifications::Audience::capture(&identity, &db, &world, system, &endpoints).await?;
            Some((identity, reach, audience))
        } else { None }
    } else { None };
    let fences = online::capture_online_fences(&world, &[owner]).await;
    let loads = db.transaction(
        |tx, outbox: &mut Outbox<OnlineChanged>| {
            let modules = modules.clone();
            let mut charge_ids = charge_ids.clone();
            let sde = sde.clone();
            let selected = selected.clone();
            let fences = fences.clone();
            Box::pin(async move {
                let refuse = || notify_error(messages::INSUFFICIENT_AMMUNITION);
                let source = charge_location.ok_or_else(refuse)?;
                let snapshot = crate::dao::items::pending_reloads::for_ship(tx, ship).await.map_err(db_err)?;
                let owner = session_owner.ok_or_else(refuse)?;
                let (locks, mut refund, planned_candidates, hull) = if automatic {
                    if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(refuse()); }
                    let raw = crate::dao::items::loading::load_items(tx, ItemQuery::Id(ship)).await.map_err(db_err)?
                        .into_iter().next().ok_or_else(refuse)?;
                    let locks = LockedItems::held_hull(tx, &raw);
                    let hull = crate::services::items::enrich_item(&sde, raw).ok_or_else(refuse)?;
                    (locks, None, Vec::new(), hull)
                } else {
                let mut planned_modules = Vec::new();
                let mut planned_banks = Vec::new();
                for &m in &modules {
                    let master = if generated.is_some() { master_for_member(tx, ship, m).await? } else { m };
                    let ids = bank(tx, ship, master).await?;
                    for &id in &ids { if !planned_modules.contains(&id) { planned_modules.push(id); } }
                    planned_banks.push((m, master, ids));
                }
                let planned_fitted = load_items(tx, &sde, ItemQuery::Ids(&planned_modules)).await.map_err(db_err)?;
                if planned_modules.iter().any(|id| !planned_fitted.iter().any(|item| item.item_id == *id)) { return Err(refuse()); }
                let mut planned_charges = Vec::new();
                let mut planned_candidates = Vec::new();
                for module in &planned_fitted {
                    let charge = loaded(tx, ship, module.flag_id).await?;
                    if let Some((type_id, quantity)) = charge {
                        if quantity > 0 {
                            if let Some(item) = load_items(tx, &sde, ItemQuery::LocationFlagTypeOwnerStack(ship, FLAG_CARGO, type_id, owner)).await.map_err(db_err)?.into_iter().next() {
                                planned_candidates.push(item);
                            }
                        }
                    }
                    planned_charges.push((module.flag_id, charge));
                }
                let planned_stacks = if automatic {
                    if let Some(module) = planned_fitted.first() {
                        load_items(tx, &sde, ItemQuery::OwnerLocationFlag(owner, ship, FLAG_CARGO)).await.map_err(db_err)?
                            .into_iter().filter(|item| item.quantity > 0 && compatible_charge(&sde, module.type_id, item)).collect::<Vec<_>>()
                    } else { Vec::new() }
                } else { load_items(tx, &sde, ItemQuery::Ids(&charge_ids)).await.map_err(db_err)? };
                let planned_flags: Vec<_> = planned_fitted.iter().map(|item| item.flag_id).collect();
                let planned_older: Vec<_> = snapshot.iter().filter(|row| planned_flags.contains(&row.flag_id)).cloned().collect();
                let refund = crate::services::combat::reload_recovery::RefundPreparation::capture_pending(tx, &sde, &planned_older).await?;
                let mut locations: Vec<_> = snapshot.iter().map(|row| row.source_location_id).collect();
                locations.extend([ship, source]);
                locations.sort_unstable(); locations.dedup();
                let captured_locations = crate::dao::items::parent_locks::capture(tx, &locations).await.map_err(db_err)?;
                if !captured_locations.iter().any(|row| row.item_id == ship) { return Err(refuse()); }
                let mut lineages = Vec::new();
                for &location in &locations {
                    lineages.push((location, crate::dao::items::deliveries::ancestry(tx, location).await.map_err(db_err)?));
                }
                let mut requests: Vec<_> = captured_locations.iter().map(|row| LockRequest { item_id: row.item_id, strength: LockStrength::Update }).collect();
                for (_, ancestry) in &lineages {
                    requests.extend(destination::parent_requests(&sde, ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?);
                }
                requests.extend(planned_fitted.iter().map(|item| LockRequest { item_id: item.item_id, strength: LockStrength::Share }));
                requests.extend(planned_stacks.iter().chain(&planned_candidates).map(|item| LockRequest { item_id: item.item_id, strength: LockStrength::Update }));
                let mut mandatory: Vec<_> = requests.iter().map(|request| request.item_id).collect();
                mandatory.extend(refund.mandatory_ids());
                let optional: Vec<_> = refund.candidate_ids().into_iter().filter(|id| !mandatory.contains(id)).collect();
                requests.extend_from_slice(refund.requests());
                let locks = destination::lock_items(tx, &sde, &requests, &optional).await?;
                let current_locations = crate::dao::items::parent_locks::capture(tx, &locations).await.map_err(db_err)?;
                if current_locations.len() != captured_locations.len() || captured_locations.iter().any(|old| {
                    !current_locations.iter().any(|row| row.item_id == old.item_id && row.type_id == old.type_id
                        && row.owner_id == old.owner_id && row.location_id == old.location_id && row.flag_id == old.flag_id
                        && row.quantity == old.quantity && row.singleton == old.singleton && row.jump_clone_id == old.jump_clone_id)
                }) { return Err(refuse()); }
                if crate::dao::items::pending_reloads::for_ship(tx, ship).await.map_err(db_err)? != snapshot { return Err(refuse()); }
                for (location, ancestry) in &lineages {
                    if crate::dao::items::deliveries::ancestry(tx, *location).await.map_err(db_err)? != *ancestry { return Err(refuse()); }
                }
                for (m, master, ids) in &planned_banks {
                    if (generated.is_some() && master_for_member(tx, ship, *m).await? != *master) || bank(tx, ship, *master).await? != *ids { return Err(refuse()); }
                }
                for (flag, charge) in &planned_charges {
                    if loaded(tx, ship, *flag).await? != *charge { return Err(refuse()); }
                }
                for item in planned_fitted.iter().chain(&planned_stacks).chain(&planned_candidates) {
                    let row = locks.row(item.item_id).ok_or_else(refuse)?;
                    if row.type_id != item.type_id || row.owner_id != item.owner_id || row.location_id != item.location_id
                        || row.flag_id != item.flag_id || row.quantity != item.quantity || row.singleton != item.singleton { return Err(refuse()); }
                }
                refund.revalidate(tx, &sde, &locks).await?;
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?
                    .into_iter().next().ok_or_else(refuse)?;
                (locks, Some(refund), planned_candidates, hull)
                };
                if hull.owner_id != owner || hull.category_id != infinity_core::consts::CATEGORY_SHIP as i64
                    || (active_ship != Some(ship) && !(hull.singleton && station == Some(hull.location_id) && hull.flag_id == FLAG_HANGAR)) {
                    return Err(refuse());
                }
                let source_flags: &[i32] = if station == Some(source) {
                    &[FLAG_HANGAR]
                } else {
                    let container = load_items(tx, &sde, ItemQuery::Id(source)).await.map_err(db_err)?
                        .into_iter().next().ok_or_else(refuse)?;
                    if !container.singleton || (selected.is_none() && container.owner_id != owner) {
                        return Err(refuse());
                    }
                    let is_ship = container.category_id == infinity_core::consts::CATEGORY_SHIP as i64;
                    let in_hangar = station == Some(container.location_id) && container.flag_id == FLAG_HANGAR;
                    let is_active_ship = is_ship && source == ship && active_ship == Some(ship);
                    if selected.is_none() && !in_hangar && !is_active_ship
                        && (is_ship || container.location_id != ship
                            || !matches!(container.flag_id, FLAG_CARGO | FLAG_FLEET_HANGAR)) {
                        return Err(refuse());
                    }
                    if is_ship { &[FLAG_CARGO, FLAG_SPECIALIZED_AMMO_HOLD, FLAG_FLEET_HANGAR] } else { &[FLAG_HANGAR] }
                };
                let mut inserted_ids = Vec::new();
                if let Some((type_id, quantity)) = generated {
                    if quantity <= 0 { return Err(refuse()); }
                    let flag = if station == Some(source) { FLAG_HANGAR } else { FLAG_CARGO };
                    crate::services::inventory::hold_storage::check_destination(tx, &sde, owner, source, flag,
                        &[crate::services::inventory::hold_storage::Addition { type_id, singleton: false, quantity: i64::from(quantity) }], Default::default(), None).await?;
                    let id = crate::dao::items::transfers::insert(tx, type_id, owner, source, flag, quantity).await.map_err(db_err)?;
                    notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                    inserted_ids.push(id);
                    charge_ids.push(id);
                }
                let mut all = Vec::new();
                for m in modules {
                    let master = if generated.is_some() { master_for_member(tx, ship, m).await? } else { m };
                    for id in bank(tx, ship, master).await? {
                        if !all.contains(&id) { all.push(id); }
                    }
                }
                let fitted = load_items(tx, &sde, ItemQuery::Ids(&all)).await.map_err(db_err)?;
                if all.iter().any(|id| !fitted.iter().any(|item| item.item_id == *id && item.owner_id == owner && item.location_id == ship && crate::services::inventory::is_fitting_flag(item.flag_id))) {
                    return Err(refuse());
                }
                let mut target_flags: Vec<i32> = fitted.iter().map(|item| item.flag_id).collect();
                target_flags.sort_unstable();
                target_flags.dedup();
                let older = crate::dao::items::pending_reloads::for_flags(tx, ship, &target_flags).await.map_err(db_err)?;
                if older.iter().any(|row| !snapshot.iter().any(|old| old.reload_id == row.reload_id && old.source_location_id == row.source_location_id && old.source_flag == row.source_flag)) {
                    return Err(refuse());
                }
                if automatic {
                    if !older.is_empty() { return Ok(Vec::new()); }
                    let Some(module) = fitted.first() else { return Ok(Vec::new()) };
                    if loaded(tx, ship, module.flag_id).await?.is_some_and(|(_, quantity)| quantity > 0) {
                        return Ok(Vec::new());
                    }
                }
                if let Some(refund) = &mut refund {
                    let older_ids: Vec<i64> = older.into_iter().map(|row| row.reload_id).collect();
                    let receipts = crate::services::combat::reload_recovery::return_pending(tx, &sde, outbox, &older_ids, true, &locks, refund).await?;
                    inserted_ids.extend(receipts.into_iter().filter_map(|receipt| receipt.created.map(|row| row.item.item_id)));
                }
                let stacks = if automatic {
                    let module = &fitted[0];
                    load_items(tx, &sde, ItemQuery::OwnerLocationFlag(owner, ship, FLAG_CARGO)).await.map_err(db_err)?
                        .into_iter().filter(|item| item.quantity > 0 && compatible_charge(&sde, module.type_id, item)).collect()
                } else {
                    load_items(tx, &sde, ItemQuery::Ids(&charge_ids)).await.map_err(db_err)?
                };
                if !automatic && stacks.iter().any(|item| !locks.is_write_locked(item.item_id) && !inserted_ids.contains(&item.item_id)) { return Err(refuse()); }
                if selected.is_some() {
                    let mut guarded = charge_ids.clone();
                    guarded.push(source);
                    if !crate::dao::trades::reserved(tx, &guarded).await.map_err(db_err)?.is_empty()
                        || crate::dao::industry::has_active_blueprint_job(tx, &charge_ids).await.map_err(db_err)? {
                        return Err(refuse());
                    }
                }
                if charge_ids.iter().any(|id| !stacks.iter().any(|item| item.item_id == *id && (selected.is_some() || item.owner_id == owner) && item.location_id == source && source_flags.contains(&item.flag_id) && item.quantity > 0)) {
                    return Err(refuse());
                }
                let viewers = if let Some((identity, reach, audience)) = &selected {
                    let destination = crate::services::inventory::access::Container::Item(ship);
                    let proofs = crate::services::inventory::movement::authorize_moves(tx, &sde, identity, reach, &stacks, Some(source), &destination, Some(FLAG_CARGO)).await?.ok_or_else(refuse)?;
                    if stacks.iter().zip(proofs).any(|(item, (_, custody))| item.owner_id != owner && custody != crate::services::inventory::destination::Custody::TakeSubtree) {
                        return Err(refuse());
                    }
                    Some(audience.resolve(tx, &sde).await?)
                } else { None };
                let selected: Vec<i64> = if automatic { stacks.iter().map(|item| item.item_id).collect() } else { charge_ids.clone() };
                let mut items: BTreeMap<i64, (Item, i64)> = stacks.into_iter().map(|item| (item.item_id, { let quantity = item.quantity; (item, quantity) })).collect();
                let mut inserts: BTreeMap<(i64, i32), i64> = BTreeMap::new();
                let mut unloads = BTreeMap::new();
                let mut loads = Vec::new();
                let mut flags = HashSet::new();
                for module_id in all {
                    let module = fitted.iter().find(|item| item.item_id == module_id).ok_or_else(refuse)?;
                    if !flags.insert(module.flag_id) { continue; }
                    let Some(charge) = selected.iter().find_map(|id| items.get(id).filter(|(_, q)| *q > 0).map(|(item, _)| item)) else { break };
                    if !compatible_charge(&sde, module.type_id, charge) { return Err(refuse()); }
                    let type_id = charge.type_id;
                    let module_type = sde.types.get(module.type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown type {}", module.type_id)))?;
                    let capacity = module_type.capacity.unwrap_or(0.0).max(module_type.attribute(ATTR_CAPACITY).unwrap_or(0.0));
                    let volume = sde.types.get(type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown type {type_id}")))?.volume.unwrap_or(0.01);
                    let max = (capacity / volume.max(1e-6) + 1e-6).floor() as i64;
                    if max <= 0 { continue; }
                    let have = match loaded(tx, ship, module.flag_id).await? {
                        Some((t, q)) if t == type_id => q,
                        Some((t, q)) => {
                            let unload_owner = session_owner.unwrap_or(module.owner_id);
                            unloads.insert(module.flag_id, (t, q, unload_owner));
                            if q > 0 {
                                let destination = if automatic {
                                    load_items(tx, &sde, ItemQuery::LocationFlagTypeOwnerStack(ship, FLAG_CARGO, t, unload_owner)).await.map_err(db_err)?.into_iter().next()
                                } else if let Some(old) = planned_candidates.iter().find(|item| item.type_id == t && item.owner_id == unload_owner) {
                                    load_items(tx, &sde, ItemQuery::Id(old.item_id)).await.map_err(db_err)?.into_iter().next()
                                } else { None };
                                if let Some(item) = destination {
                                    let entry = items.entry(item.item_id).or_insert_with(|| { let quantity = item.quantity; (item, quantity) });
                                    entry.1 = entry.1.checked_add(q).ok_or_else(|| notify_error(messages::INSUFFICIENT_AMMUNITION))?;
                                } else {
                                    let entry = inserts.entry((unload_owner, t)).or_default();
                                    *entry = entry.checked_add(q).ok_or_else(|| notify_error(messages::INSUFFICIENT_AMMUNITION))?;
                                }
                            }
                            0
                        }
                        None => 0,
                    };
                    if let Some((now, sources)) = reserve_load(&mut items, &selected, type_id, max, have) {
                        loads.push(((module_id, module.flag_id, type_id, now, have), sources));
                    }
                }
                let mut cargo_additions = Vec::new();
                let mut cargo_excluded = Vec::new();
                for (&id, (item, quantity)) in &items {
                    if item.location_id == ship && item.flag_id == FLAG_CARGO && *quantity != item.quantity {
                        cargo_excluded.push(id);
                        if *quantity > 0 {
                            cargo_additions.push(crate::services::inventory::hold_storage::Addition {
                                type_id: item.type_id, singleton: item.singleton, quantity: i64::from(*quantity),
                            });
                        }
                    }
                }
                for (&(_, type_id), &quantity) in &inserts {
                    if quantity > 0 {
                        cargo_additions.push(crate::services::inventory::hold_storage::Addition {
                            type_id, singleton: false, quantity: i64::from(quantity),
                        });
                    }
                }
                if source == ship {
                    for ((_, _, type_id, _, _), sources) in &loads {
                        if let Some(&quantity) = sources.get(&(ship, FLAG_CARGO)) {
                            cargo_additions.push(crate::services::inventory::hold_storage::Addition {
                                type_id: *type_id, singleton: false, quantity: i64::from(quantity),
                            });
                        }
                    }
                }
                crate::services::inventory::hold_storage::check_destination(
                    tx, &sde, owner, ship, FLAG_CARGO, &cargo_additions, crate::services::inventory::hold_storage::Replaced { items: &cargo_excluded, reloads: &[] }, None,
                ).await?;
                let changes = items.iter().filter_map(|(&id, (item, quantity))| {
                    (*quantity != item.quantity).then(|| (id, QuantityChange::new(item, (*quantity > 0).then_some(*quantity))))
                }).collect();
                if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_AMMUNITION)); }
                let before = if unloads.is_empty() { None } else {
                    Some(online::fitting::capture(tx, &sde, &fences, owner, ship).await?)
                };
                if !crate::dao::items::combat::delete_reserved(tx, ship, &unloads).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_AMMUNITION)); }
                for (&flag, &(type_id, quantity, unload_owner)) in &unloads {
                    outbox.notify_client("OnModuleAttributeChanges", vec![quantity_change(unload_owner, ship, flag, type_id, 0, quantity as i64)]);
                }
                for (id, (item, quantity)) in items {
                    if quantity == item.quantity { continue; }
                    if let Some(viewers) = &viewers {
                        let new = if quantity == 0 {
                            Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..item.clone() }
                        } else { Item { quantity, ..item.clone() } };
                        viewers.item_change(outbox, &item, &new, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))], owner);
                    }
                    if quantity == 0 { notify_item_removed(outbox, item); }
                    else { notify_item_change(tx, &sde, outbox, id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?; }
                }
                let ids = crate::dao::items::combat::insert_cargo(tx, ship, FLAG_CARGO, &inserts).await.map_err(db_err)?;
                for id in ids {
                    notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(ship)), (IX_FLAG, Value::Int(FLAG_CARGO as i64))]).await?;
                }
                let mut pending = Vec::with_capacity(loads.len());
                for (load, sources) in loads {
                    let (module, flag, type_id, _, _) = load;
                    let mut reload_ids = Vec::with_capacity(sources.len());
                    let mut external_sources = Vec::new();
                    let mut external_facts = Vec::new();
                    for ((source_location, source_flag), quantity) in sources {
                        let external = source_location != ship && !is_static_location(&sde, source_location);
                        if external && !external_sources.contains(&source_location) { external_sources.push(source_location); }
                        let reload_id = crate::dao::items::pending_reloads::insert(tx, owner, ship, module, flag, type_id, quantity, source_flag, source_location).await.map_err(db_err)?;
                        if external {
                            external_facts.push(crate::dao::items::pending_reloads::PendingReload { reload_id, owner_id: owner, ship_id: ship, module_id: module, flag_id: flag, type_id, quantity, source_flag, source_location_id: source_location });
                        }
                        reload_ids.push(reload_id);
                    }
                    pending.push(PendingLoad { owner, reload_ids, external_sources, external_facts, load });
                }
                if let Some(before) = before {
                    let final_fit = Fit::load(tx, sde.clone(), owner, ship).await?;
                    online::fitting::finish(tx, before, final_fit, outbox).await?;
                }
                Ok(pending)
            })
        },
        |value, outbox| {
            let world = &world;
            let notify = &notify;
            async move {
                online::dispatch_mixed_entries(value, outbox, world, Ok, |_| unreachable!(),
                    |n| online::dispatch_notification(n, world, notify, Some(owner)))
            }
        },
    ).await?;
    drop(_market);
    let count = loads.len();
    if loads.is_empty() { return Ok(0); }
    let Some(system) = system else {
        apply_loads(&db, &world, &notify, ship, loads).await?;
        return Ok(count);
    };
    let groups = if let Some(ms) = automatic_reload_ms {
        let type_id = loads[0].load.2;
        BTreeMap::from([((filetime_ticks_from_ms(ms), type_id), loads.into_iter().map(|load| (load, ms)).collect())])
    } else {
        let fit = Fit::load_in_space(&db, &world, system, owner, ship).await?;
        if generated.is_some() {
            group_loads(loads, |module| fit.value(Key::Module(module), ATTR_RELOAD_TIME))
        } else {
            let first = loads[0].load;
            let ms = match fit.value(Key::Module(first.0), ATTR_RELOAD_TIME) {
                ms if ms > 0.0 => ms,
                _ => DEFAULT_RELOAD_MS,
            };
            BTreeMap::from([((filetime_ticks_from_ms(ms), first.2), loads.into_iter().map(|load| (load, ms)).collect())])
        }
    };
    let start = filetime_now();
    for ((ticks, type_id), loads) in groups {
        let until = start + ticks;
        let reload_ms = loads[0].1;
        let loads: Vec<_> = loads.into_iter().map(|(load, _)| load).collect();
        let ids: Vec<i64> = loads.iter().map(|l| l.load.0).collect();
        {
            let mut st = state();
            for id in &ids { st.reloading.insert(*id, until); }
        }
        // (moduleIDs, chargeTypeID, reloadTime ms): the HUD's reload ramp.
        let ramp = vec![Value::list(ids.iter().map(|i| Value::Int(*i))), Value::Int(type_id as i64), Value::Int(reload_ms as i64)];
        let timers = ids.iter().map(|i| attribute_change(owner, Value::Int(*i), ATTR_NEXT_ACTIVATION_TIME, Value::Int(until), Value::Int(start))).collect();
        if let Some(client) = client.as_deref_mut() {
            client.notify("OnChargeBeingLoadedToModule", ramp);
            client.notify("OnModuleAttributeChanges", vec![Value::List(timers)]);
        } else {
            let _ = notify.send(("OnChargeBeingLoadedToModule".into(), ramp));
            let _ = notify.send(("OnModuleAttributeChanges".into(), vec![Value::List(timers)]));
        }
        let timer_world = world.clone();
        let db = db.clone();
        let notify = notify.clone();
        let deadline = crate::client::unix_ms_from_filetime_ceil(until);
        world.post(system, move |s| {
            s.schedule_at(deadline, move |s| {
                s.defer(async move {
                    if let Err(e) = apply_loads(&db, &timer_world, &notify, ship, loads).await {
                        warn!("reload of {ids:?} failed: {}", match e {
                            CallError::Internal(e) => format!("{e:#}"),
                            CallError::User { msg, .. } => msg,
                        });
                    }
                    let mut st = state();
                    clear_reload_markers(&mut st.reloading, &ids, until);
                });
            });
        });
    }
    Ok(count)
}
