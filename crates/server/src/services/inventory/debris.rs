//! GM space debris cleanup behind `/sysjunkclear` and `/testclear`.
//! No client inventory module is mirrored: the client's `svc.slash` only forwards
//! the command line. Roots are selected from the live system actor, which these
//! commands never load. A live or parked claim refuses the batch. A sealed
//! Unknown GM delete is proved once before any fresh capture: committed work
//! only finishes that old scene, and a proved rollback is the only path into a
//! newly authorized cleanup. Fresh roots are then bound to their captured
//! `game.items` rows and the whole mixed-ownership batch is destroyed through
//! the shared inventory lifecycle in one transaction, with pending-reload, job,
//! clone, in-use hull and protected-type refusals kept intact. Post-commit, the
//! still-matching balls are removed through `System::remove_ball`; a
//! committed-but-unconfirmed removal is reported honestly rather than rolled
//! back. There is no TTL, restart restoration or private notification replay.
//! Debris expiry, wreck respawn, cold-system cleanup and arbitrary scope
//! arguments are not supported.

use super::lifecycle::{self, CustodyRule, Operation, Prepared};
use super::movement::{dispatch_move, MoveEffect};
use super::notifications::Audience;
use super::reachability::Endpoint;
use crate::dao::items::{deliveries::ancestry, inventory as inventory_dao, loading::ItemQuery};
use crate::db::outbox::Outbox;
use crate::sde::Sde;
use crate::services::effects::online::{self, OnlineFences};
use crate::services::identity::Identity;
use crate::services::{db_err, items::load_items, market, notify_error, CallError};
use crate::space::{NotifyTx, System, World};
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP};
use infinity_core::messages;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Server design: fixed ball-center distance for `/testclear`, sampled on the
/// actor's interpolation clock and inclusive of the boundary.
const NEARBY_CLEAR_RADIUS_M: f64 = 20_000.0;

/// Which roots a cleanup admits.
#[derive(Clone, Copy)]
pub(in crate::services) enum CleanupScope {
    /// Every debris ball in the system's actor membership.
    System,
    /// Debris within `NEARBY_CLEAR_RADIUS_M` of the caller's live ship.
    Nearby,
}

/// The committed outcome of one cleanup.
pub(in crate::services) struct CleanupReport {
    /// Destroyed root count.
    pub roots: i64,
    /// Destroyed item row count, roots included.
    pub items: i64,
    /// Whether the actor callback confirmed every selected root was removed or
    /// already absent. `false` means the rows are deleted but the live scene
    /// could not be confirmed; it is never a rollback.
    pub space_applied: bool,
}

/// The compact root snapshot captured inside the actor. A busy root keeps the
/// actual claim so an old sealed delete can be proved before any fresh capture.
#[derive(Clone)]
struct DebrisRoot {
    item_id: i64,
    type_id: i32,
    owner_id: i64,
    /// The actor owns this root and its plain loot: no `game.items` rows exist.
    transient: bool,
    claim: Option<std::sync::Arc<crate::space::system::npc_wrecks::NpcWreckClaim>>,
}

fn refused() -> CallError {
    notify_error(messages::gm::GM_DEBRIS_REFUSED)
}

