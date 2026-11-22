//! Atomic `invbroker.TrashItems` deletion of authorized item trees.
//! The same rule is used by Lua item deletion; all ancestry, custody and reservations
//! are checked inside the transaction before guarded sorted deletes.
//! Corporation deliveries are never trashed, even with the deliveries Take role:
//! the client treats them as a separate container whose items are taken out first.
//! Inventory writers that only create space objects (exploration/combat loot and
//! positioned containers), character-body implants, drone-bay/space state, trade
//! escrow, or newly-created ships/fittings cannot create a trashable station or
//! owned-container destination. The reachable cargo/hangar exceptions are the
//! slash `/create`, probe recovery, and pending-reload recovery writers; their
//! outer operations take the market lock. `return_pending` intentionally remains
//! lock-free because trashing and repackaging call it inside their existing lock.
//! Closure validation, locking and transactional destruction live in the shared
//! inventory lifecycle; online fitted modules are taken offline through its
//! destruction entry before their item rows are deleted. Surviving parent ships
//! receive a single final fitting update after their children are deleted.
use infinity_core::consts::CATEGORY_SHIP;
use crate::{client::Client, dao::items::{inventory as inventory_dao, loading::ItemQuery}, db::outbox::Outbox, services::{db_err, items::load_items, market, notify_error, user_error, CallError}};
use infinity_core::messages;
use marshal::Value;
use super::{movement::{dispatch_move, MoveEffect}};


/// Convert the trash business refusal into the catalogue key expected by
/// `invItemFunctions.TrashInvItems`; unrelated errors remain errors at the RPC boundary.
pub(super) fn broker_refusal_key(error: &CallError) -> Option<&'static str> {
    match error {
        CallError::User { msg, .. } if *msg == messages::MSG_CANNOT_TRASH_ITEM => Some(messages::MSG_CANNOT_TRASH_ITEM),
        _ => None,
    }
}

pub(crate) async fn resolve_location(c: &Client, item: i64) -> Result<i64, CallError> {
    // A transient child's true outer is its actor's system, not a database
    // ancestry that does not exist; everything else keeps the old walk.
    let actor_identity = super::npc_loot::client_identity(c);
    if let Some(system) = c.session.int("solarsystemid") {
        match super::npc_loot::source(&c.world, &actor_identity, system, item, None).await {
            crate::space::system::npc_wrecks::NpcWreckLookup::Ready(source) =>
                return Ok(source.wreck.root.location_id),
            crate::space::system::npc_wrecks::NpcWreckLookup::Busy(claim) => {
                super::npc_loot::reconcile_busy(&c.world, &c.sde, claim).await;
                return Err(super::npc_loot::busy());
            }
            crate::space::system::npc_wrecks::NpcWreckLookup::Absent => match super::npc_loot::view(&c.world, &actor_identity, system, item).await {
                crate::space::system::npc_wrecks::NpcWreckLookup::Ready(view) => return Ok(view.root.location_id),
                crate::space::system::npc_wrecks::NpcWreckLookup::Busy(claim) => {
                    super::npc_loot::reconcile_busy(&c.world, &c.sde, claim).await;
                    return Err(super::npc_loot::busy());
                }
                _ => {}
            },
            crate::space::system::npc_wrecks::NpcWreckLookup::Denied => {}
        }
    }
    let chain = crate::dao::items::deliveries::ancestry(&c.db, item).await.map_err(db_err)?;
    let ids: std::collections::HashSet<_> = chain.iter().map(|row| row.item_id).collect();
    chain.iter().find(|row| !ids.contains(&row.location_id)).map(|row| row.location_id)
        .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))
}

