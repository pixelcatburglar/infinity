//! Stored ship transitions through the location-bound `ship` service.
//! `menuFunctions.py` and `invItemFunctions.py` supply the host and stored item;
//! game items and sampled system state determine custody and physical access.
//! Client wrappers ignore the return; successful selection returns ship state,
//! and launch returns the standard empty mutator result.
//! As with undock, an uncertain post-commit outcome while the actor remains
//! alive can leave a persisted ship invisible until the system reloads.
use super::{Call, CallError, Client};
use infinity_core::consts::{CATEGORY_SHIP, FLAG_SHIP_HANGAR, GROUP_CAPSULE, TYPE_CAPSULE};
use crate::dao::{characters::selection, items::loading::ItemQuery};
use crate::services::{db_err, notify_error, identity::Identity, items::load_items};
use crate::space::{Ball, MODE_WARP};

fn unavailable() -> CallError { notify_error(infinity_core::messages::fitting::BOARD_UNAVAILABLE) }

struct Current {
    identity: Identity,
    system: i64,
    old: Ball,
    transition: Option<crate::space::system::boarding::CombatTransition>,
    host: Ball,
    corporation: i64,
    alliance: i64,
}

async fn current(c: &Client, call: &Call<'_>, host: i64, target: Option<i64>, reserve: bool,
    expected: Option<infinity_core::session::OnlineSession>) -> Result<Current, CallError> {
    let (station, system) = super::configuration::current_binding(c, call).ok_or_else(unavailable)?;
    if station.is_some() || host <= 0 { return Err(unavailable()); }
    let identity = Identity { system_id: Some(system), ..Identity::of_client(c) };
    let character = identity.char_id;
    let old = identity.ship_id.filter(|id| *id > 0).ok_or_else(unavailable)?;
    if character <= 0 { return Err(unavailable()); }
    let selected = selection::selected(&c.db, character, c.login.user_id).await.map_err(db_err)?.ok_or_else(unavailable)?;
    if selected.station_id.is_some() || selected.solar_system_id != system
        || selection::ship_id(&c.db, character).await.map_err(db_err)? != Some(old) { return Err(unavailable()); }
    let (old, transition, host) = c.world.with(system, move |space| {
        if !space.combat_session_current(character, expected.as_ref()) { return None; }
        if target.is_some_and(|id| space.ball(id).is_some()) { return None; }
        let pilot = space.pilot_ball(character).filter(|ball| ball.id == old && ball.owner_id == character && ball.char_id == Some(character))?;
        if pilot.mode == MODE_WARP || pilot.cloak != 0 { return None; }
        space.persistent_generation(old)?;
        let old = space.sampled_ball(old)?;
        let host = space.sampled_ball(host)?;
        if host.mode == MODE_WARP || host.cloak != 0 || !space.visible_to(character, host.id)
            || !old.pos.iter().chain(host.pos.iter()).all(|value| value.is_finite()) { return None; }
        let transition = if reserve { Some(space.begin_condition_transitions(&[old.id])?) } else { None };
        Some((old, transition, host))
    }).await.flatten().ok_or_else(unavailable)?;
    Ok(Current { identity, system, old, transition, host, corporation: selected.corporation_id, alliance: selected.alliance_id.unwrap_or(-1) })
}

