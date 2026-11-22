//! Shared wreck recovery settlement for the salvager module and salvage drones.
//! The client dogma salvaging effect uses fitted accessDifficultyBonus and the
//! wreck's SDE accessDifficulty; the additive percent scaling is server policy.
//! Native wreck yield tables do not ship: rewards are a configured random
//! selection of published Salvaged Materials types, drawn once outside the
//! transaction retry. One admitted end runs one transaction: the touched
//! ship's pending loaded rows settle, the yield lands in the module ship's
//! hold or in the drone's own cargo, and the WreckSalvaged fact is journaled
//! with the same commit. A persistent wreck root is claimed in the database
//! and its loose loot preserved in a positioned cargo container; a transient
//! actor-owned root settles through its one frozen claim, which keeps the
//! success draw, replacement ball and commit receipt so waiting IDs, retries
//! and ambiguous outcomes never re-roll. The durable outcome is
//! authoritative; live container publication is best effort and failures are
//! warned, with no automatic recovery.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use marshal::Value;
use infinity_core::consts::{CATEGORY_DRONE, FLAG_CARGO, GROUP_CARGO_CONTAINER, GROUP_SALVAGED_MATERIALS, IX_LOCATION_ID};
use infinity_core::messages;
use crate::dao::items::{creation, inventory, loading::{self, ItemQuery}, npc_loot, parent_locks::{LockStrength, LockedItems}, salvage};
use crate::db::{Db, outbox::{Effect as OutboxEffect, Outbox, SourceKey}, executor::Executor};
use crate::sde::Sde;
use crate::services::{CallError, db_err, fit::{Key, PERCENT_SCALE}, items::{load_items, DepositPreparation, deposit_prepared, deposit_held}, market, notify_error, rand01, user_error,
    combat::{loaded::{settle_in, LoadedHolds}, state, targeting_charges::targets_of},
    inventory::{destination::{self, Destination}, hold_storage::{self, Addition, CapacitySource, HoldCapacity}, notifications::notify_item_change}};
use crate::space::{Ball, MODE_RIGID, NotifyTx, World,
    npc_wrecks::NpcWreckGuard,
    system::npc_wrecks::{NpcWreckAccess, NpcWreckClaim, NpcWreckClaimState, NpcWreckDisposition, NpcWreckLookup, NpcWreckOperation, RecoveryAttempt}};
use crate::lua::events::{GameEvent, resources::{FactContext, FactOrigin}};

/// SDE dgmAttributeTypes: accessDifficulty, highIsGood=true, unit 121 (realPercent).
/// The additive sign and percent scaling are server policy.
const ATTR_ACCESS_DIFFICULTY: i32 = 901;

/// The success probability of one cycle end against the wreck's difficulty.
pub(in crate::services) fn chance(bonus: f64, difficulty: f64) -> f64 {
    ((bonus + difficulty) / PERCENT_SCALE).clamp(0.0, 1.0)
}

/// The wreck type's fitted access difficulty; a missing attribute refuses the roll.
pub(in crate::services) fn difficulty(sde: &Sde, type_id: i32) -> Option<f64> {
    sde.types.attribute(type_id, ATTR_ACCESS_DIFFICULTY)
}

/// One success roll's published Salvaged Materials draw: the configured stack
/// count picks without replacement, each quantity in the configured range.
/// The draw is frozen once here so a transaction retry never re-rolls.
pub(in crate::services) fn select_yields(sde: &Sde) -> Result<Vec<(i32, i64)>, CallError> {
    let mut pool: Vec<_> = sde.types.group_types(GROUP_SALVAGED_MATERIALS).iter().copied()
        .filter(|id| sde.types.get(*id).is_some_and(|t| t.published == Some(true)))
        .collect();
    if pool.is_empty() { return Err(CallError::Internal(anyhow::anyhow!("published salvage materials are missing"))); }
    let policy = &crate::config::game().salvage;
    let count = (policy.salvage_material_stacks as usize).min(pool.len());
    let mut yields = Vec::with_capacity(count);
    for i in 0..count {
        let j = i + (rand01() * (pool.len() - i) as f64) as usize;
        pool.swap(i, j);
        let width = u64::from(policy.salvage_quantity_max) - u64::from(policy.salvage_quantity_min) + 1;
        let quantity = u64::from(policy.salvage_quantity_min) + (rand01() * width as f64) as u64;
        yields.push((pool[i], quantity as i64));
    }
    Ok(yields)
}

