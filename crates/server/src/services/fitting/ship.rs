//! Owned ship assembly and GM hull creation.
//! Uses game items and the SDE; foreign custody and structure access are unsupported.
use crate::services::inventory::notifications::notify_item_change;
use crate::services::{db_err, user_error, value_ids, Call, CallResult};
use crate::dao::items::loading::ItemQuery;
use crate::services::items::load_items;
use crate::dao::items::fitting as fitting_dao;
use crate::client::Client;
use infinity_core::consts::{CATEGORY_SHIP, FLAG_CARGO, FLAG_HANGAR, GROUP_STRATEGIC_CRUISER, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE};
use crate::db::outbox::{dispatch_client, Outbox};
use infinity_core::messages;
use marshal::Value;
use std::collections::BTreeMap;

/// Plan one request against a packaged stack; zero in `remaining` marks conversion.
fn assembly_step(original: i64, remaining: Option<i64>) -> Option<(i64, bool, i64)> {
    if remaining == Some(0) { return None; }
    let quantity = remaining.unwrap_or(original);
    if quantity > 1 {
        Some((quantity - 1, true, quantity))
    } else {
        Some((0, false, quantity))
    }
}

fn assembly_source_change(original: i64, remaining: i64) -> Vec<(i64, Value)> {
    let mut change = vec![(IX_STACK_SIZE, Value::Int(original))];
    if remaining == 0 {
        change.push((IX_SINGLETON, Value::Bool(false)));
    }
    change
}

/// AssembleShip(itemIDs, name=, subSystems=) -> the assembled ship's itemID (or IDs).
pub(in crate::services) async fn assemble(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ids = value_ids(call.args.first());
    let name = call.kwargs.get("name").and_then(|v| v.as_text()).map(|t| t.into_owned());
    let assembled = assemble_ships(c, ids, name).await?;
    Ok(match assembled.as_slice() {
        [one] => Value::Int(*one),
        _ => Value::list(assembled.into_iter().map(Value::Int)),
    })
}

/// Assemble packaged hulls with the same transaction and notifications as AssembleShip.
pub(in crate::services) async fn assemble_ships(c: &mut Client, ids: Vec<i64>, name: Option<String>) -> Result<Vec<i64>, crate::services::CallError> {
    let char_id = c.session.int("charid").unwrap_or(0);
    let station = c.session.int("stationid");
    let active_ship = c.session.int("shipid");
    let db = c.db.clone();
    let sde = std::sync::Arc::clone(&c.sde);
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let (ids, name) = (ids.clone(), name.clone());
            let sde = std::sync::Arc::clone(&sde);
            Box::pin(async move {
                assemble_core(tx, &sde, outbox, char_id, station, active_ship, ids, name).await
            })
        },
        |assembled, outbox| async move { dispatch_client(assembled, outbox, c) },
    ).await
}