/// Capture everything ordinary trash needs outside its transaction, in the same
/// order and shape the shared lifecycle verifies after locking. Each selected
/// root's true ancestry terminal is classified here and must equal the supplied
/// outer location; mismatched or mixed terminals refuse the whole batch.
/// A root in the caller's own NPC station hangar exempts its captured subtree,
/// including the contents of an inactive hull. Other roots prove their
/// non-station terminal or a reachable ship ancestor, so docking never grants
/// reach to space-rooted items.
async fn capture(c: &Client, roots: &[i64], location: i64) -> Result<(super::lifecycle::Prepared, super::notifications::Audience, super::super::effects::online::OnlineFences), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let station = c.session.int("stationid");
    let snapshots_ids = inventory_dao::descendants(&c.db, roots).await.map_err(db_err)?;
    let mut snapshots = load_items(&c.db, &c.sde, ItemQuery::Ids(&snapshots_ids)).await.map_err(db_err)?;
    snapshots.sort_by_key(|item| item.item_id);
    let notice_endpoints: Vec<_> = snapshots.iter().map(|item| super::reachability::Endpoint { location: item.location_id, flag: item.flag_id }).collect();
    let hangar_roots: std::collections::BTreeSet<i64> = snapshots.iter()
        .filter(|item| roots.contains(&item.item_id) && item.owner_id == me
            && item.flag_id == infinity_core::consts::FLAG_HANGAR
            && c.sde.map.station(item.location_id).is_some())
        .map(|item| item.item_id).collect();
    let mut checked = std::collections::BTreeSet::new();
    let mut lineages = std::collections::HashMap::new();
    let mut shared_ids = std::collections::HashSet::new();
    for item in &snapshots {
        if !lineages.contains_key(&item.location_id) {
            lineages.insert(item.location_id, crate::dao::items::deliveries::ancestry(&c.db, item.location_id).await.map_err(db_err)?);
        }
        let rows = &lineages[&item.location_id];
        let endpoint = super::reachability::Endpoint { location: item.location_id, flag: item.flag_id };
        if super::movement::shared_lineage(&c.sde, endpoint, rows.len(), |id| rows.iter().find(|row| row.item_id == id)) {
            shared_ids.insert(item.item_id);
        }
        let exempt = hangar_roots.contains(&item.item_id)
            || rows.iter().any(|row| hangar_roots.contains(&row.item_id));
        let mut parent = item.location_id;
        let mut flag = item.flag_id;
        for _ in 0..rows.len() {
            let Some(row) = rows.iter().find(|row| row.item_id == parent) else { break };
            if !exempt && c.sde.types.get(row.type_id).is_some_and(|ty| ty.category_id == Some(CATEGORY_SHIP)) { checked.insert((parent, flag)); }
            parent = row.location_id;
            flag = row.flag_id;
        }
        if roots.contains(&item.item_id) {
            if c.sde.map.station(parent).is_none() { checked.insert((item.location_id, item.flag_id)); }
            if parent != location { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
        }
    }
    let endpoints: Vec<_> = checked.into_iter().map(|(location, flag)| super::reachability::Endpoint { location, flag }).collect();
    let identity = crate::services::identity::Identity::of_client(c);
    let system = c.session.int("solarsystemid");
    let reach = super::reachability::Reachability::capture(&identity, &c.db, &c.world, system, &endpoints).await?;
    let audience = super::notifications::Audience::capture(&identity, &c.db, &c.world, system, &notice_endpoints).await?;
    let mut owners: Vec<_> = snapshots.iter().map(|item| item.owner_id).collect();
    owners.extend(lineages.values().flat_map(|rows| rows.iter().map(|row| row.owner_id)));
    owners.push(me);
    owners.sort_unstable();
    owners.dedup();
    let fences = super::super::effects::online::capture_online_fences(&c.world, &owners).await;
    let mut unique_roots = roots.to_vec();
    unique_roots.sort_unstable();
    unique_roots.dedup();
    Ok((super::lifecycle::Prepared {
        roots: unique_roots,
        snapshots,
        lineages,
        outer: location,
        boundary: None,
        endpoints,
        reach: Some(reach),
        shared_ids,
        active_ship: c.session.int("shipid"),
        station,
        notify: Some(c.notify_tx.clone()),
    }, audience, fences))
}

/// How one requested root resolved inside the actor turn: a transient child
/// with its true outer, a transient root, or an ordinary persistent row.
enum TrashClass {
    Child { root: i64, outer: i64, item: crate::dao::items::loading::Item },
    Root { outer: i64, owner: i64, type_id: i32, group_id: i64, category_id: i64 },
    Persistent,
    Denied,
    Busy(Option<std::sync::Arc<crate::space::system::npc_wrecks::NpcWreckClaim>>),
}

