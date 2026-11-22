//! Transient NPC loot consumers for `eve/client/script/environment/invCache.py`.
//! Ordinary NPC wreck roots and their untaken loot live only in the owning
//! system actor: reads and in-place mutations never reach `game.items`
//! or the economy lock, a persistent deposit into a transient root is refused
//! for every `move_item` caller, and only the taken slice of a successful
//! pickup is staged inside the existing movement transaction and its
//! `ItemsMoved` journal. A sealed Unknown delete is proved once under its
//! original fence and never written again: all frozen rows gone is Committed,
//! the exact original closure is RolledBack, and any drift stays sealed.
//! A live or parked claim is only busy. Claim lifecycle, source notices,
//! reserved IDs and the receipt fence live in `crate::space::system::npc_wrecks`
//! and `crate::dao::items::npc_loot`; this module only adapts that narrow actor
//! API to the inventory services. Nothing here restores loot after actor exit,
//! and a cancelled notification is never replayed.

use std::sync::Arc;

use marshal::Value;
use infinity_core::consts::{
    CATEGORY_SHIP, FLAG_HANGAR,
    IX_FLAG, IX_LOCATION_ID, IX_OWNER_ID,
};
use infinity_core::messages;
use crate::client::Client;
use crate::dao::items::{inventory as inventory_dao, loading::{GameItem, Item, ItemQuery}, npc_loot as npc_dao};
use crate::db::{Db, outbox::{Notification, Outbox}};
use crate::sde::Sde;
use crate::services::{db_err, identity::Identity, items::{enrich_item, load_items}, keyval, notify_error, CallError};
use crate::space::{System, World};
use crate::space::npc_wrecks::NpcWreckGuard;
use crate::space::system::npc_wrecks::{
    NpcLootMutation, NpcLootSource, NpcWreckAccess, NpcWreckClaim, NpcWreckClaimState,
    NpcWreckDisposition, NpcWreckKind, NpcWreckLookup, NpcWreckOperation, NpcWreckView,
};
use super::access::Container;
use super::destination::{Custody, ParentSnapshot};
use super::movement::{authorized_destination, dispatch_move, move_core, shared_lineage, MoveEffect, FLAG_AUTOFIT};
use super::notifications::Audience;
use super::reachability::{Endpoint, Reachability};

/// The shared "try again shortly" refusal used while a root claim is in flight.
pub(super) fn busy() -> CallError { notify_error(messages::fitting::CHARGES_SETTLING) }

/// The shared "not here any more" refusal for denied or vanished transient ids.
pub(super) fn denied() -> CallError { notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION) }

/// The identity an actor admission must see: `system_id` is the live actor id,
/// not the session's other system field.
pub(super) fn actor_identity(identity: &Identity, space_system: Option<i64>) -> Identity {
    Identity { system_id: space_system, ..*identity }
}

/// Actor-bound identity for a client call in the caller's current system.
pub(in crate::services) fn client_identity(c: &Client) -> Identity {
    actor_identity(&Identity::of_client(c), c.session.int("solarsystemid"))
}

/// One actor turn for a transient-root read; a stopped actor reads as `Absent`
/// so the ordinary database path keeps the final say.
pub(super) async fn view(world: &World, identity: &Identity, system: i64, root: i64) -> NpcWreckLookup<NpcWreckView> {
    let identity = *identity;
    world.with(system, move |s| s.npc_wreck_view(&identity, root)).await
        .unwrap_or(NpcWreckLookup::Absent)
}

/// One actor turn classifying every candidate id; results keep input order.
pub(in crate::services) async fn views(world: &World, identity: &Identity, system: i64, ids: &[i64]) -> Vec<NpcWreckLookup<NpcWreckView>> {
    let (identity, ids) = (*identity, ids.to_vec());
    let count = ids.len();
    world.with(system, move |s| ids.iter().map(|&id| s.npc_wreck_view(&identity, id)).collect())
        .await.unwrap_or_else(|| (0..count).map(|_| NpcWreckLookup::Absent).collect())
}