/// Where one admitted recovery delivers its yield. The drone destination is
/// the drone item itself: its cargo is not capacity checked.
#[derive(Clone, Copy)]
pub(in crate::services) enum RewardDestination {
    Ship { ship: i64, capacity: HoldCapacity },
    Drone { item: i64, owner: i64, type_id: i32 },
}

/// Who is settling: a running module cycle or an in-flight drone order.
/// A stale or cancelled authority vetoes admission; once admitted the
/// settlement may complete with the durable custody locks authoritative.
pub(in crate::services) enum RecoveryAuthority {
    Module { master: i64, generation: Arc<AtomicBool>, membership: u64, presented_effect: i64 },
    Drone { item: i64, generation: u64, membership: u64, cancelled: Arc<AtomicBool> },
}

pub(in crate::services) struct RecoveryRequest {
    pub(in crate::services) system: i64,
    pub(in crate::services) character: i64,
    /// The real controlling ship: its pending loaded rows settle and it is the
    /// WreckSalvaged fact's ship context.
    pub(in crate::services) ship: i64,
    pub(in crate::services) wreck: Ball,
    /// The salvage action range the caller already captured: the module's
    /// MasterPacket input or the drone order's contact range, never refit.
    pub(in crate::services) range: f64,
    pub(in crate::services) authority: RecoveryAuthority,
    pub(in crate::services) destination: RewardDestination,
    pub(in crate::services) yields: Vec<(i32, i64)>,
}

pub(in crate::services) enum RecoveryOutcome {
    Recovered,
    Unavailable,
    /// A recognized transient source could not run its business write:
    /// `unknown = false` is a proven refusal or a still-waiting parked plan,
    /// `unknown = true` means a possibly-executed attempt stayed unproven.
    /// The original error is carried, never swallowed or wrapped as success.
    Deferred { unknown: bool, error: CallError },
}

/// The module authority's in-memory liveness: the same active activation and
/// a still-held target lock.
fn module_active(master: i64, generation: &Arc<AtomicBool>, character: i64, target: i64) -> bool {
    let current = state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, generation));
    current && targets_of(character).contains(&target)
}

/// Lock the drone root and every yield's candidate stack, then prove the root
/// is still the admitted positioned singleton.
async fn drone_locks<'tx>(
    ex: &'tx (impl Executor + ?Sized), sde: &Arc<Sde>, system: i64,
    item: i64, owner: i64, type_id: i32, preparations: &[DepositPreparation],
) -> Result<LockedItems<'tx>, CallError> {
    let mut requests = destination::parent_requests(sde, [(item, type_id)], LockStrength::Update, LockStrength::Update)?;
    requests.extend(preparations.iter().filter_map(|preparation| preparation.candidate_request()));
    let optional: Vec<_> = preparations.iter().filter_map(|preparation| preparation.candidate_id())
        .filter(|id| !requests.iter().any(|request| request.item_id == *id)).collect();
    let locks = destination::lock_items(ex, sde, &requests, &optional).await?;
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    let row = locks.row(item).ok_or_else(refuse)?;
    if row.type_id != type_id || row.owner_id != owner || row.location_id != system
        || row.flag_id != 0 || !row.singleton || row.quantity != 1 || !locks.is_write_locked(item)
        || sde.types.get(row.type_id).and_then(|t| t.category_id) != Some(CATEGORY_DRONE) {
        return Err(refuse());
    }
    Ok(locks)
}

/// The attempt witness a parked claim must match before it may resume: the
/// module activation's stop token or the drone order's generation.
impl RecoveryAuthority {
    fn attempt(&self) -> RecoveryAttempt {
        match self {
            Self::Module { master, generation, .. } =>
                RecoveryAttempt::Module { master: *master, stop: generation.clone() },
            Self::Drone { item, generation, .. } =>
                RecoveryAttempt::Drone { item: *item, generation: *generation },
        }
    }
}