/// Destroy the permitted roots through the shared lifecycle after the client
/// inventory's own session capture; post-commit routing is the same as before.
pub(crate) async fn trash_items(c: &mut Client, roots: &[i64], location: i64) -> Result<Value, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    if roots.is_empty() { return Ok(Value::list([])); }
    // Every requested root is classified in one actor turn: transient members
    // carry their real outer and owner, everything else stays persistent.
    let actor_identity = super::npc_loot::client_identity(c);
    let system = c.session.int("solarsystemid").unwrap_or_default();
    let classified = match c.session.int("solarsystemid") {
        Some(system) => {
            let roots = roots.to_vec();
            c.world.with(system, move |s| roots.iter().map(|&id| {
                match s.npc_loot_source(&actor_identity, id, None) {
                    crate::space::system::npc_wrecks::NpcWreckLookup::Ready(source) =>
                        TrashClass::Child { root: source.wreck.root.item_id, outer: source.wreck.root.location_id, item: source.item },
                    crate::space::system::npc_wrecks::NpcWreckLookup::Busy(claim) => TrashClass::Busy(claim),
                    crate::space::system::npc_wrecks::NpcWreckLookup::Denied => TrashClass::Denied,
                    crate::space::system::npc_wrecks::NpcWreckLookup::Absent => match s.npc_wreck_view(&actor_identity, id) {
                        crate::space::system::npc_wrecks::NpcWreckLookup::Ready(view) => TrashClass::Root {
                            outer: view.root.location_id, owner: view.root.owner_id,
                            type_id: view.root.type_id, group_id: view.root.group_id, category_id: view.root.category_id,
                        },
                        crate::space::system::npc_wrecks::NpcWreckLookup::Busy(claim) => TrashClass::Busy(claim),
                        crate::space::system::npc_wrecks::NpcWreckLookup::Denied => TrashClass::Denied,
                        crate::space::system::npc_wrecks::NpcWreckLookup::Absent => TrashClass::Persistent,
                    },
                }
            }).collect::<Vec<_>>()).await
        }
        None => None,
    };
    let classified = classified.unwrap_or_else(|| roots.iter().map(|_| TrashClass::Persistent).collect());
    let cannot_trash = || user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]);
    let mut root_deletes = std::collections::BTreeSet::new();
    let mut child_deletes: std::collections::BTreeMap<i64, Vec<i64>> = std::collections::BTreeMap::new();
    let mut persistent_ids = Vec::new();
    for (index, entry) in classified.iter().enumerate() {
        match entry {
            TrashClass::Child { root, outer, item } => {
                if *outer != location || item.owner_id != me
                    || c.sde.typelists.is_untrashable(item.type_id, item.group_id as i32, item.category_id as i32) {
                    return Err(cannot_trash());
                }
                child_deletes.entry(*root).or_default().push(item.item_id);
            }
            TrashClass::Root { outer, owner, type_id, group_id, category_id } => {
                if *outer != location || *owner != me
                    || c.sde.typelists.is_untrashable(*type_id, *group_id as i32, *category_id as i32) {
                    return Err(cannot_trash());
                }
                root_deletes.insert(roots[index]);
            }
            TrashClass::Busy(claim) => {
                super::npc_loot::reconcile_busy(&c.world, &c.sde, claim.clone()).await;
                return Err(super::npc_loot::busy());
            }
            TrashClass::Denied => return Err(cannot_trash()),
            TrashClass::Persistent => persistent_ids.push(roots[index]),
        }
    }
    let access = crate::space::system::npc_wrecks::NpcWreckAccess::Inventory(actor_identity);
    let specs: Vec<(i64, Vec<i64>)> = root_deletes.iter().map(|&root| (root, vec![root]))
        .chain(child_deletes.iter().filter(|(root, _)| !root_deletes.contains(*root))
            .map(|(root, items)| (*root, items.clone())))
        .collect();
    if persistent_ids.is_empty() {
        // Pure transient batch: the claims validate every root together, then
        // each apply removes its own rows. No database, no economy lock.
        let guards = super::npc_loot::claim_deletes(&c.world, &c.sde, system, access, specs,
            Vec::new(), Vec::new()).await.map_err(|_| cannot_trash())?;
        super::npc_loot::commit_deletes(guards).await;
        return Ok(Value::list([]));
    }
    let (prepared, audience, fences) = capture(c, &persistent_ids, location).await?;
    let mut hulls = std::collections::BTreeSet::new();
    for item in &prepared.snapshots {
        if item.category_id == i64::from(CATEGORY_SHIP) { hulls.insert(item.item_id); }
        else if item.category_id == i64::from(infinity_core::consts::CATEGORY_MODULE) && super::is_fitting_flag(item.flag_id) { hulls.insert(item.location_id); }
    }
    let ships: Vec<_> = hulls.iter().filter_map(|&ship| fences.membership(ship).map(|(system, _)| (system, ship))).collect();
    let armed = super::super::combat::loaded::LoadedHolds::arm(&c.world, &ships, notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let _lock = market::lock().await;
    let mut holds = super::super::combat::loaded::LoadedHolds::lock(&c.world, armed, notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let settle = holds.settle_rows();
    let held_ships = holds.ships();
    // Mixed batch: claims freeze the complete persistent preimage, then one
    // transaction does the shared lifecycle; the memory apply only runs on a
    // proven commit.
    let mut deletes = if specs.is_empty() { Vec::new() } else {
        super::npc_loot::claim_deletes(&c.world, &c.sde, system, access, specs,
            prepared.roots.clone(), prepared.snapshots.clone()).await?
    };
    let claims: Vec<std::sync::Arc<crate::space::system::npc_wrecks::NpcWreckClaim>> =
        deletes.iter().map(|claimed| claimed.guard.claim().clone()).collect();
    let fence_keys: Vec<String> = claims.iter()
        .map(|claim| super::npc_loot::delete_fence_key(claim.root(), claim.serial())).collect();
    super::npc_loot::may_execute_deletes(&mut deletes);
    let db = c.db.clone(); let sde = c.sde.clone();
    let world = c.world.clone(); let notify_tx = c.notify_tx.clone();
    let char_id = c.session.int("charid");
    let custody = super::lifecycle::CustodyRule::Ordinary { me };
    holds.may_execute();
    let result = db.transaction(move |tx, outbox: &mut Outbox<MoveEffect>| {
        let sde = sde.clone(); let prepared = prepared.clone(); let custody = custody;
        let audience = audience.clone(); let fences = fences.clone();
        let settle = settle.clone();
        let held_ships = held_ships.clone();
        let fence_keys = fence_keys.clone();
        Box::pin(async move {
            for key in &fence_keys {
                crate::dao::items::npc_loot::fence(tx, key).await.map_err(db_err)?;
            }
            let mut verified = super::lifecycle::verify_closure(tx, &sde, &prepared, &custody, super::lifecycle::Operation::Destroy).await?;
            verified.loaded_writes = settle;
            let audience = Some(audience.resolve(tx, &sde).await?);
            super::lifecycle::destroy(tx, &sde, outbox, &prepared, &custody, audience.as_ref(), &fences, me, verified).await?;
            let mut seed_sets = Vec::with_capacity(held_ships.len());
            for ship in held_ships {
                let touched: Vec<_> = prepared.snapshots.iter()
                    .filter(|item| item.category_id == i64::from(infinity_core::consts::CATEGORY_MODULE)
                        && super::is_fitting_flag(item.flag_id) && item.location_id == ship)
                    .map(|item| (item.item_id, item.flag_id)).collect();
                seed_sets.push((ship, super::super::combat::loaded::seed_entries(tx, ship, &touched).await?));
            }
            Ok((Value::list([]), seed_sets))
        })
    }, move |value, outbox| async move {
        super::npc_loot::commit_deletes(deletes).await;
        dispatch_move(value, outbox, &world, |notification| super::super::effects::online::dispatch_notification(notification, &world, &notify_tx, char_id))
    }).await;
    if result.is_err() {
        super::npc_loot::reconcile_deletes(&c.world, &c.sde, &claims).await;
    }
    let seeds = match &result { Ok((_, seed_sets)) => seed_sets.clone(), _ => Vec::new() };
    holds.finish(&db, &c.world, result, &seeds).await.map(|(value, _)| value)
}

#[cfg(all(test, feature = "tests-inventory"))]
mod tests;