/// Locate one plain child through the actor reverse index in a single turn.
pub(super) async fn source(world: &World, identity: &Identity, system: i64, item: i64, source: Option<i64>) -> NpcWreckLookup<NpcLootSource> {
    let identity = *identity;
    world.with(system, move |s| s.npc_loot_source(&identity, item, source)).await
        .unwrap_or(NpcWreckLookup::Absent)
}

/// `List`/`ListByFlags` rows from one admitted view: the original flag filter
/// and item-id order, with flags kept at their true zero value.
pub(super) fn list_rows(view: &NpcWreckView, flags: Option<Vec<i64>>) -> Vec<Value> {
    let flags: Option<Vec<i32>> = flags.map(|f| f.into_iter().filter_map(|x| i32::try_from(x).ok()).collect());
    view.items.iter()
        .filter(|item| flags.as_ref().is_none_or(|flags| flags.contains(&item.flag_id)))
        .map(Item::row).collect()
}

fn game_item(item: &Item) -> GameItem {
    GameItem {
        item_id: item.item_id, type_id: item.type_id, owner_id: item.owner_id,
        location_id: item.location_id, flag_id: item.flag_id, quantity: item.quantity,
        singleton: item.singleton, custom_info: item.custom_info.clone(),
    }
}

/// `GetCapacity` for a transient root: an ordinary replacement container uses
/// the shared room rule with an empty pending-reload set, while a wreck keeps
/// the original static-capacity plus item-volume semantics.
pub(super) fn capacity_reply(sde: &Sde, view: &NpcWreckView) -> Result<Value, CallError> {
    let (capacity, used) = if super::hold_storage::is_ordinary_container(sde, view.root.type_id) {
        let parent = game_item(&view.root);
        let items: Vec<GameItem> = view.items.iter().map(game_item).collect();
        let room = super::hold_storage::container_room(sde, &parent, &items, &[])?
            .ok_or_else(denied)?;
        let capacity = match room.capacity {
            super::holds::CapacityLimit::Finite(value) => value,
            super::holds::CapacityLimit::Unlimited => return Err(denied()),
        };
        (capacity, room.used)
    } else {
        let mut items: Vec<GameItem> = Vec::with_capacity(view.items.len() + 1);
        items.push(game_item(&view.root));
        items.extend(view.items.iter().map(game_item));
        super::inv_inventory::item_capacity(&items, view.root.item_id, sde)?
    };
    Ok(keyval([("capacity", Value::Float(capacity)), ("used", Value::Float(used))]))
}

/// The display name a transient root contributes to `evelocations`: the
/// renamed slim value first, then the SDE type name.
pub(in crate::services) fn location_name(sde: &Sde, view: &NpcWreckView) -> String {
    view.name.clone()
        .or_else(|| sde.types.get(view.root.type_id).and_then(|ty| ty.name.clone()))
        .unwrap_or_default()
}

/// Same-actor transient→transient move: same-root in-place changes and
/// cross-root moves both go through the actor's own move mutation with the
/// flag already normalized by the shared movement contract - transient roots
/// are never ships, so anything requiring a real fitting slot is refused.
/// Cross-root ownership follows the original ForeignLoot rule - the caller's
/// own root is the only destination authority.
pub(super) async fn transient_move(
    world: &World, identity: &Identity, system: i64,
    source: NpcLootSource, destination: NpcWreckView, flag: Option<i32>, qty: Option<i64>,
) -> Result<Option<i64>, CallError> {
    let item = source.item.clone();
    let quantity = qty.unwrap_or(item.quantity);
    if quantity <= 0 || quantity > item.quantity { return Err(denied()); }
    let identity = *identity;
    if super::is_internal_modifier_flag(item.flag_id) { return Err(denied()); }
    let Some(defaulted) = super::movement::item_destination_flag(destination.root.category_id, flag)
        else { return Err(denied()) };
    if super::is_internal_modifier_flag(defaulted) { return Err(denied()); }
    let (flag, needs_fitting) = super::movement::classify_move_flag(item.category_id, defaulted);
    if needs_fitting { return Err(denied()); }
    let expected = source.wreck.identity.clone();
    if destination.root.item_id == source.wreck.root.item_id {
        // The root must be the caller's own container before any in-place
        // operation, exactly like the persistent destination rule. Whole
        // moves and splits share one mutation so the final flag is carried
        // for both the flag update and the new slice.
        if destination.root.owner_id != identity.char_id { return Err(denied()); }
        return world.with(system, move |s| s.mutate_npc_loot(&identity, &expected,
            NpcLootMutation::Move { item: item.item_id, quantity, destination: destination.identity.clone(), flag }))
            .await.unwrap_or_else(|| Err(busy()))
            .map(|id| Some(id.unwrap_or(item.item_id)));
    }
    // Cross-root: the merge survivor is the caller-owned stack the mutation
    // picks first - the destination authority is the caller's own root.
    let merge_id = destination.items.iter()
        .find(|stack| stack.type_id == item.type_id && stack.owner_id == identity.char_id
            && stack.flag_id == flag && !stack.singleton)
        .map(|stack| stack.item_id);
    let created = world.with(system, move |s| s.mutate_npc_loot(&identity, &expected,
        NpcLootMutation::Move { item: item.item_id, quantity, destination: destination.identity.clone(), flag }))
        .await.unwrap_or_else(|| Err(busy()))?;
    // The verified plan maps the result exactly once: a new slice id, the
    // captured merge survivor, or the moved original id.
    Ok(Some(created.or(merge_id).unwrap_or(item.item_id)))
}