/// Pick the admissible debris roots from the actor's ball membership. `Nearby`
/// reads only the caller's own pilot ball as the center; a selected ball must be
/// an unclaimed wreck or ordinary container that is not global and is not an SDE
/// celestial. Sorting happens in the caller, outside this callback.
fn select_roots(
    space: &mut System,
    sde: &Sde,
    identity: &Identity,
    scope: CleanupScope,
) -> Result<Vec<DebrisRoot>, CallError> {
    let center = match scope {
        CleanupScope::System => None,
        CleanupScope::Nearby => {
            let ego = space.pilot_ball(identity.char_id)
                .filter(|ball| Some(ball.id) == identity.ship_id)
                .ok_or_else(|| notify_error(messages::GM_UNDOCK_FIRST))?;
            Some(space.sample_position(ego.id)
                .filter(|pos| pos.iter().all(|v| v.is_finite()))
                .ok_or_else(|| notify_error(messages::GM_UNDOCK_FIRST))?)
        }
    };
    // Collect the admissible balls first: the membership iterator borrows the
    // actor, while the transient lookup needs a mutable turn inside the loop.
    let mut candidates = Vec::new();
    for ball in space.balls() {
        if ball.global || ball.char_id.is_some()
            || sde.map.celestial(ball.id).is_some()
            || !lifecycle::is_debris_type(sde, ball.type_id) {
            continue;
        }
        if let Some(center) = center {
            let near = space.sample_position(ball.id)
                .is_some_and(|pos| pos.iter().all(|v| v.is_finite())
                    && crate::space::dist(center, pos) <= NEARBY_CLEAR_RADIUS_M);
            if !near { continue; }
        }
        candidates.push((ball.id, ball.type_id, ball.owner_id));
    }
    let access = crate::space::system::npc_wrecks::NpcWreckAccess::GmCleanup { character: identity.char_id };
    let mut roots = Vec::with_capacity(candidates.len());
    for (item_id, type_id, owner_id) in candidates {
        let lookup = space.npc_wreck_recovery_view(&access, item_id);
        let (transient, claim) = match lookup {
            crate::space::system::npc_wrecks::NpcWreckLookup::Absent => (false, None),
            crate::space::system::npc_wrecks::NpcWreckLookup::Ready(_) => (true, None),
            crate::space::system::npc_wrecks::NpcWreckLookup::Busy(Some(claim)) => (true, Some(claim)),
            crate::space::system::npc_wrecks::NpcWreckLookup::Busy(None) => return Err(refused()),
            crate::space::system::npc_wrecks::NpcWreckLookup::Denied => return Err(refused()),
        };
        roots.push(DebrisRoot { item_id, type_id, owner_id, transient, claim });
    }
    Ok(roots)
}