/// The recovery access source and the transient operation's destination are
/// the same ball: the range proof samples the module ship or the drone itself,
/// which is also where the real cargo deposit lands.
fn destination_item(destination: &RewardDestination) -> i64 {
    match destination {
        RewardDestination::Ship { ship, .. } => *ship,
        RewardDestination::Drone { item, .. } => *item,
    }
}


/// The frozen yield draw of this authority's own parked claim on `root`, when
/// one exists: same character, same destination item and the same attempt
/// resume the plan, so the adapters never roll chance or yields again.
pub(in crate::services) async fn parked_yields(
    world: &Arc<World>, system: i64, root: i64, character: i64, ship: i64,
    source: i64, destination: i64, range: f64, attempt: RecoveryAttempt,
) -> Option<Vec<(i32, i64)>> {
    world.with(system, move |s| {
        let access = NpcWreckAccess::Recovery {
            character, ship, source, source_token: s.capture_ball(source)?, range, attempt: attempt.clone(),
        };
        let NpcWreckLookup::Busy(Some(claim)) = s.npc_wreck_recovery_view(&access, root) else { return None };
        let NpcWreckOperation::Salvage { character: c, destination: d, yields, .. } = claim.operation() else { return None };
        let own = *c == character && *d == destination
            && matches!(claim.access(), NpcWreckAccess::Recovery { attempt: stored, .. } if stored.same_attempt(&attempt));
        own.then(|| yields.clone())
    }).await.flatten()
}

/// The authority's liveness evaluated inside one actor turn: the module
/// activation's stop token, the still-held target lock and the resident
/// subject carrying its presented effect, or the drone order still in flight
/// against this wreck.
enum AuthorityGate {
    Module { master: i64, generation: Arc<AtomicBool>, membership: u64, presented_effect: i64 },
    Drone { item: i64, generation: u64, membership: u64, cancelled: Arc<AtomicBool> },
}

fn authority_gate(authority: &RecoveryAuthority) -> AuthorityGate {
    match authority {
        RecoveryAuthority::Module { master, generation, membership, presented_effect } =>
            AuthorityGate::Module { master: *master, generation: generation.clone(), membership: *membership, presented_effect: *presented_effect },
        RecoveryAuthority::Drone { item, generation, membership, cancelled } =>
            AuthorityGate::Drone { item: *item, generation: *generation, membership: *membership, cancelled: cancelled.clone() },
    }
}

fn authority_in_actor(s: &mut crate::space::System, ship: i64, character: i64, target: i64, gate: &AuthorityGate) -> bool {
    match gate {
        AuthorityGate::Module { master, generation, membership, presented_effect } =>
            module_active(*master, generation, character, target) && {
                // The same resident-subject gate the module lifecycle runs:
                // unchanged actor membership and the real module still fitted
                // online carrying its presented effect. `flag` is not
                // consulted by the subject check.
                let module = s.with_combat_fit(ship, |fit, _, _| fit.source_type(Key::Module(*master)))
                    .flatten()
                    .map(|type_id| super::super::lifecycle::Module { id: *master, type_id, flag: 0 });
                module.is_some_and(|module|
                    super::super::lifecycle::source_subject_current(s, ship, character, *membership, &module, *presented_effect, generation))
            },
        AuthorityGate::Drone { item, generation, membership, cancelled } =>
            !cancelled.load(Ordering::Relaxed) && s.drone_salvage_in_flight(*item, *generation, target, *membership),
    }
}

/// What one claiming actor turn resolved about the transient root.
enum ClaimGate {
    /// This attempt owns a fresh or resumed claim.
    Claimed(NpcWreckGuard),
    /// An Unknown-sealed claim needs its one shared fence proof outside the actor.
    Proof(Arc<NpcWreckClaim>),
    /// A live attempt owns the root or the claim serial ran out.
    Busy,
    /// The authority died, admission refused or the source left.
    Gone,
}