fn take_claim_turn(s: &mut System, identity: &Identity, item: i64, source: Option<i64>, operation: &NpcWreckOperation) -> NpcWreckLookup<NpcWreckGuard> {
    match s.npc_loot_source(identity, item, source) {
        NpcWreckLookup::Ready(fresh) => s.claim_npc_wreck(&fresh.wreck.identity, NpcWreckAccess::Inventory(*identity), operation.clone()),
        NpcWreckLookup::Absent => NpcWreckLookup::Absent,
        NpcWreckLookup::Denied => NpcWreckLookup::Denied,
        NpcWreckLookup::Busy(claim) => NpcWreckLookup::Busy(claim),
    }
}

/// Whether `claim` is the root's sealed commit-unknown attempt. Only a sealed
/// claim may enter a fenced proof; a live claim is just busy to its callers.
async fn claim_is_unknown(world: &World, claim: &Arc<NpcWreckClaim>) -> bool {
    let claim = claim.clone();
    world.with(claim.system(), move |s| s.npc_wreck_claim_status(&claim) == Some(NpcWreckClaimState::Unknown)).await.unwrap_or(false)
}

/// The operation fence a delete attempt and its commit proof share: a fixed
/// business namespace bound to the frozen root/serial pair.
pub(super) fn delete_fence_key(root: i64, serial: i64) -> String {
    format!("NpcWreckDelete:{root}:{serial}")
}