/// Destroy every admissible debris root of `system` in one batch transaction.
/// The actor snapshot is bound to the captured rows before the shared lifecycle
/// revalidates the locked closure; after commit the matching balls are removed
/// through one actor callback.
pub(in crate::services) async fn gm_clear(
    identity: &Identity,
    db: &crate::db::Db,
    world: &World,
    sde: &Arc<Sde>,
    sender: NotifyTx,
    system: i64,
    scope: CleanupScope,
) -> Result<CleanupReport, CallError> {
    if matches!(scope, CleanupScope::Nearby)
        && (identity.station_id.is_some() || identity.ship_id.is_none()) {
        return Err(notify_error(messages::GM_UNDOCK_FIRST));
    }
    let selected = world.with(system, {
        let sde = sde.clone();
        let identity = *identity;
        move |space| select_roots(space, &sde, &identity, scope)
    }).await;
    let mut roots = match (scope, selected) {
        (CleanupScope::System, None) => return Ok(CleanupReport { roots: 0, items: 0, space_applied: true }),
        (CleanupScope::Nearby, None) => return Err(notify_error(messages::GM_UNDOCK_FIRST)),
        (_, Some(roots)) => roots?,
    };
    roots.sort_unstable_by_key(|root| root.item_id);
    let Some(first) = roots.first() else {
        return Ok(CleanupReport { roots: 0, items: 0, space_applied: true });
    };
    let mut first_id = first.item_id;
    // Old sealed claims are proved before any descendants, lineage, audience
    // or hold capture. Only an all-rollback result continues, and it captures
    // the current selection once.
    let roots = match prove_selected(world, sde, &roots).await? {
        Proof::Terminal(report) => return Ok(report),
        Proof::Fresh => roots,
        Proof::Released => {
            let selected = world.with(system, {
                let sde = sde.clone();
                let identity = *identity;
                move |space| select_roots(space, &sde, &identity, scope)
            }).await;
            let mut roots = match (scope, selected) {
                (CleanupScope::System, None) => return Err(refused()),
                (CleanupScope::Nearby, None) => return Err(notify_error(messages::GM_UNDOCK_FIRST)),
                (_, Some(roots)) => roots?,
            };
            roots.sort_unstable_by_key(|root| root.item_id);
            if roots.iter().any(|root| root.claim.is_some()) {
                return Err(refused());
            }
            let Some(fresh) = roots.first() else {
                return Ok(CleanupReport { roots: 0, items: 0, space_applied: true });
            };
            first_id = fresh.item_id;
            roots
        }
    };
    let persistent: Vec<DebrisRoot> = roots.iter().filter(|root| !root.transient).cloned().collect();
    let transient: Vec<DebrisRoot> = roots.iter().filter(|root| root.transient).cloned().collect();
    if persistent.is_empty() {
        // Pure transient cleanup: one turn verifies every root and takes all
        // claims, then each apply removes its own root, children and ball.
        // No descendants, no transaction, no economy lock; any busy or denied
        // root refuses the whole batch.
        let specs: Vec<(i64, Vec<i64>)> = transient.iter().map(|root| (root.item_id, vec![root.item_id])).collect();
        let guards = super::npc_loot::claim_deletes(world, sde, system,
            crate::space::system::npc_wrecks::NpcWreckAccess::GmCleanup { character: identity.char_id },
            specs, Vec::new(), Vec::new()).await.map_err(|_| refused())?;
        let report_roots = i64::try_from(roots.len())
            .map_err(|_| CallError::Internal(anyhow::anyhow!("debris root count too large")))?;
        let items: i64 = guards.iter().map(|guard| guard.items).sum();
        let space_applied = super::npc_loot::commit_deletes(guards).await;
        return Ok(CleanupReport { roots: report_roots, items, space_applied });
    }
    let root_ids: Vec<i64> = persistent.iter().map(|root| root.item_id).collect();
    let closure = inventory_dao::descendants(db, &root_ids).await.map_err(db_err)?;
    let mut snapshots = load_items(db, sde, ItemQuery::Ids(&closure)).await.map_err(db_err)?;
    snapshots.sort_by_key(|item| item.item_id);
    // A row the loader could not enrich must never be silently skipped.
    if snapshots.len() != closure.len() {
        return Err(refused());
    }
    for root in &persistent {
        let item = snapshots.binary_search_by_key(&root.item_id, |item| item.item_id)
            .map(|index| &snapshots[index]).map_err(|_| refused())?;
        if item.type_id != root.type_id || item.owner_id != root.owner_id
            || !item.singleton || item.location_id != system
            || !lifecycle::is_debris_type(sde, item.type_id) {
            return Err(refused());
        }
    }
    let mut lineages = HashMap::new();
    for item in &snapshots {
        if !lineages.contains_key(&item.location_id) {
            lineages.insert(item.location_id, ancestry(db, item.location_id).await.map_err(db_err)?);
        }
    }
    let endpoints: Vec<Endpoint> = snapshots.iter()
        .map(|item| Endpoint { location: item.location_id, flag: item.flag_id }).collect();
    let audience = Audience::capture(identity, db, world, Some(system), &endpoints).await?;
    let online_content = snapshots.iter().any(|item| item.category_id == i64::from(CATEGORY_SHIP)
        || (item.category_id == i64::from(CATEGORY_MODULE) && super::is_fitting_flag(item.flag_id)));
    let fences = if online_content {
        let mut owners: Vec<i64> = snapshots.iter().map(|item| item.owner_id).collect();
        owners.extend(lineages.values().flat_map(|rows| rows.iter().map(|row| row.owner_id)));
        owners.push(identity.char_id);
        owners.sort_unstable();
        owners.dedup();
        online::capture_online_fences(world, &owners).await
    } else {
        // The empty membership never reads the ship argument.
        OnlineFences::single(system, first_id, None)
    };
    let mut report_items = i64::try_from(snapshots.len())
        .map_err(|_| CallError::Internal(anyhow::anyhow!("debris closure too large")))?;
    let report_roots = i64::try_from(roots.len())
        .map_err(|_| CallError::Internal(anyhow::anyhow!("debris root count too large")))?;
    let prepared = Arc::new(Prepared {
        roots: root_ids,
        snapshots,
        lineages,
        outer: system,
        boundary: None,
        endpoints: Vec::new(),
        reach: None,
        shared_ids: HashSet::new(),
        active_ship: identity.ship_id,
        station: None,
        notify: None,
    });
    let custody = CustodyRule::GmDebris { system };
    let audience = Arc::new(audience);
    let char_id = identity.char_id;
    let removal = prepared.clone();
    let mut hulls = std::collections::BTreeSet::new();
    for item in &prepared.snapshots {
        if item.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP) { hulls.insert(item.item_id); }
        else if item.category_id == i64::from(infinity_core::consts::CATEGORY_MODULE) && super::is_fitting_flag(item.flag_id) { hulls.insert(item.location_id); }
    }
    let ships: Vec<_> = hulls.iter().filter_map(|&ship| fences.membership(ship).map(|(system, _)| (system, ship))).collect();
    let armed = super::super::combat::loaded::LoadedHolds::arm(world, &ships, notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let _lock = market::lock().await;
    let mut holds = super::super::combat::loaded::LoadedHolds::lock(world, armed, notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let settle = holds.settle_rows();
    let held_ships = holds.ships();
    let sde_op = sde.clone();
    let sde_removal = sde.clone();
    // Mixed batch: every transient root is claimed under the economy lock with
    // the whole persistent pre-delete preimage frozen into each operation, so
    // a later proof re-verifies exactly what this batch was authorized to
    // destroy. Any failed claim releases the earlier guards and refuses the
    // batch before the transaction exists.
    let specs: Vec<(i64, Vec<i64>)> = transient.iter().map(|root| (root.item_id, vec![root.item_id])).collect();
    let mut deletes = super::npc_loot::claim_deletes(world, sde, system,
        crate::space::system::npc_wrecks::NpcWreckAccess::GmCleanup { character: identity.char_id },
        specs, prepared.roots.clone(), prepared.snapshots.clone()).await?;
    let claims: Vec<std::sync::Arc<crate::space::system::npc_wrecks::NpcWreckClaim>> =
        deletes.iter().map(|claimed| claimed.guard.claim().clone()).collect();
    let transient_items: i64 = deletes.iter().map(|claimed| claimed.items).sum();
    report_items += transient_items;
    let fence_keys: Vec<String> = claims.iter()
        .map(|claim| super::npc_loot::delete_fence_key(claim.root(), claim.serial())).collect();
    super::npc_loot::may_execute_deletes(&mut deletes);
    holds.may_execute();
    let outcome = db.transaction(
        move |tx, outbox: &mut Outbox<MoveEffect>| {
            let settle = settle.clone();
            let held_ships = held_ships.clone();
            let (sde, prepared, audience, fences, fence_keys) = (sde_op.clone(), prepared.clone(), audience.clone(), fences.clone(), fence_keys.clone());
            Box::pin(async move {
                // The operation fences come first: the same-named locks prove
                // later whether this backend finished before it answers.
                for key in &fence_keys {
                    crate::dao::items::npc_loot::fence(tx, key).await.map_err(db_err)?;
                }
                let mut verified = lifecycle::verify_closure(tx, &sde, &prepared, &custody, Operation::Destroy).await?;
                verified.loaded_writes = settle;
                let resolved = audience.resolve(tx, &sde).await?;
                lifecycle::destroy(tx, &sde, outbox, &prepared, &custody, Some(&resolved), &fences, char_id, verified).await?;
                let mut seed_sets = Vec::with_capacity(held_ships.len());
                for ship in held_ships {
                    let touched: Vec<_> = prepared.snapshots.iter()
                        .filter(|item| item.category_id == i64::from(infinity_core::consts::CATEGORY_MODULE)
                            && super::is_fitting_flag(item.flag_id) && item.location_id == ship)
                        .map(|item| (item.item_id, item.flag_id)).collect();
                    seed_sets.push((ship, super::super::combat::loaded::seed_entries(tx, ship, &touched).await?));
                }
                Ok(seed_sets)
            })
        },
        move |seed_sets, outbox| async move {
            // Memory claims finish before the original notifications and the
            // persistent ball removal, exactly once per committed batch.
            let memory_applied = super::npc_loot::commit_deletes(deletes).await;
            dispatch_move((), outbox, world, |notification| {
                online::dispatch_notification(notification, world, &sender, Some(char_id))
            })?;
            let applied = world.with(system, move |space| {
                remove_committed_roots(space, &sde_removal, system, &removal.roots, &removal.snapshots)
            }).await;
            let space_applied = memory_applied && applied == Some(true);
            if !space_applied {
                tracing::warn!(system, "debris destruction committed but actor removal is unconfirmed");
            }
            Ok((CleanupReport { roots: report_roots, items: report_items, space_applied }, seed_sets))
        },
    ).await;
    let seeds = match &outcome { Ok((_, seed_sets)) => seed_sets.clone(), _ => Vec::new() };
    if outcome.is_err() {
        // The persistent write's outcome is unproven: seal each delete claim
        // and prove it once through the same-named fence before answering.
        super::npc_loot::reconcile_deletes(world, sde, &claims).await;
    }
    match holds.finish(db, world, outcome, &seeds).await {
        Ok((report, _)) => Ok(report),
        Err(CallError::User { msg, .. }) if msg == messages::MSG_CANNOT_TRASH_ITEM => Err(refused()),
        Err(error) => Err(error),
    }
}

/// Remove persistent debris balls whose current scene identity still matches
/// the frozen rows. Every root is checked before any ball is removed, so a
/// mismatch leaves the scene untouched. An already absent ball is confirmed.
pub(super) fn remove_committed_roots(space: &mut System, sde: &Sde, system: i64, roots: &[i64], snapshots: &[crate::dao::items::loading::Item]) -> bool {
    let matches = |id: i64| {
        let Some(item) = snapshots.iter().find(|item| item.item_id == id) else { return false };
        space.ball(id).is_none() || space.ball(id).is_some_and(|ball|
            ball.type_id == item.type_id && ball.owner_id == item.owner_id
            && ball.char_id.is_none() && !ball.global && item.location_id == system
            && sde.map.celestial(ball.id).is_none()
            && lifecycle::is_debris_type(sde, ball.type_id))
    };
    if !roots.iter().all(|&id| matches(id)) { return false; }
    for &id in roots {
        if space.ball(id).is_some() { space.remove_ball(id); }
    }
    true
}

/// One proof of the claims this selection already observed. Committed work is
/// terminal; only every eligible claim rolling back authorizes one fresh
/// cleanup. A live, parked or non-GM claim is refused and never counted.
enum Proof {
    Fresh,
    Released,
    Terminal(CleanupReport),
}

async fn prove_selected(world: &World, sde: &Arc<Sde>, roots: &[DebrisRoot]) -> Result<Proof, CallError> {
    let claims: Vec<_> = roots.iter().filter_map(|root| root.claim.clone()).collect();
    if claims.is_empty() { return Ok(Proof::Fresh); }
    let mut outcomes = Vec::with_capacity(claims.len());
    for claim in &claims {
        let eligible = matches!(claim.access(), crate::space::system::npc_wrecks::NpcWreckAccess::GmCleanup { .. })
            && matches!(claim.operation(), crate::space::system::npc_wrecks::NpcWreckOperation::Delete { .. })
            && world.with(claim.system(), {
                let claim = claim.clone();
                move |space| space.npc_wreck_claim_status(&claim) == Some(crate::space::system::npc_wrecks::NpcWreckClaimState::Unknown)
            }).await == Some(true);
        if !eligible { return Err(refused()); }
        outcomes.push(super::npc_loot::reconcile_claim(world, sde, claim.clone()).await?);
    }
    let committed = outcomes.iter().filter(|outcome| matches!(outcome, crate::space::system::npc_wrecks::NpcWreckDisposition::Committed)).count();
    let rolled_back = outcomes.iter().filter(|outcome| matches!(outcome, crate::space::system::npc_wrecks::NpcWreckDisposition::RolledBack)).count();
    if committed > 0 && rolled_back > 0 { return Err(refused()); }
    if committed == claims.len() {
        let selected: std::collections::HashSet<i64> = roots.iter().map(|root| root.item_id).collect();
        let mut accounted = std::collections::HashSet::new();
        let mut items = std::collections::HashSet::new();
        for claim in &claims {
            let crate::space::system::npc_wrecks::NpcWreckOperation::Delete { items: deleted, persistent, .. } = claim.operation() else {
                return Err(refused());
            };
            accounted.insert(claim.root());
            accounted.extend(persistent.iter().filter(|item| item.singleton && item.location_id == claim.system()).map(|item| item.item_id));
            items.extend(deleted.iter().copied());
            items.extend(persistent.iter().map(|item| item.item_id));
        }
        if !selected.is_subset(&accounted) { return Err(refused()); }
        let roots = i64::try_from(selected.len())
            .map_err(|_| CallError::Internal(anyhow::anyhow!("debris root count too large")))?;
        let items = i64::try_from(items.len())
            .map_err(|_| CallError::Internal(anyhow::anyhow!("debris closure too large")))?;
        return Ok(Proof::Terminal(CleanupReport { roots, items, space_applied: true }));
    }
    if rolled_back == claims.len() { return Ok(Proof::Released); }
    Err(refused())
}

#[cfg(all(test, feature = "tests-inventory"))]
mod tests;