/// Resolve an existing claim or take a fresh one inside a single actor turn.
/// A parked plan is resumed only by its own attempt; a never-executed plan
/// whose authority provably died is ended so this attempt may claim fresh;
/// everything still live stays busy and an Unknown seal defers to the fence
/// proof outside. The final authority check runs in this same turn.
#[allow(clippy::too_many_arguments)]
fn claim_gate(
    s: &mut crate::space::System, ship: i64, character: i64, target: i64, destination: i64,
    gate: &AuthorityGate, access: &NpcWreckAccess, attempt: &RecoveryAttempt,
    operation: &NpcWreckOperation, pending: Option<Arc<NpcWreckClaim>>,
) -> ClaimGate {
    if !authority_in_actor(s, ship, character, target, gate) { return ClaimGate::Gone; }
    let mut pending = pending;
    for _ in 0..2 {
        if let Some(claim) = pending.take() {
            match s.npc_wreck_claim_status(&claim) {
                Some(NpcWreckClaimState::WaitingIds) => {
                    let own = matches!(claim.operation(),
                            NpcWreckOperation::Salvage { character: c, destination: d, .. } if *c == character && *d == destination)
                        && matches!(claim.access(),
                            NpcWreckAccess::Recovery { attempt: stored, .. } if stored.same_attempt(attempt));
                    if own {
                        return match s.resume_npc_wreck(&claim, access.clone()) {
                            NpcWreckLookup::Ready(guard) => ClaimGate::Claimed(guard),
                            NpcWreckLookup::Busy(_) => ClaimGate::Busy,
                            _ => ClaimGate::Gone,
                        };
                    }
                    let dead = match claim.access() {
                        NpcWreckAccess::Recovery { character: c, attempt: RecoveryAttempt::Module { master, stop }, .. } =>
                            !module_active(*master, stop, *c, target),
                        NpcWreckAccess::Recovery { attempt: RecoveryAttempt::Drone { item, generation }, .. } =>
                            !s.drone_current(*item, *generation),
                        _ => false,
                    };
                    if !dead { return ClaimGate::Busy; }
                    // A dead authority's never-executed plan keeps no draw:
                    // cancel it now; the next independent request claims fresh.
                    s.cancel_waiting_npc_wreck(&claim);
                    return ClaimGate::Busy;
                }
                Some(NpcWreckClaimState::Unknown) => return ClaimGate::Proof(claim),
                Some(NpcWreckClaimState::Claimed) => return ClaimGate::Busy,
                None => {}
            }
            continue;
        }
        match s.npc_wreck_recovery_view(access, target) {
            NpcWreckLookup::Ready(view) => match s.claim_npc_wreck(&view.identity, access.clone(), operation.clone()) {
                NpcWreckLookup::Ready(guard) => return ClaimGate::Claimed(guard),
                NpcWreckLookup::Busy(claim) => { pending = claim; }
                _ => return ClaimGate::Gone,
            },
            NpcWreckLookup::Busy(claim) => pending = claim,
            _ => return ClaimGate::Gone,
        }
    }
    ClaimGate::Busy
}