/// Prove one outstanding claim the caller collided with: receipt claims take
/// the shared canonical source proof, a mixed delete re-verifies its frozen
/// persistent preimage under the same fence, and a pure-memory delete has no
/// database question to answer at all. A claim that is not sealed stays busy;
/// an active claim is never judged from absent rows.
pub(super) async fn reconcile_claim(world: &World, sde: &Arc<Sde>, claim: Arc<NpcWreckClaim>) -> Result<NpcWreckDisposition, CallError> {
    if claim.receipt().is_some() {
        return world.reconcile_npc_wreck(claim).await;
    }
    let NpcWreckOperation::Delete { persistent_roots, .. } = claim.operation() else {
        return Err(CallError::Internal(anyhow::anyhow!("npc wreck claim without receipt or delete plan")));
    };
    if !claim_is_unknown(world, &claim).await {
        return Err(busy());
    }
    if persistent_roots.is_empty() {
        // A pure-memory delete has no database side; while the claim is still
        // registered its apply provably never ran, so release is truthful.
        let system = claim.system();
        let settled = world.with(system, move |s| s.finish_npc_wreck(&claim, NpcWreckDisposition::RolledBack)).await.unwrap_or(false);
        return if settled { Ok(NpcWreckDisposition::RolledBack) } else { Err(busy()) };
    }
    let system = claim.system();
    let key = delete_fence_key(claim.root(), claim.serial());
    let sde_tx = sde.clone();
    let frozen = claim.clone();
    let disposition = world.db().transaction(
        move |tx, _: &mut Outbox<()>| {
            let key = key.clone();
            let sde = sde_tx.clone();
            let attempt = frozen.clone();
            Box::pin(async move {
                let NpcWreckOperation::Delete { persistent_roots, persistent, .. } = attempt.operation() else {
                    return Err(CallError::Internal(anyhow::anyhow!("npc wreck claim without receipt or delete plan")));
                };
                npc_dao::fence(tx, &key).await.map_err(CallError::from)?;
                // Every frozen ID is an optional candidate: a missing row is
                // the committed fact, never a filtered-load absence.
                use crate::dao::items::parent_locks::{LockRequest, LockStrength};
                let requests: Vec<LockRequest> = persistent.iter()
                    .map(|item| LockRequest { item_id: item.item_id, strength: LockStrength::Update }).collect();
                let mut frozen_ids: Vec<i64> = persistent.iter().map(|item| item.item_id).collect();
                let locks = super::destination::lock_items(tx, &sde, &requests, &frozen_ids).await?;
                if persistent.iter().all(|item| locks.row(item.item_id).is_none()) {
                    return Ok(NpcWreckDisposition::Committed);
                }
                let unchanged = persistent.iter().all(|item| locks.row(item.item_id).is_some_and(|row|
                    row.jump_clone_id.is_none() && row.matches_item_metadata(item)));
                if unchanged {
                    frozen_ids.sort_unstable();
                    let closure = inventory_dao::descendants(tx, persistent_roots).await.map_err(db_err)?;
                    if closure == frozen_ids {
                        return Ok(NpcWreckDisposition::RolledBack);
                    }
                }
                Err(busy())
            })
        },
        |disposition, _| async move { Ok::<_, CallError>(disposition) },
    ).await;
    let outcome = match &disposition {
        Ok(disposition) => *disposition,
        Err(_) => return Err(busy()),
    };
    let settled = claim.clone();
    let sde_scene = sde.clone();
    // Same actor turn: recheck Unknown, confirm the persistent scene, and
    // only then release the last claim authority. No await sits between them.
    let applied = world.with(system, move |s| {
        if s.npc_wreck_claim_status(&settled) != Some(NpcWreckClaimState::Unknown) { return false; }
        if matches!(outcome, NpcWreckDisposition::Committed) {
            if let (true, NpcWreckOperation::Delete { persistent_roots, persistent, .. }) =
                (matches!(settled.access(), NpcWreckAccess::GmCleanup { .. }), settled.operation())
            {
                if !super::debris::remove_committed_roots(s, &sde_scene, system, persistent_roots, persistent) {
                    return false;
                }
            }
        }
        s.finish_npc_wreck(&settled, outcome)
    }).await.unwrap_or(false);
    if !applied { return Err(busy()); }
    disposition
}

/// One collided claim gets its sealed-Unknown proof once, then the caller
/// reports busy: only the old actor delta is completed, never the old
/// business write, and the next independent request reads the new preimage.
pub(in crate::services) async fn reconcile_busy(world: &World, sde: &Arc<Sde>, claim: Option<Arc<NpcWreckClaim>>) {
    if let Some(claim) = claim {
        let _ = reconcile_claim(world, sde, claim).await;
    }
}

/// One taken-slice write inside the pickup transaction.
enum TakeWrite {
    /// This attempt staged and moved the slice itself.
    Applied,
    /// The frozen receipt already existed; nothing was written again.
    Preexisting,
}