async fn stored_item(c: &Client, host: i64, item: i64) -> Result<crate::dao::items::loading::Item, CallError> {
    let item = load_items(&c.db, &c.sde, ItemQuery::Id(item)).await.map_err(db_err)?.pop().ok_or_else(unavailable)?;
    if item.location_id != host || item.flag_id != FLAG_SHIP_HANGAR || item.category_id != i64::from(CATEGORY_SHIP)
        || item.group_id == i64::from(GROUP_CAPSULE) || !item.singleton || item.quantity != 1 { return Err(unavailable()); }
    Ok(item)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation { Store, Board, Launch }

pub(super) async fn store(c: &mut Client, call: &Call<'_>) -> super::CallResult {
    let [marshal::Value::Int(host)] = call.args else { return Err(unavailable()) };
    run(c, call, *host, None, Operation::Store).await
}

pub(super) async fn board(c: &mut Client, call: &Call<'_>) -> super::CallResult {
    let [marshal::Value::Int(host), marshal::Value::Int(item)] = call.args else { return Err(unavailable()) };
    run(c, call, *host, Some(*item), Operation::Board).await
}

pub(super) async fn launch(c: &mut Client, call: &Call<'_>) -> super::CallResult {
    let [marshal::Value::Int(host), marshal::Value::Int(item)] = call.args else { return Err(unavailable()) };
    run(c, call, *host, Some(*item), Operation::Launch).await
}

async fn run(c: &mut Client, call: &Call<'_>, host_id: i64, target_id: Option<i64>, operation: Operation) -> super::CallResult {
    use infinity_core::consts::{IX_FLAG, IX_LOCATION_ID, IX_OWNER_ID, LOCATION_JUNKYARD};
    use crate::dao::items::{boarding, creation, inventory, space_ships::FreeShip};
    use crate::db::outbox::Outbox;
    use crate::services::{combat::reload_recovery::TakePreparation, effects::{fitting, online},
        fit::{Fit, Key}, inventory::{access::Container, destination::{Custody, Destination}, movement,
        notifications::{self, Audience}, reachability::{Endpoint, Reachability}}};
    use crate::space::system::boarding::transitions::{PreparedOld, PreparedTarget, PreparedTransition, settle_space};
    use marshal::Value;

    let market = crate::services::market::lock().await;
    let (station, bound_system) = super::configuration::current_binding(c, call).ok_or_else(unavailable)?;
    if station.is_some() { return Err(unavailable()); }
    let gate = if operation == Operation::Launch { None } else {
        Some(c.world.lock_condition_snapshot(bound_system).await.map_err(db_err)?)
    };
    let character = c.session.int("charid").ok_or_else(unavailable)?;
    let expected = c.world.online_session(character);
    if operation != Operation::Launch && expected.is_none() { return Err(unavailable()); }
    if expected.as_ref().is_some_and(|session| session.character_id != character
        || session.user_id != Some(c.login.user_id) || session.session_id != c.session.sid
        || session.ship_id != c.session.int("shipid") || session.system_id != Some(bound_system) || session.station_id.is_some()) { return Err(unavailable()); }
    let Current { identity, system, old, transition, host, corporation, alliance } =
        current(c, call, host_id, target_id, operation != Operation::Launch, expected.clone()).await?;
    let character = identity.char_id;
    let old_id = old.id;
    if operation == Operation::Store && old_id == host_id { return Err(unavailable()); }
    let old_item = load_items(&c.db, &c.sde, ItemQuery::Id(old_id)).await.map_err(db_err)?.pop().ok_or_else(unavailable)?;
    if old_item.owner_id != character || old_item.location_id != system || old_item.flag_id != 0 || old_item.type_id != old.type_id
        || old_item.category_id != i64::from(CATEGORY_SHIP) || !old_item.singleton || old_item.quantity != 1
        || (operation == Operation::Store && old_item.group_id == i64::from(GROUP_CAPSULE)) { return Err(unavailable()); }
    let item = if operation == Operation::Store { old_item.clone() }
        else { stored_item(c, host_id, target_id.ok_or_else(unavailable)?).await? };
    if operation != Operation::Store && item.item_id == old_id { return Err(unavailable()); }
    let endpoint = Endpoint { location: host_id, flag: FLAG_SHIP_HANGAR };
    let reach = Reachability::capture(&identity, &c.db, &c.world, Some(system), &[endpoint]).await?;
    let preparation = TakePreparation::capture(&c.db, &c.sde, item.item_id).await?;
    let mut endpoints = vec![endpoint];
    endpoints.extend_from_slice(preparation.endpoints());
    let audience = Audience::capture(&identity, &c.db, &c.world, Some(system), &endpoints).await?;
    let host_ancestry = crate::dao::items::deliveries::ancestry(&c.db, host_id).await.map_err(db_err)?;
    if !host_ancestry.iter().any(|row| row.item_id == host_id && row.location_id == system
        && row.flag_id == 0 && row.type_id == host.type_id && row.owner_id == host.owner_id
        && row.singleton && row.quantity == 1) { return Err(unavailable()); }
    let security = if operation == Operation::Launch { 0.0 } else { crate::services::crimewatch::security_status(&c.db, character).await? };
    let user = c.login.user_id;
    let client = c.login.client_id;
    let sde = c.sde.clone();
    let db = c.db.clone();
    let destination_pos = if operation == Operation::Store { old.pos } else {
        let radius = f64::from(super::balls::radius(&sde, item.type_id)?);
        if !radius.is_finite() || radius < 0.0 || !host.radius.is_finite() || host.radius < 0.0 { return Err(unavailable()); }
        crate::space::add(host.pos, crate::space::scale(crate::services::rand_dir(), f64::from(host.radius) + radius))
    };
    if !destination_pos.iter().all(|value| value.is_finite()) { return Err(unavailable()); }
    let prior_owner = item.owner_id;
    let item_id = item.item_id;
    let old_type = old.type_id;
    let target_type = item.type_id;
    let fences = online::capture_online_fences(&c.world, &[character]).await;
    macro_rules! execute {
        ($tx:ident, $outbox:ident, $states:ident, $facts:ident) => {{
            let tx = $tx;
            let outbox = $outbox;
            {
            let sde = sde.clone();
            let reach = reach.clone();
            let audience = audience.clone();
            let mut preparation = preparation.fresh_attempt();
            let host_ancestry = host_ancestry.clone();
            let old = old.clone();
            let item = item.clone();
            let fences = fences.clone();
            let states = $states.to_vec();
            let facts = $facts;
            Box::pin(async move {
                use crate::dao::items::parent_locks::{LockRequest, LockStrength};
                let mut ancestors = preparation.ancestors().to_vec();
                ancestors.extend(host_ancestry.iter().map(|row| row.item_id));
                let roots = [item.item_id, old_id];
                let closure = inventory::descendants(tx, &roots).await.map_err(db_err)?;
                let mut ids = closure.clone();
                ids.extend(ancestors);
                ids.sort_unstable();
                ids.dedup();
                let mut requests: Vec<_> = ids.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
                requests.extend_from_slice(preparation.refund().requests());
                let mandatory = preparation.refund().mandatory_ids();
                let optional: Vec<_> = preparation.refund().candidate_ids().into_iter()
                    .filter(|id| !ids.contains(id) && !mandatory.contains(id)).collect();
                let locks = crate::services::inventory::destination::lock_items(tx, &sde, &requests, &optional).await?;
                if roots.iter().any(|id| locks.row(*id).is_none())
                    || inventory::descendants(tx, &roots).await.map_err(db_err)? != closure
                    || crate::dao::items::deliveries::ancestry(tx, host_id).await.map_err(db_err)? != host_ancestry {
                    return Err(unavailable());
                }
                preparation.refund().revalidate(tx, &sde, &locks).await?;
                preparation.revalidate(tx).await?;
                if !boarding::guard_current(tx, character, user, old_id, system, old.type_id).await.map_err(db_err)? { return Err(unavailable()); }
                let current_item = load_items(tx, &sde, ItemQuery::Id(item.item_id)).await.map_err(db_err)?.pop().ok_or_else(unavailable)?;
                if current_item.owner_id != item.owner_id || current_item.location_id != item.location_id
                    || current_item.flag_id != item.flag_id || current_item.type_id != item.type_id
                    || current_item.singleton != item.singleton || current_item.quantity != item.quantity { return Err(unavailable()); }
                if operation != Operation::Store && !boarding::unselected(tx, item.item_id).await.map_err(db_err)? { return Err(unavailable()); }
                let written = if states.is_empty() { Vec::new() } else {
                    crate::space::system::boarding::transitions::checkpoint(tx, outbox, system, &states, facts).await?
                };
                let resolved = audience.resolve(tx, &sde).await?;
                let destination = if operation == Operation::Store {
                    movement::authorize_destination(tx, &sde, &identity, &reach, &Container::Item(host_id), Some(FLAG_SHIP_HANGAR)).await?.ok_or_else(unavailable)?
                } else {
                    reach.check(tx, &sde, endpoint).await?;
                    Destination::owned(character, system, 0)
                };
                if operation != Operation::Store {
                    let tree = load_items(tx, &sde, ItemQuery::Ids(preparation.closure())).await.map_err(db_err)?;
                    if tree.len() != preparation.closure().len() || tree.iter().any(|row| row.owner_id != character && !sde.is_tradable(row.type_id)) { return Err(unavailable()); }
                }
                let custody = if operation == Operation::Store { Custody::Preserve } else { Custody::TakeSubtree };
                movement::move_core(tx, &sde, outbox, &item, &destination, None, custody, &locks,
                    Some(&mut preparation), Some(&resolved), character).await?.ok_or_else(unavailable)?;
                let moved = load_items(tx, &sde, ItemQuery::Id(item.item_id)).await.map_err(db_err)?.pop().ok_or_else(unavailable)?;
                let mut changes = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(i64::from(item.flag_id)))];
                if item.owner_id != moved.owner_id { changes.push((IX_OWNER_ID, Value::Int(item.owner_id))); }
                resolved.item_change(outbox, &item, &moved, changes, character);
                outbox.event(crate::lua::events::GameEvent::ItemsMoved {
                    char_id: character, item_id: item.item_id, from_location_id: item.location_id,
                    to_location_id: destination.location, quantity: 1,
                });
                let target = if operation == Operation::Store {
                    let capsule = creation::positioned(tx, TYPE_CAPSULE, character, system, destination_pos).await.map_err(db_err)?;
                    notifications::notify_item_change(tx, &sde, outbox, capsule, vec![(IX_LOCATION_ID, Value::Int(LOCATION_JUNKYARD))]).await?;
                    capsule
                } else {
                    if !boarding::position_moved(tx, item.item_id, character, system, destination_pos).await.map_err(db_err)? { return Err(unavailable()); }
                    item.item_id
                };
                if operation != Operation::Launch {
                    if operation == Operation::Board && !boarding::position_moved(tx, old_id, character, system, old.pos).await.map_err(db_err)? { return Err(unavailable()); }
                    if !boarding::select_moved(tx, character, user, old_id, target, system).await.map_err(db_err)? { return Err(unavailable()); }
                    if operation == Operation::Board {
                        let fit = Fit::load(tx, sde.clone(), character, target).await?;
                        fitting::required_skills(&fit, &sde, Key::Ship, item.type_id, false)?;
                        online::reconcile_core(tx, &sde, outbox, &fences, character, target).await?;
                    }
                }
                let row = |id, kind, pos| FreeShip { item_id: id, type_id: kind, owner_id: character, corporation_id: corporation, alliance_id: alliance, pos };
                let old_transition = match operation {
                    Operation::Store => Some(PreparedOld::Stored { id: old_id }),
                    Operation::Board => Some(PreparedOld::Retained { snapshot_ball: old.clone(),
                        prepared: super::balls::unpiloted(tx, sde.clone(), &row(old_id, old.type_id, old.pos)).await? }),
                    Operation::Launch => None,
                };
                let target_type = if operation == Operation::Store { TYPE_CAPSULE } else { item.type_id };
                let target_row = row(target, target_type, destination_pos);
                let prepared = if operation == Operation::Launch {
                    super::balls::unpiloted(tx, sde, &target_row).await?
                } else {
                    super::balls::piloted(tx, sde, &target_row, character, security).await?
                };
                if prepared.ball.id != target || prepared.ball.char_id != (operation != Operation::Launch).then_some(character)
                    || prepared.ball.interactive != (operation != Operation::Launch) { return Err(unavailable()); }
                Ok((PreparedTransition { old: old_transition, target: PreparedTarget {
                    id: target, sampled_position: destination_pos, prepared, existing: false,
                    pilot: (operation != Operation::Launch).then_some((client, character)),
                }}, written))
            }) as crate::db::transaction::TxFuture<'_, (PreparedTransition, Vec<i64>), CallError>
            }
        }};
    }
    if operation == Operation::Launch {
        if c.world.online_session(character) != expected { return Err(unavailable()); }
        db.transaction(
            |tx, outbox: &mut Outbox<online::OnlineChanged>| {
                let states: &[crate::space::combat::ActorConditionSnapshot] = &[];
                let facts: Option<&crate::space::snapshot::FactCheckpoint> = None;
                execute!(tx, outbox, states, facts)
            },
            |(prepared, _), outbox| async move {
                super::publish_ship(&c.world, system, prepared.target.prepared, &market).await?;
                online::dispatch_changed((), outbox, c)
            },
        ).await?;
        Ok(Value::None)
    } else {
        let plan = if operation == Operation::Store {
            boarding::RecoveryPlan::Store { character, user, system, old: old_id, old_type, host: host_id }
        } else {
            boarding::RecoveryPlan::BoardStored { character, user, system, old: old_id, old_type,
                target: item_id, target_type, host: host_id, prior_owner }
        };
        let expected = expected.ok_or_else(unavailable)?;
        let selected = settle_space(c.world.clone(), db, c.sde.clone(), system, character, client, expected.clone(),
            transition.ok_or_else(unavailable)?, market, gate.ok_or_else(unavailable)?, plan,
            move |tx, outbox, states, facts| execute!(tx, outbox, states, facts)).await?;
        super::boarding::finish(c, selected, Some(system), Some(&expected)).await
    }
}

#[cfg(test)] mod tests;
