//! Owned ship boarding in stations and space, assembly, and GM hull creation.
//! Mirrors clientDogmaLocation.MakeShipActive and uses game items, characters and the SDE.
//! Boarding preserves both hulls and their condition; foreign custody and structure access are unsupported.
use crate::services::dogma::ship_state_core;
use crate::services::inventory::notifications::notify_item_change;
use crate::services::{db_err, user_error, value_ids, Call, CallResult};
use crate::dao::items::loading::ItemQuery;
use crate::services::items::load_items;
use crate::dao::items::fitting as fitting_dao;
use crate::client::Client;
use crate::consts::{CATEGORY_SHIP, FLAG_CARGO, FLAG_HANGAR, GROUP_STRATEGIC_CRUISER, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE};
use crate::db::outbox::{dispatch_client, Outbox};
use crate::messages;
use marshal::Value;
use std::collections::BTreeMap;
use tracing::info;

/// Client `appConst.maxBoardingDistance`, in metres of surface separation.
const MAX_BOARDING_DISTANCE: f64 = 6550.0;

/// Board(shipID, oldLocationID) -> shipState.
async fn board(c: &mut Client, ship_id: i64, context: Option<i64>) -> CallResult {
    use crate::services::{combat, effects::{fitting, online}, fit::{Fit, Key}};
    use crate::dao::{characters::selection, items::{boarding, space_ships::FreeShip}};
    let refusal = || crate::services::notify_error(messages::fitting::BOARD_UNAVAILABLE);
    let char_id = c.session.int("charid").ok_or_else(refusal)?;
    let old = c.session.int("shipid").ok_or_else(refusal)?;
    let station = c.session.int("stationid");
    let system = c.session.int("solarsystemid");
    let location = station.or(system).ok_or_else(refusal)?;
    let _lock = crate::services::market::lock().await;
    let selected = selection::selected(&c.db, char_id, c.login.user_id).await.map_err(db_err)?.ok_or_else(refusal)?;
    if selected.station_id != station || (station.is_none() && Some(selected.solar_system_id) != system)
        || selection::ship_id(&c.db, char_id).await.map_err(db_err)? != Some(old) {
        return Err(refusal());
    }
    let ship = load_items(&c.db, &c.sde, ItemQuery::Id(ship_id)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| user_error(messages::MSG_SHIP_NOT_FOUND, vec![]))?;
    if ship.category_id != CATEGORY_SHIP as i64 || ship.owner_id != char_id || ship.location_id != location
        || ship.flag_id != if station.is_some() { FLAG_HANGAR } else { 0 } {
        return Err(if station.is_some() { user_error(messages::MSG_CANT_BOARD_SHIP_NOT_IN_HANGAR, vec![]) } else { refusal() });
    }
    if !ship.singleton || ship.quantity != 1 {
        return Err(user_error(messages::MSG_CANT_BOARD_UNASSEMBLED_SHIP, vec![]));
    }
    if ship_id == old {
        if context != Some(old) { return Err(refusal()); }
        if let Some(system) = system.filter(|_| station.is_none()) {
            if !c.world.with(system, move |s| s.pilot_ball(char_id).is_some_and(|b| b.id == old && b.owner_id == char_id && b.char_id == Some(char_id))).await.unwrap_or(false) {
                return Err(refusal());
            }
        }
        return ship_state_core(c, ship_id).await;
    }
    if context != Some(station.unwrap_or(old)) { return Err(refusal()); }
    let old_ship = load_items(&c.db, &c.sde, ItemQuery::Id(old)).await.map_err(db_err)?.into_iter().next().ok_or_else(refusal)?;
    let snapshot = if station.is_none() {
        let system = system.ok_or_else(refusal)?;
        Some(c.world.with(system, move |s| s.authorize_board(char_id, old, ship_id, MAX_BOARDING_DISTANCE))
            .await.ok_or_else(refusal)?.map_err(|_| refusal())?)
    } else { None };
    let security = if snapshot.is_some() { crate::services::crimewatch::security_status(&c.db, char_id).await? } else { 0.0 };
    let db = c.db.clone();
    let sde = c.sde.clone();
    let captured = snapshot.clone();
    let old_type = old_ship.type_id;
    let target_type = ship.type_id;
    let (prepared, outbox) = db.transaction(
        move |tx, outbox: &mut Outbox<online::OnlineChanged>| {
            let sde = sde.clone();
            let snapshot = captured.clone();
            Box::pin(async move {
                let guarded = if let Some(snap) = &snapshot {
                    boarding::board_in_space(tx, char_id, old, ship_id, location, old_type, target_type, snap.old.pos, snap.target.pos).await
                } else {
                    boarding::board_in_station(tx, char_id, old, ship_id, location, old_type, target_type).await
                }.map_err(db_err)?;
                if !guarded { return Err(refusal()); }
                combat::save_condition_in(tx, old).await?;
                let fit = Fit::load(tx, sde.clone(), char_id, ship_id).await?;
                fitting::required_skills(&fit, &sde, Key::Ship, target_type, false)?;
                online::reconcile_core(tx, &sde, outbox, char_id, ship_id).await?;
                if let Some(snap) = snapshot {
                    let row = |ball: &crate::space::Ball| FreeShip {
                        item_id: ball.id, type_id: ball.type_id, owner_id: ball.owner_id,
                        corporation_id: ball.corp_id, alliance_id: ball.alliance_id, pos: ball.pos,
                    };
                    let old = crate::services::ship::balls::unpiloted(tx, sde.clone(), &row(&snap.old)).await?;
                    let target = crate::services::ship::balls::piloted(tx, sde, &row(&snap.target), char_id, security).await?;
                    Ok(Some((old, target)))
                } else { Ok(None) }
            })
        },
        |prepared, outbox| async move { Ok((prepared, outbox)) },
    ).await?;
    let notification = online::dispatch_changed((), outbox, c);
    let publication = if let (Some(snapshot), Some((old_ball, target_ball))) = (snapshot, prepared) {
        c.world.apply_committed_board(location, c.login.client_id, char_id, snapshot, old_ball, target_ball).await.map_err(db_err)
    } else {
        combat::load_docked_condition(&db, &c.world, char_id, ship_id).await.map(|_| ())
    };
    combat::reset(c);
    c.change_session(vec![("shipid", Value::Int(ship_id))]);
    info!(user = c.login.user_id, "character {char_id} boarded ship {ship_id}");
    publication?;
    notification?;
    if station.is_none() { crate::services::ship::send_state(c).await?; }
    ship_state_core(c, ship_id).await
}