/// Take a slice of transient NPC loot into a real persistent destination
/// through the existing movement transaction: destination authorization,
/// resident-hull settlement, parent locks, fit validation and `move_core` are
/// the original post-half; the claim under the economy lock freezes the whole
/// attempt and the receipt fence proves it exactly once.
#[allow(clippy::too_many_arguments)]
pub(super) async fn take(
    identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, space_system: Option<i64>,
    container: &Container, npc: NpcLootSource, rpc_source: Option<i64>,
    flag: Option<i32>, qty: Option<i64>,
    notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> Result<Option<i64>, CallError> {
    let char_id = identity.char_id;
    let station = identity.station_id;
    let corporation = (identity.corp_id != 0).then_some(identity.corp_id);
    let Some(system) = space_system else { return Err(denied()) };
    let item_id = npc.item.item_id;
    let Some((location, owner, destination_flag, corporate_destination)) =
        authorized_destination(db, sde, container, flag, char_id, station, corporation).await?
    else {
        warn_take(char_id, item_id, container);
        return Err(denied());
    };
    let actor_identity = actor_identity(identity, Some(system));
    let destination_fitting = matches!(container, Container::Item(_))
        && super::movement::classify_move_flag(npc.item.category_id, destination_flag).1;
    // A fitting take is always a single unit; everything else is the
    // original whole-or-requested quantity rule.
    let quantity = if destination_fitting { 1 }
        else { qty.filter(|&q| q > 0 && q < npc.item.quantity).unwrap_or(npc.item.quantity) };
    let destination_ancestry = crate::dao::items::deliveries::ancestry(db, location).await.map_err(db_err)?;
    let destination_endpoint = Endpoint { location, flag: destination_flag };
    let reachability = Reachability::capture(&actor_identity, db, world, space_system, &[destination_endpoint]).await?;
    let destination_parent = if matches!(container, Container::Item(_)) && !corporate_destination {
        load_items(db, sde, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next()
    } else { None };
    let destination_shared = shared_lineage(sde, destination_endpoint, destination_ancestry.len(), |id| destination_ancestry.iter().find(|row| row.item_id == id));
    let audience = if destination_shared {
        Some(Audience::capture(identity, db, world, space_system, std::slice::from_ref(&destination_endpoint)).await?)
    } else { None };
    let mut fence_owners = vec![npc.item.owner_id, owner];
    if super::is_fitting_flag(destination_flag) || destination_flag == FLAG_AUTOFIT {
        if let Some(parent) = destination_ancestry.iter().find(|row| row.item_id == location) {
            fence_owners.push(parent.owner_id);
        }
    }
    fence_owners.sort_unstable();
    fence_owners.dedup();
    let fences = super::super::effects::online::capture_online_fences(world, &fence_owners).await;
    // Every resident hull actually touched: cargo destination ancestors count,
    // not only a fitting destination's own hull.
    let mut engage_ships = Vec::new();
    for row in &destination_ancestry {
        if !sde.types.get(row.type_id).is_some_and(|ty| ty.category_id == Some(CATEGORY_SHIP)) { continue; }
        if let Some((member_system, _)) = fences.membership(row.item_id) {
            engage_ships.push((member_system, row.item_id));
        }
    }
    let armed = super::super::combat::loaded::LoadedHolds::arm(world, &engage_ships, busy()).await?;
    let _lock = super::super::market::lock().await;
    let mut holds = super::super::combat::loaded::LoadedHolds::lock(world, armed, busy()).await?;
    let settle = holds.settle_rows();
    let held = holds.held(location);
    // The final recheck and the claim happen in one actor turn under the
    // economy lock, so the frozen plan always binds the current revision.
    let operation = NpcWreckOperation::Take {
        item: item_id, quantity, character: char_id, destination: location,
    };
    let mut reconciled = false;
    let mut guard = loop {
        let lookup = {
            let operation = operation.clone();
            world.with(system, move |s| take_claim_turn(s, &actor_identity, item_id, rpc_source, &operation)).await
        };
        match lookup {
            Some(NpcWreckLookup::Ready(guard)) => break guard,
            Some(NpcWreckLookup::Busy(Some(claim))) if !reconciled && claim.prepared() => {
                reconciled = true;
                let _ = reconcile_claim(world, sde, claim).await;
            }
            Some(NpcWreckLookup::Busy(_)) | None => return Err(busy()),
            Some(NpcWreckLookup::Absent | NpcWreckLookup::Denied) => return Err(denied()),
        }
    };

    let claim_done = guard.claim().clone();
    let claim_err = claim_done.clone();
    let claim = claim_done.clone();
    let taken = claim.taken().expect("take claim carries its slice").clone();
    let receipt_key = claim.receipt().expect("take claim carries a receipt").source_key().as_str().to_owned();
    let moved_id = taken.item_id;
    guard.may_execute();
    holds.may_execute();
    let active_ship = identity.ship_id;
    let sde_op = sde.clone();
    let result = db.transaction(
        move |tx, outbox: &mut Outbox<MoveEffect>| {
            let (sde, container, reachability, fences, settle, audience) =
                (sde_op.clone(), container.clone(), reachability.clone(), fences.clone(), settle.clone(), audience.clone());
            let (destination_ancestry, destination_parent, taken) =
                (destination_ancestry.clone(), destination_parent.clone(), taken.clone());
            let (claim, receipt_key) = (claim.clone(), receipt_key.clone());
            Box::pin(async move {
                npc_dao::fence(tx, &receipt_key).await.map_err(db_err)?;
                if let Some(stored) = crate::dao::progress::source(tx, "business", &receipt_key).await.map_err(db_err)? {
                    if !crate::space::npc_wrecks::receipt_matches(&stored, claim.receipt().expect("take claim carries a receipt"))? {
                        return Err(CallError::Internal(anyhow::anyhow!("npc loot receipt payload mismatch")));
                    }
                    // The same frozen attempt already committed: re-reading the
                    // exact source is the proof, so nothing is written again.
                    return Ok((TakeWrite::Preexisting, Vec::new()));
                }
                let stored = npc_dao::insert_taken_slice(tx, &GameItem {
                    item_id: taken.item_id, type_id: taken.type_id, owner_id: taken.owner_id,
                    location_id: taken.location_id, flag_id: taken.flag_id, quantity: taken.quantity,
                    singleton: false, custom_info: taken.custom_info.clone(),
                }).await.map_err(db_err)?;
                let slice = enrich_item(&sde, stored)
                    .ok_or_else(|| CallError::Internal(anyhow::anyhow!("npc loot slice type missing from SDE")))?;
                use crate::dao::items::parent_locks::{LockRequest, LockStrength};
                let mut requests = vec![LockRequest { item_id: slice.item_id, strength: LockStrength::Update }];
                requests.extend(super::destination::parent_requests(&sde,
                    destination_ancestry.iter().map(|row| (row.item_id, row.type_id)),
                    LockStrength::Update, LockStrength::Share)?);
                for row in destination_ancestry.iter().filter(|row| row.item_id == location
                    && super::hold_storage::is_ordinary_container(&sde, row.type_id)) {
                    requests.push(LockRequest { item_id: row.item_id, strength: LockStrength::Update });
                }
                let locks = super::destination::lock_items(tx, &sde, &requests, &[]).await?;
                super::super::combat::loaded::settle_in(tx, &settle).await?;
                if !locks.row(slice.item_id).is_some_and(|row| row.matches_item_metadata(&slice))
                    || crate::dao::items::deliveries::ancestry(tx, location).await.map_err(db_err)? != destination_ancestry {
                    return Err(denied());
                }
                let _parent_guard = destination_parent.as_ref().filter(|parent| parent.category_id != i64::from(CATEGORY_SHIP))
                    .map(|parent| ParentSnapshot { item: parent.clone(), ancestry: destination_ancestry.clone() })
                    .map(|snapshot| super::destination::ParentGuard::revalidate(&locks, &[snapshot])).transpose()?;
                let Some(mut destination) = super::movement::authorize_destination(tx, &sde, &actor_identity, &reachability, &container, Some(destination_flag)).await?
                    .filter(|destination| destination.location == location && destination.flag == destination_flag)
                else { return Err(denied()) };
                let audience = match &audience { Some(audience) => Some(audience.resolve(tx, &sde).await?), None => None };
                let owner = destination.item_owner;
                if destination_fitting {
                    let hull = load_items(tx, &sde, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next().ok_or_else(denied)?;
                    if hull.owner_id != owner || hull.category_id != i64::from(CATEGORY_SHIP) || !hull.singleton
                        || (active_ship != Some(location) && !(station == Some(hull.location_id) && hull.flag_id == FLAG_HANGAR)) {
                        return Err(denied());
                    }
                }
                let flag = if destination_flag == FLAG_AUTOFIT && destination_fitting {
                    super::super::effects::fitting::free_slot(tx, sde.clone(), owner, location, slice.item_id, slice.type_id).await?
                        .ok_or_else(|| notify_error(messages::fitting::SLOTS))?
                } else { super::movement::classify_move_flag(slice.category_id, destination_flag).0 };
                destination.flag = flag;
                let before = if destination_fitting {
                    Some(super::super::effects::online::fitting::capture(tx, &sde, &fences, owner, location).await?)
                } else { None };
                let actor = if audience.is_some() { char_id } else { destination.item_owner };
                let moved_id = move_core(tx, &sde, outbox, &slice, &destination, None, Custody::Preserve,
                    &locks, None, audience.as_ref(), actor).await?
                    .ok_or_else(denied)?;
                if let Some(audience) = &audience {
                    if let Some(moved) = load_items(tx, &sde, ItemQuery::Id(moved_id)).await.map_err(db_err)?.into_iter().next() {
                        let mut change = vec![(IX_LOCATION_ID, Value::Int(slice.location_id)), (IX_FLAG, Value::Int(i64::from(slice.flag_id)))];
                        if slice.owner_id != moved.owner_id { change.push((IX_OWNER_ID, Value::Int(slice.owner_id))); }
                        audience.item_change(outbox, &slice, &moved, change, char_id);
                    }
                }
                if super::is_fitting_flag(flag) {
                    super::super::effects::fitting::validate_fit(tx, sde.clone(), owner, location, moved_id).await?;
                }
                if let Some(before) = before {
                    let final_fit = super::super::effects::online::reconcile_core(tx, &sde, outbox, &fences, owner, location).await?;
                    super::super::effects::online::fitting::finish(tx, before, final_fit, outbox).await?;
                }
                let receipt = claim.receipt().expect("take claim carries a receipt");
                outbox.event_from(receipt.source_key(), receipt.event().clone());
                let mut seed_sets = Vec::new();
                if held && super::is_fitting_flag(flag) {
                    seed_sets.push((location, super::super::combat::loaded::seed_entries(tx, location, &[(moved_id, flag)]).await?));
                }
                Ok((TakeWrite::Applied, seed_sets))
            })
        },
        move |write, outbox| async move {
            // The claim disposition is always the first post-commit step: a
            // later notification failure must never roll the source back.
            guard.committed();
            world.with(system, move |s| { s.finish_npc_wreck(&claim_done, NpcWreckDisposition::Committed); }).await;
            dispatch_move(write, outbox, world, notify)
        },
    ).await;
    match result {
        Ok((TakeWrite::Applied, seeds)) => {
            holds.commit(world, &seeds).await;
            Ok(Some(moved_id))
        }
        Ok((TakeWrite::Preexisting, _)) => {
            // This attempt only fenced and read: the dirty rows captured for
            // this hold were not settled by it, so they take the reseed path.
            holds.unknown(db, world).await;
            Ok(Some(moved_id))
        }
        Err(error) => {
            // The result alone cannot prove rollback: seal this attempt, let
            // the same-fence proof decide, then answer honestly.
            let seal = claim_err.clone();
            let _ = world.with(system, move |s| { s.finish_npc_wreck(&seal, NpcWreckDisposition::Unknown); }).await;
            let disposition = world.reconcile_npc_wreck(claim_err).await;
            holds.unknown(db, world).await;
            match disposition {
                Ok(NpcWreckDisposition::Committed) => Ok(Some(moved_id)),
                Ok(NpcWreckDisposition::RolledBack) => Err(error),
                Ok(NpcWreckDisposition::Unknown) | Err(_) => Err(busy()),
            }
        }
    }
}

fn warn_take(char_id: i64, item_id: i64, container: &Container) {
    let destination = match container { Container::Hangar { station_id, .. } | Container::Deliveries { station_id, .. } => *station_id, Container::Item(id) => *id };
    tracing::warn!(char_id, item_id, destination, "inventory move authorization refused");
}

/// One claimed transient delete: the guard plus the root and children its
/// committed apply removes.
pub(super) struct ClaimedDelete {
    pub(super) guard: NpcWreckGuard,
    pub(super) items: i64,
}

/// Claim every transient root of a delete batch in one actor turn. Any root
/// not ready fails the whole batch; the already-taken guards release
/// themselves as never-executed on drop. `persistent_roots` and `persistent`
/// freeze the mixed batch's complete pre-delete preimage into every claim so a
/// later proof re-verifies exactly what this batch was authorized to destroy.
/// A late sealed collision is proved once and then refused; the turn is never
/// repeated with the captured preimage.
enum ClaimTurn {
    Done(Vec<ClaimedDelete>),
    Busy(Option<Arc<NpcWreckClaim>>),
    Failed(CallError),
}

pub(super) async fn claim_deletes(
    world: &World, sde: &Arc<Sde>, system: i64, access: NpcWreckAccess, specs: Vec<(i64, Vec<i64>)>,
    persistent_roots: Vec<i64>, persistent: Vec<Item>,
) -> Result<Vec<ClaimedDelete>, CallError> {
    let turn = world.with(system, move |s| {
        let mut guards = Vec::with_capacity(specs.len());
        for (root, items) in &specs {
            let view = match s.npc_wreck_recovery_view(&access, *root) {
                NpcWreckLookup::Ready(view) => view,
                NpcWreckLookup::Absent | NpcWreckLookup::Denied => return ClaimTurn::Failed(denied()),
                NpcWreckLookup::Busy(claim) => return ClaimTurn::Busy(claim),
            };
            let items = if items.contains(root) {
                let mut frozen = vec![*root];
                frozen.extend(view.items.iter().map(|item| item.item_id));
                frozen
            } else {
                items.clone()
            };
            let count = match i64::try_from(items.len()) {
                Ok(count) => count,
                Err(e) => return ClaimTurn::Failed(CallError::Internal(e.into())),
            };
            match s.claim_npc_wreck(&view.identity, access.clone(), NpcWreckOperation::Delete {
                items,
                persistent_roots: persistent_roots.clone(),
                persistent: persistent.clone(),
            }) {
                NpcWreckLookup::Ready(guard) => guards.push(ClaimedDelete { guard, items: count }),
                NpcWreckLookup::Absent | NpcWreckLookup::Denied => return ClaimTurn::Failed(denied()),
                NpcWreckLookup::Busy(claim) => return ClaimTurn::Busy(claim),
            }
        }
        ClaimTurn::Done(guards)
    }).await;
    match turn {
        Some(ClaimTurn::Done(guards)) => Ok(guards),
        Some(ClaimTurn::Busy(Some(claim))) if claim.prepared() => {
            let _ = reconcile_claim(world, sde, claim).await;
            Err(busy())
        }
        Some(ClaimTurn::Busy(_)) | None => Err(busy()),
        Some(ClaimTurn::Failed(error)) => Err(error),
    }
}

/// Seal every claim of a batch as commit-unknown after an ambiguous outcome,
/// then prove each one once through the matching fenced read.
pub(super) async fn reconcile_deletes(world: &World, sde: &Arc<Sde>, claims: &[Arc<NpcWreckClaim>]) {
    for claim in claims {
        let sealed = claim.clone();
        let system = claim.system();
        let _ = world.with(system, move |s| { s.finish_npc_wreck(&sealed, NpcWreckDisposition::Unknown); }).await;
        let _ = reconcile_claim(world, sde, claim.clone()).await;
    }
}

/// Finish every claimed delete as committed; each apply is its own atomic
/// actor turn, and a `false` only means the claim already resolved.
pub(super) async fn commit_deletes(guards: Vec<ClaimedDelete>) -> bool {
    let mut applied = true;
    for claimed in guards {
        applied &= claimed.guard.finish(NpcWreckDisposition::Committed).await;
    }
    applied
}

/// Arm every guard's in-flight disposition right before the batch transaction.
pub(super) fn may_execute_deletes(guards: &mut [ClaimedDelete]) {
    for claimed in guards { claimed.guard.may_execute(); }
}


/// Whether a transient kind is a renameable container: wrecks keep the
/// original no-rename rule, only the loose-loot container may be labelled.
pub(super) fn renameable(view: &NpcWreckView) -> bool {
    view.kind == NpcWreckKind::LooseLootContainer
}

#[cfg(all(test, any(feature = "tests-inventory", feature = "tests-effects")))]
pub(in crate::services) mod tests;