/// The shared business body of the single recovery transaction: pending
/// loaded rows settle first, the destination's real locks and capacity are
/// proven, every yield stack is deposited and WreckSalvaged is journaled.
/// A transient source first takes its claim's receipt fence and replays an
/// exact committed fact without writing again; a persistent source claims
/// the durable root and preserves loose loot in a positioned container.
#[allow(clippy::too_many_arguments)]
async fn run_business(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<Option<Ball>>,
    sde: &Arc<Sde>, system: i64, character: i64, ship: i64, target: i64,
    wreck: &Ball, destination: &RewardDestination, yields: &[(i32, i64)],
    settle_rows: &[crate::dao::items::charges::LoadedWrite],
    transient: Option<(&str, &Arc<NpcWreckClaim>)>,
) -> Result<RecoveryOutcome, CallError> {
    if let Some((key, claim)) = transient {
        npc_loot::fence(ex, key).await.map_err(db_err)?;
        match crate::dao::progress::source(ex, "business", key).await.map_err(db_err)? {
            // The same full receipt proof the world reconcile runs: an exact
            // match replays only the actor finish; any contradiction or a
            // foreign source stays closed, never a second deposit or event.
            Some(envelope) => match claim.receipt() {
                Some(receipt) if crate::space::npc_wrecks::receipt_matches(&envelope, receipt)? =>
                    return Ok(RecoveryOutcome::Recovered),
                _ => return Err(CallError::Internal(anyhow::anyhow!("npc wreck salvage receipt collision"))),
            },
            None => {}
        }
    }
    settle_in(ex, settle_rows).await?;
    let ship_locks = match destination {
        RewardDestination::Ship { ship, capacity } => {
            let additions: Vec<_> = yields.iter().map(|(type_id, quantity)| Addition {
                type_id: *type_id, singleton: false, quantity: *quantity,
            }).collect();
            match hold_storage::check_destination(ex, sde, character, *ship, FLAG_CARGO, &additions,
                Default::default(), CapacitySource::Captured(*capacity)).await {
                Ok(Some(locks)) => Some(locks),
                Ok(None) => return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)),
                Err(CallError::User { msg, .. }) if msg == messages::MSG_NOT_ENOUGH_CARGO_SPACE => return Ok(RecoveryOutcome::Unavailable),
                Err(error) => return Err(error),
            }
        }
        RewardDestination::Drone { .. } => None,
    };
    let mut preparations = Vec::with_capacity(yields.len());
    if let RewardDestination::Drone { item, .. } = destination {
        let destination = Destination::owned(character, *item, FLAG_CARGO);
        for (kind, _) in yields {
            preparations.push(DepositPreparation::capture(ex, sde, &destination, *kind).await?);
        }
    }
    let drone_root_locks = match destination {
        RewardDestination::Drone { item, owner, type_id } => {
            Some(drone_locks(ex, sde, system, *item, *owner, *type_id, &preparations).await?)
        }
        RewardDestination::Ship { .. } => None,
    };
    let preserved = if transient.is_none() {
        if !salvage::claim(ex, target, system, wreck.type_id).await.map_err(db_err)? {
            return Ok(RecoveryOutcome::Unavailable);
        }
        let loot = load_items(ex, sde, ItemQuery::Location(target)).await.map_err(db_err)?;
        if loot.len() != loading::load_items(ex, ItemQuery::Location(target)).await.map_err(db_err)?.len() {
            return Err(CallError::Internal(anyhow::anyhow!("wreck contains an item with missing SDE type")));
        }
        if loot.is_empty() { None } else {
            let container = sde.types.iter().filter(|t| t.group_id == Some(GROUP_CARGO_CONTAINER))
                .min_by_key(|t| t.type_id).map(|t| (t.type_id, t.radius));
            let (kind, radius) = container.ok_or_else(|| CallError::Internal(anyhow::anyhow!("cargo container type missing")))?;
            let id = creation::positioned(ex, kind, wreck.owner_id, system, wreck.pos).await.map_err(db_err)?;
            for item in loot {
                if !inventory::move_item(ex, &item, id, item.flag_id, item.owner_id, item.singleton).await.map_err(db_err)? {
                    return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
                }
                notify_item_change(ex, sde, outbox, item.item_id, vec![(IX_LOCATION_ID, Value::Int(target))]).await?;
            }
            let mut replacement = wreck.clone();
            replacement.id = id; replacement.type_id = kind; replacement.mode = MODE_RIGID;
            replacement.radius = radius.unwrap_or(f64::from(wreck.radius)) as f32;
            replacement.vel = [0.0; 3]; replacement.old_vel = [0.0; 3]; replacement.old_pos = replacement.pos;
            replacement.slim.clear();
            Some(replacement)
        }
    } else { None };
    match destination {
        RewardDestination::Ship { ship, capacity } => {
            for (kind, quantity) in yields {
                deposit_held(ex, sde, outbox, &Destination::owned(character, *ship, FLAG_CARGO),
                    *kind, *quantity, target, ship_locks.as_ref(), CapacitySource::Captured(*capacity)).await?;
            }
        }
        RewardDestination::Drone { .. } => {
            let locks = drone_root_locks.as_ref().expect("drone destination locked");
            for (preparation, (_, quantity)) in preparations.iter_mut().zip(yields) {
                deposit_prepared(ex, sde, outbox, preparation, *quantity, target,
                    Some(locks), None, Default::default(), CapacitySource::Current).await?;
            }
        }
    }
    match transient {
        Some((_, claim)) => {
            let event = claim.receipt().map(|receipt| receipt.event().clone())
                .ok_or_else(|| CallError::Internal(anyhow::anyhow!("salvage claim without receipt")))?;
            outbox.event_from(SourceKey::canonical("WreckSalvaged", target, 0), event);
        }
        None => {
            let org = crate::dao::characters::members::organization(ex, character).await.map_err(db_err)?;
            let ship_type_id = loading::load_items(ex, ItemQuery::Id(ship)).await.map_err(db_err)?.first().map(|item| item.type_id);
            outbox.event_from(SourceKey::new("WreckSalvaged", target), GameEvent::WreckSalvaged {
                char_id: character, wreck_id: target, wreck_type: wreck.type_id, occurred_at: crate::client::filetime_now(),
                context: FactContext { corporation_id: org.as_ref().map(|o| o.corporation_id), alliance_id: org.and_then(|o| o.alliance_id), faction_id: None, ship_id: Some(ship), ship_type_id, system_id: Some(system), station_id: None, origin: FactOrigin::Natural },
            });
        }
    }
    outbox.operation(preserved);
    Ok(RecoveryOutcome::Recovered)
}