/// Plan one request against a packaged stack; zero in `remaining` marks conversion.
fn assembly_step(original: i32, remaining: Option<i32>) -> Option<(i32, bool, i32)> {
    if remaining == Some(0) { return None; }
    let quantity = remaining.unwrap_or(original);
    if quantity > 1 {
        Some((quantity - 1, true, quantity))
    } else {
        Some((0, false, quantity))
    }
}

fn assembly_source_change(original: i32, remaining: i32) -> Vec<(i64, Value)> {
    let mut change = vec![(IX_STACK_SIZE, Value::Int(original as i64))];
    if remaining == 0 {
        change.push((IX_SINGLETON, Value::Bool(false)));
    }
    change
}

/// AssembleShip(itemIDs, name=, subSystems=) -> the assembled ship's itemID (or IDs).
async fn assemble(c: &mut Client, call: &Call<'_>) -> CallResult {
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
                        sources.insert(id, (item, None::<i32>));
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
                        crate::db::outbox::Effect::Event(event) => crate::lua::on_event(event),
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
    c.world.add_persistent_ship(system, ball).await.map_err(db_err)?;
    Ok(id)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match call.method {
        "Board" => board(c, call.args.first().and_then(|v| v.as_int()).unwrap_or(0), call.args.get(1).and_then(|v| v.as_int())).await,
        "AssembleShip" => assemble(c, call).await,
        "CheckEnoughSubSystemToUndock" => Ok(Value::Bool(true)),
        _ => return None,
    })
}

#[cfg(test)] mod tests;