async fn assemble_core(
    tx: &(impl crate::db::executor::Executor + ?Sized), sde: &crate::sde::Sde,
    outbox: &mut Outbox, char_id: i64, station: Option<i64>, active_ship: Option<i64>,
    ids: Vec<i64>, name: Option<String>,
) -> Result<Vec<i64>, crate::services::CallError> {
                // Simulate requests in their original order, but write each source once
                // in primary-key order before inserting any generated hulls.
                let mut sources = BTreeMap::new();
                let mut actions = Vec::new();
                for id in ids {
                    if !sources.contains_key(&id) {
                        let item = load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
                            .ok_or_else(|| crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                        let valid_source = if let Some(station) = station {
                            item.location_id == station && item.flag_id == FLAG_HANGAR
                        } else if Some(item.location_id) == active_ship && item.flag_id == FLAG_CARGO {
                            let carrier = load_items(tx, &sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next();
                            carrier.is_some_and(|carrier| carrier.owner_id == char_id && carrier.category_id == CATEGORY_SHIP as i64)
                        } else {
                            false
                        };
                        if item.owner_id != char_id || !valid_source {
                            return Err(crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                        }
                        if station.is_none() && item.group_id == GROUP_STRATEGIC_CRUISER && !item.singleton {
                            return Err(user_error(messages::MSG_CANT_ASSEMBLE_MODULAR_SHIP_IN_SPACE, vec![]));
                        }
                        sources.insert(id, (item, None::<i64>));
                    }
                    let Some((item, remaining)) = sources.get_mut(&id) else { continue };
                    if item.category_id != CATEGORY_SHIP as i64 || item.singleton {
                        continue;
                    }
                    if let Some((next, split, _)) = assembly_step(item.quantity, *remaining) {
                        *remaining = Some(next);
                        actions.push((id, split));
                    }
                }
                if !fitting_dao::assemble_sources(tx, &sources, char_id, &name).await.map_err(db_err)? {
                    return Err(crate::services::notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
                }
                let mut assembled = Vec::new();
                let mut notified = std::collections::BTreeSet::new();
                for (id, split) in actions {
                    let (item, remaining) = &sources[&id];
                    if notified.insert(id) {
                        notify_item_change(tx, &sde, outbox, id, assembly_source_change(item.quantity, remaining.unwrap())).await?;
                    }
                    let ship_id = if split {
                        let new_id = fitting_dao::insert_assembled(tx, item, char_id, &name).await.map_err(db_err)?;
                        notify_item_change(tx, &sde, outbox, new_id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                        new_id
                    } else {
                        id
                    };
                    assembled.push(ship_id);
                }
                Ok(assembled)
}

/// Create and assemble a hull atomically in the target character's station hangar.
pub(in crate::services) async fn gm_create_ship(c: &mut Client, owner: i64, type_id: i32) -> Result<i64, crate::services::CallError> {
    if c.sde.types.get(type_id).and_then(|t| t.category_id) != Some(CATEGORY_SHIP) {
        return Err(crate::services::notify_error(messages::gm::GM_SHIP_TYPE_REQUIRED));
    }
    let db = c.db.clone();
    let sde = c.sde.clone();
    let caller = c.session.int("charid");
    let world = c.world.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
                let station = crate::dao::items::transfers::docked(tx, owner).await.map_err(db_err)?
                    .ok_or_else(|| crate::services::notify_error(messages::gm::GM_SHIP_DOCKED_REQUIRED))?;
                let id = crate::dao::items::transfers::insert(tx, type_id, owner, station, FLAG_HANGAR, 1).await.map_err(db_err)?;
                notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                let assembled = assemble_core(tx, &sde, outbox, owner, Some(station), None, vec![id], None).await?;
                assembled.into_iter().next().ok_or_else(|| crate::services::notify_error(messages::gm::GM_SHIP_TYPE_REQUIRED))
            })
        },
        |id, outbox| async move {
            if caller == Some(owner) {
                dispatch_client(id, outbox, c)
            } else {
                for entry in outbox.into_entries() {
                    match entry {
                        crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::DirectClient { event, args }) => world.notify_pilot(owner, &event, args),
                        crate::db::outbox::Effect::Notification(notification) => notification.dispatch(&world, Some(c))?,
                        crate::db::outbox::Effect::Operation(never) => match never {},
                    }
                }
                Ok(id)
            }
        },
    ).await
}

/// Create a caller-owned assembled hull without changing the active ship.
pub(in crate::services) async fn gm_spawn_ship(c: &mut Client, type_id: i32) -> Result<i64, crate::services::CallError> {
    let owner = c.session.int("charid").ok_or_else(|| db_err(anyhow::anyhow!("missing spawn character")))?;
    if c.session.int("stationid").is_some() { return gm_create_ship(c, owner, type_id).await; }
    if c.sde.types.get(type_id).and_then(|t| t.category_id) != Some(CATEGORY_SHIP) {
        return Err(crate::services::notify_error(messages::gm::GM_SHIP_TYPE_REQUIRED));
    }
    let system = c.session.int("solarsystemid").ok_or_else(|| db_err(anyhow::anyhow!("missing spawn system")))?;
    let active = c.session.int("shipid").ok_or_else(|| db_err(anyhow::anyhow!("missing active ship")))?;
    let _lock = crate::services::market::lock().await;
    let pilot = c.world.with(system, move |s| {
        let id = s.pilot_ball(owner)?.id;
        if id != active { return None; }
        s.sampled_ball(id)
    }).await.flatten().ok_or_else(|| db_err(anyhow::anyhow!("active pilot not present in system")))?;
    let radius = crate::services::ship::balls::radius(&c.sde, type_id)?;
    let distance = crate::config::game().gm.spawn_distance_m + f64::from(pilot.radius) + f64::from(radius);
    let pos = crate::space::add(pilot.pos, crate::space::scale(crate::services::rand_dir(), distance));
    let db = c.db.clone();
    let id = db.transaction(
        move |tx, _: &mut Outbox| Box::pin(async move {
            crate::dao::items::creation::positioned(tx, type_id, owner, system, pos).await.map_err(db_err)
        }),
        |id, _| async move { Ok(id) },
    ).await?;
    let row = crate::dao::items::space_ships::FreeShip {
        item_id: id, type_id, owner_id: owner, corporation_id: pilot.corp_id,
        alliance_id: pilot.alliance_id, pos,
    };
    let ball = crate::services::ship::balls::unpiloted(&db, c.sde.clone(), &row).await?;
    c.world.add_persistent_ship_locked(system, ball, &_lock).await.map_err(db_err)?;
    Ok(id)
}


#[cfg(test)] mod tests;