/// Run one admitted recovery: pending loaded settles first, then the single
/// durable transaction delivers the yield and journals WreckSalvaged. A
/// persistent source claims the durable root and preserves loose loot; a
/// transient actor-owned source settles through its one frozen claim; the
/// same destination verification and deposit, with the root removal,
/// ordinary-loot container and source notices applied by the actor finish.
/// An unknown transaction outcome is proven once against the claim's
/// receipt fence; a still-unproven attempt defers instead of guessing.
pub(in crate::services) async fn settle(
    db: &Db, world: &Arc<World>, request: RecoveryRequest, sender: Option<NotifyTx>,
) -> Result<RecoveryOutcome, CallError> {
    let RecoveryRequest { system, character, ship, wreck, authority, destination, yields, range } = request;
    let target = wreck.id;
    let busy = || notify_error(messages::fitting::CHARGES_SETTLING);
    let attempt = authority.attempt();
    // The access source is the destination ball itself: the range proof
    // samples the module ship or the drone the yield will land in.
    let (source, destination_id) = { let id = destination_item(&destination); (id, id) };

    // One actor turn classifies the source: only an absent transient root
    // keeps the persistent claim path. The frozen access later proves the
    // same source ball incarnation and the caller-captured range at claim.
    enum Source {
        Persistent,
        Transient { access: NpcWreckAccess, pending: Option<Arc<NpcWreckClaim>> },
        Refused,
    }
    let classified = world.with(system, {
        let (wreck, attempt) = (wreck.clone(), attempt.clone());
        move |s| {
            let Some(source_token) = s.capture_ball(source) else { return Source::Refused };
            let access = NpcWreckAccess::Recovery { character, ship, source, source_token, range, attempt };
            match s.npc_wreck_recovery_view(&access, target) {
                NpcWreckLookup::Ready(view) =>
                    if view.root.type_id == wreck.type_id && view.root.owner_id == wreck.owner_id {
                        Source::Transient { access, pending: None }
                    } else { Source::Refused },
                NpcWreckLookup::Busy(claim) => Source::Transient { access, pending: claim },
                NpcWreckLookup::Denied => Source::Refused,
                NpcWreckLookup::Absent => Source::Persistent,
            }
        }
    }).await.unwrap_or(Source::Refused);
    if matches!(classified, Source::Refused) { return Ok(RecoveryOutcome::Unavailable); }

    // The transient event's organization facts come from one bounded business
    // preparation outside any actor closure; the event the claim carries is
    // frozen here once and replayed verbatim by every later attempt turn.
    let transient = match classified {
        Source::Transient { access, pending } => {
            let org = crate::dao::characters::members::organization(db, character).await.map_err(db_err)?;
            let ship_type_id = loading::load_items(db, ItemQuery::Id(ship)).await.map_err(db_err)?.first().map(|item| item.type_id);
            let event = GameEvent::WreckSalvaged {
                char_id: character, wreck_id: target, wreck_type: wreck.type_id, occurred_at: crate::client::filetime_now(),
                context: FactContext { corporation_id: org.as_ref().map(|o| o.corporation_id), alliance_id: org.and_then(|o| o.alliance_id), faction_id: None, ship_id: Some(ship), ship_type_id, system_id: Some(system), station_id: None, origin: FactOrigin::Natural },
            };
            let operation = NpcWreckOperation::Salvage { character, destination: destination_id, yields: yields.clone(), event };
            Some((access, pending, operation))
        }
        _ => None,
    };

    let armed = LoadedHolds::arm(world, &[(system, ship)], busy()).await?;
    let market = market::lock().await;
    let mut holds = LoadedHolds::lock(world, armed, busy()).await?;
    let settle_rows = holds.settle_rows();
    let held_ships = holds.ships();
    // The recovery touches no charge slot; the seeds lists stay empty and the
    // commit releases the held scope after publishing the settled absolutes.
    let seeds: Vec<(i64, Vec<(crate::dao::items::charges::LoadedSlotKey, Option<crate::space::combat::loaded::LoadedSeed>)>)> =
        held_ships.into_iter().map(|ship| (ship, Vec::new())).collect();
    let sde = world.sde().clone();

    let result = match transient {
        None => {
            // Persistent source: authority admission after the locks, before
            // the transaction may execute; a refused admit settles nothing
            // and invalidates the holds.
            let admitted = world.with(system, {
                let gate = authority_gate(&authority);
                move |s| authority_in_actor(s, ship, character, target, &gate)
            }).await.unwrap_or(false);
            if !admitted {
                holds.unknown(db, world).await;
                return Ok(RecoveryOutcome::Unavailable);
            }
            holds.may_execute();
            let world_for_dispatch = world.clone();
            let market = &market;
            db.transaction(
                move |tx, outbox: &mut Outbox<Option<Ball>>| {
                    let (sde, yields, wreck, settle_rows) = (sde.clone(), yields.clone(), wreck.clone(), settle_rows.clone());
                    let destination = destination;
                    Box::pin(async move {
                        run_business(tx, outbox, &sde, system, character, ship, target,
                            &wreck, &destination, &yields, &settle_rows, None).await
                    })
                },
                move |value, outbox| {
                    let sender = sender.clone();
                    async move {
                        let world = &world_for_dispatch;
                        for entry in outbox.into_entries() {
                            match entry {
                                OutboxEffect::Notification(notification) => notification.dispatch_sender(world, sender.as_ref())?,
                                OutboxEffect::Operation(replacement) => {
                                    world.remove_ball(system, target).await;
                                    if let Some(ball) = replacement {
                                        let replacement_id = ball.id;
                                        if let Err(error) = world.add_ship_locked(system, ball, market).await {
                                            tracing::warn!(system, wreck_id = target, replacement_id, %error, "failed to publish salvaged wreck container");
                                        }
                                    }
                                }
                            }
                        }
                        Ok(value)
                    }
                },
            ).await
        }
        Some((access, mut pending, operation)) => {
            // Claim, resume or resolve the root inside bounded actor turns;
            // a contested root can never spin.
            let mut guard = {
                let mut rounds = 3u8;
                loop {
                    let claimed = world.with(system, {
                        let (access, operation, attempt, gate) =
                            (access.clone(), operation.clone(), attempt.clone(), authority_gate(&authority));
                        let pending = pending.take();
                        move |s| claim_gate(s, ship, character, target, destination_id, &gate, &access, &attempt, &operation, pending)
                    }).await.unwrap_or(ClaimGate::Gone);
                    match claimed {
                        ClaimGate::Claimed(guard) => break guard,
                        ClaimGate::Gone => {
                            holds.unknown(db, world).await;
                            return Ok(RecoveryOutcome::Unavailable);
                        }
                        ClaimGate::Busy => {
                            holds.unknown(db, world).await;
                            return Ok(RecoveryOutcome::Deferred { unknown: false, error: busy() });
                        }
                        ClaimGate::Proof(claim) => match world.reconcile_npc_wreck(claim).await {
                            Ok(NpcWreckDisposition::Committed) => {
                                holds.unknown(db, world).await;
                                return Ok(RecoveryOutcome::Unavailable);
                            }
                            Ok(NpcWreckDisposition::RolledBack) => {
                                rounds -= 1;
                                if rounds == 0 {
                                    holds.unknown(db, world).await;
                                    return Ok(RecoveryOutcome::Deferred { unknown: false, error: busy() });
                                }
                            }
                            Ok(NpcWreckDisposition::Unknown) => {
                                holds.unknown(db, world).await;
                                return Ok(RecoveryOutcome::Deferred { unknown: true, error: busy() });
                            }
                            Err(error) => {
                                holds.unknown(db, world).await;
                                return Ok(RecoveryOutcome::Deferred { unknown: true, error });
                            }
                        },
                    }
                }
            };
            let claim = guard.claim().clone();
            if !claim.prepared() {
                // Replacement IDs are still exhausted: park the never-executed
                // plan with its frozen draw and refuse busy; a later attempt
                // of the same authority resumes it instead of re-rolling.
                guard.park();
                holds.unknown(db, world).await;
                return Ok(RecoveryOutcome::Deferred { unknown: false, error: busy() });
            }
            // The claim's frozen plan is authoritative: the operation's yields
            // and the receipt's event replay, not this request's fresh values.
            let yields = match claim.operation() {
                NpcWreckOperation::Salvage { yields, .. } => yields.clone(),
                _ => yields,
            };
            let key = SourceKey::canonical("WreckSalvaged", target, 0).as_str().to_owned();
            holds.may_execute();
            guard.may_execute();
            let world_for_dispatch = world.clone();
            let claim_dispatch = claim.clone();
            let claim_tx = claim.clone();
            let result = db.transaction(
                move |tx, outbox: &mut Outbox<Option<Ball>>| {
                    let (sde, yields, wreck, settle_rows, key, claim) =
                        (sde.clone(), yields.clone(), wreck.clone(), settle_rows.clone(), key.clone(), claim_tx.clone());
                    let destination = destination;
                    Box::pin(async move {
                        run_business(tx, outbox, &sde, system, character, ship, target,
                            &wreck, &destination, &yields, &settle_rows, Some((key.as_str(), &claim))).await
                    })
                },
                move |value, outbox| {
                    // The durable commit is proven: witness it, let the actor
                    // install the claim's frozen source delta, then notify.
                    guard.committed();
                    let (claim, sender, world) = (claim_dispatch.clone(), sender.clone(), world_for_dispatch.clone());
                    async move {
                        let _ = world.with(system, move |s| {
                            s.finish_npc_wreck(&claim, NpcWreckDisposition::Committed);
                        }).await;
                        for entry in outbox.into_entries() {
                            match entry {
                                OutboxEffect::Notification(notification) => notification.dispatch_sender(&world, sender.as_ref())?,
                                OutboxEffect::Operation(_) => {}
                            }
                        }
                        Ok(value)
                    }
                },
            ).await;
            match result {
                // A possibly-executed attempt is proven once against the
                // claim's same-fence receipt, never re-deposited.
                Err(error) => match world.reconcile_npc_wreck(claim).await {
                    Ok(NpcWreckDisposition::Committed) => Ok(RecoveryOutcome::Recovered),
                    disposition => {
                        let unknown = !matches!(disposition, Ok(NpcWreckDisposition::RolledBack));
                        return match holds.finish(db, world, Err::<RecoveryOutcome, _>(error), &seeds).await {
                            Err(error) => Ok(RecoveryOutcome::Deferred { unknown, error }),
                            Ok(_) => Ok(RecoveryOutcome::Deferred {
                                unknown,
                                error: CallError::Internal(anyhow::anyhow!("loaded holds echoed success on a failed settle")),
                            }),
                        };
                    }
                },
                ok => ok,
            }
        }
    };
    holds.finish(db, world, result, &seeds).await
}
