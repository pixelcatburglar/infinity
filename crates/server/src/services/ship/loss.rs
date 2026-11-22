//! Destroys a player ship and replaces it in one database transaction: the
//! single validated assembled frigate parked in the ship's frigate escape bay
//! keeps its own item ID, fitting, cargo and persistent state, and a new
//! capsule is created only when the bay holds no eligible survivor. The
//! transaction has no client RPC or runtime mutation: the ship's remaining
//! active contents closure at any depth, its reservations, charges, banks and
//! mutable attachments are consumed, while killmails, insurance and characters
//! are guarded or updated through their services, then ShipLost is handed to
//! the post-commit runtime dispatcher. The hidden tactical-mode item and its
//! timing row are deleted with the closure but never listed in the killmail,
//! valuation or recovery manifests. Preparation fences the closure,
//! intersecting reservations and external reservation-source ancestry before
//! acquiring item rows; snapshot drift aborts the attempt for the bounded
//! whole-transaction retry. Client-visible work is limited to replacing the
//! existing session and observing the resulting space membership; an escaping
//! frigate keeps only its persisted state, so runtime heat and active cycles
//! are not promised. A piloted capsule-group hull additionally dies with its
//! unprotected active augmentation rows and respawns through the medical home
//! station in the same transaction.
use infinity_core::consts::{FLAG_FRIGATE_ESCAPE_BAY, FLAG_HIDDEN_MODIFIERS, IX_FLAG, IX_LOCATION_ID, TYPE_CAPSULE};
use crate::dao::{characters::selection, items::{boarding, creation, loading::ItemQuery, ship as item_dao}};
use crate::db::{executor::Executor, outbox::{Outbox, SourceKey}};
use crate::lua::events::GameEvent;
use crate::services::{combat::damage::DamageSource, db_err, identity::Identity, CallError,
    effects::online::{OnlineChanged, OnlineFences},
    inventory::{holds::{self, CapacityLimit}, hold_storage, notifications}, items::load_items};
use marshal::Value;
use std::sync::Arc;
use crate::{db::Db, sde::Sde, space::World};
pub(crate) mod facts;
pub(crate) mod medical;

/// Presence preparation releases its market guard before online registration.
/// Registration can therefore occur between the actor-turn check and mutation.
pub(crate) async fn publish_loss(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>,
    outcome: LossOutcome, token: u64, generation: u64, expected: Option<infinity_core::session::OnlineSession>) -> Result<bool, CallError> {
    let ship = identity.ship_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing lost ship")))?;
    let system = identity.system_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing loss system")))?;
    let character = identity.char_id;
    let mut next = expected.clone();
    if let Some(session) = &mut next {
        session.ship_id = Some(outcome.replacement_ship);
        if let Some((station, home_system, _, _)) = outcome.home {
            session.station_id = Some(station);
            session.system_id = Some(home_system);
        }
    }
    // A committed database loss may refresh only the captured session. The
    // native token/generation is checked after this guarded CAS so a stale
    // completion cannot leave a matching old session pointing at the lost hull.
    let prepared = if outcome.home.is_none() {
        Some(super::balls::selected(identity, db, world, sde, Some(system),
            expected.as_ref().and_then(|s| s.alliance_id), outcome.replacement_ship, outcome.position).await?)
    } else { None };
    if world.online_session(character) != next
        && !world.replace_online_session(character, expected.as_ref(), next.clone()) {
        let current_session = world.online_session(character).map(|s| s.session_id);
        let expected_session = expected.as_ref().map(|s| s.session_id);
        tracing::warn!(character, ship, ?expected_session, ?current_session, "committed loss session is stale");
        return Ok(false);
    }
    let expected_session = expected.as_ref().map(|s| s.session_id);
    world.with(system, move |s| {
        if !s.combat_session_current(character, next.as_ref()) {
            let current_session = s.combat_session_id(character);
            tracing::warn!(character, ship, ?expected_session, ?current_session, "committed loss session is stale");
            return false;
        }
        if s.combat_phase(ship) != Some(crate::space::combat::CombatPhase::Dying { token })
            || s.combat_membership_generation(ship) != Some(generation) { return false; }
        // Capture the informed ego ticket before destruction so the queued
        // exit still reaches the old binding and its handoff can re-aim it.
        let Some(departed) = s.capture_ball(ship) else { return false };
        if s.destroy_ball(ship).is_none() { return false; }
        s.finalize_dying(ship, token, generation);
        if let Some(prepared) = prepared {
            prepared.install(s, |s, ball| s.add_loss_replacement(departed, ball, next));
        }
        true
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped after committed loss")))
}

#[derive(Clone)]
pub(crate) struct LossInput {
    pub position: [f64; 3],
    pub source: DamageSource,
    pub lethal_at: i64,
    pub contributions: Vec<crate::space::combat::ContributionEntry>,
    pub victim: crate::lua::events::combat::CombatEntity,
    pub context: crate::lua::events::resources::FactContext,
    pub conflict: crate::lua::events::combat::ConflictKind,
    pub pending: Arc<facts::PlayerLossContext>,
}

#[derive(Clone)]
pub(crate) struct LossOutcome {
    pub replacement_ship: i64,
    pub home: Option<(i64, i64, i64, i64)>,
    pub position: [f64; 3],
}

/// Capture the ship's complete active contents closure, the reservations
/// intersecting it and the ancestry of every physical external reservation
/// source, then acquire the whole request set once through the shared
/// hull-then-nonhull helper. A `Some` body owner merges the owner's medical
/// lock set into the same single acquisition. Any change to those snapshots
/// after fencing aborts
/// the attempt with a retryable stale-snapshot conflict; a missing root has no
/// closure and is an idempotent no-loss. Already-absent external sources stay
/// an absence fact, never a refusal or a lock request.
pub(crate) async fn lock_loss_items<'tx>(
    ex: &'tx (impl Executor + ?Sized), sde: &crate::sde::Sde, ship: i64,
    body_owner: Option<i64>,
) -> Result<Option<crate::dao::items::parent_locks::LockedItems<'tx>>, CallError> {
    use crate::dao::items::parent_locks::{LockRequest, LockStrength};
    let stale = || db_err(crate::db::Error::StaleSnapshot("loss snapshot changed"));
    let closure = crate::dao::items::inventory::descendants(ex, &[ship]).await.map_err(db_err)?;
    if !closure.contains(&ship) { return Ok(None); }
    let captured = crate::dao::items::parent_locks::capture(ex, &closure).await.map_err(db_err)?;
    let reservations = crate::dao::items::pending_reloads::intersecting(ex, &closure).await.map_err(db_err)?;
    let mut physical_sources: Vec<i64> = reservations.iter().map(|row| row.source_location_id)
        .filter(|id| !closure.contains(id) && !crate::services::items::is_static_location(sde, *id)).collect();
    physical_sources.sort_unstable();
    physical_sources.dedup();
    let mut source_lineages = Vec::with_capacity(physical_sources.len());
    for source in &physical_sources {
        source_lineages.push((*source, crate::dao::items::deliveries::ancestry(ex, *source).await.map_err(db_err)?));
    }
    let mut requests: Vec<_> = closure.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
    for (_, ancestry) in &source_lineages {
        requests.extend(crate::services::inventory::destination::parent_requests(sde,
            ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?);
    }
    for (source, ancestry) in &source_lineages {
        if ancestry.iter().any(|row| row.item_id == *source) {
            requests.push(LockRequest { item_id: *source, strength: LockStrength::Update });
        }
    }
    if let Some(owner) = body_owner {
        requests.extend(medical::body_lock_requests(ex, owner).await?);
    }
    let locks = crate::services::inventory::destination::lock_items(ex, sde, &requests, &[])
        .await.map_err(|error| match error {
            CallError::User { ref msg, .. } if msg == infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION => stale(),
            error => error,
        })?;
    // Post-fence revalidation: the closure and reservation facts observed before
    // any item lock must still hold, while reservation rows completed
    // legitimately in between may be gone. Everything else is drift.
    let current_reservations = crate::dao::items::pending_reloads::intersecting(ex, &closure).await.map_err(db_err)?;
    if crate::dao::items::inventory::descendants(ex, &[ship]).await.map_err(db_err)? != closure
        || captured.iter().any(|old| !locks.row(old.item_id).is_some_and(|row|
            row.type_id == old.type_id && row.owner_id == old.owner_id && row.location_id == old.location_id
                && row.flag_id == old.flag_id && row.quantity == old.quantity && row.singleton == old.singleton
                && row.jump_clone_id == old.jump_clone_id))
        || current_reservations.iter().any(|row| !reservations.contains(row)) {
        return Err(stale());
    }
    // A captured physical source's ancestry must survive the fence exactly: a
    // changed or vanished source, and an initially absent source that
    // appeared, are both snapshot drift. A still-empty absence fact is valid.
    for (source, ancestry) in &source_lineages {
        if crate::dao::items::deliveries::ancestry(ex, *source).await.map_err(db_err)? != *ancestry {
            return Err(stale());
        }
    }
    crate::dao::combat::lock_related(ex, ship, &closure, &locks).await.map_err(db_err)?;
    let fenced = crate::dao::items::pending_reloads::intersecting(ex, &closure).await.map_err(db_err)?;
    if fenced.iter().any(|row| !reservations.contains(row)) { return Err(stale()); }
    // An external surviving weapon hull is never locked: its reservation is
    // destroyed with the closure it sources. A physical external source is
    // covered by its captured write lock or by its revalidated absence fact.
    for row in &fenced {
        if closure.contains(&row.ship_id) && !locks.is_write_locked(row.ship_id) {
            return Err(stale());
        }
        if closure.contains(&row.source_location_id)
            || crate::services::items::is_static_location(sde, row.source_location_id) {
            continue;
        }
        let captured_absent = source_lineages.iter()
            .find(|entry| entry.0 == row.source_location_id)
            .is_some_and(|(_, ancestry)| ancestry.is_empty());
        if !captured_absent && !locks.is_write_locked(row.source_location_id) {
            return Err(stale());
        }
    }
    Ok(Some(locks))
}

pub(crate) async fn destroy_ship_in_transaction<X: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, identity: &Identity,
    sde: &Arc<Sde>, input: &LossInput, fences: Option<&OnlineFences>,
    loaded: &[crate::dao::items::charges::LoadedWrite],
) -> Result<Option<LossOutcome>, CallError> {
    let (Some(ship), Some(system)) = (identity.ship_id, identity.system_id) else { return Ok(None) };
    if identity.char_id <= 0 || input.victim.character_id != Some(identity.char_id) || input.victim.item_id != ship {
        return Err(CallError::Internal(anyhow::anyhow!("loss victim identity mismatch")));
    }
    selection::lock_character(ex, identity.char_id).await.map_err(db_err)?;
    // The type read precedes lock-set formation so a capsule-group hull can
    // merge the owner's medical requests; the locked row is rechecked against
    // it and against the victim snapshot the caller fenced for.
    let Some(ship_type) = crate::dao::items::inventory::type_id(ex, ship).await.map_err(db_err)? else { return Ok(None) };
    let capsule = medical::is_capsule(sde, ship_type);
    if capsule && fences.is_none() {
        return Err(CallError::Internal(anyhow::anyhow!("capsule loss missing medical fences")));
    }
    if (!capsule && fences.is_some()) || ship_type != input.victim.type_id {
        return Err(db_err(crate::db::Error::StaleSnapshot("loss snapshot changed")));
    }
    let Some(locks) = lock_loss_items(ex, sde, ship, capsule.then_some(identity.char_id)).await? else { return Ok(None) };
    if !locks.row(ship).is_some_and(|row| row.type_id == ship_type) {
        return Err(db_err(crate::db::Error::StaleSnapshot("loss snapshot changed")));
    }
    if !crate::dao::combat::victim_current(ex, identity.char_id, ship, system).await.map_err(db_err)? { return Ok(None); }
    if crate::dao::killmails::victim_exists(ex, ship).await.map_err(db_err)? { return Ok(None); }
    // The victim's own loaded absolute commits before the loss reads, values
    // and deletes the charge rows; a prior HP checkpoint or an already
    // committed fact frame never skips it.
    crate::services::combat::loaded::settle_in(ex, loaded).await?;
    let source = facts::freeze_source(ex, &input.pending, crate::dao::repairable_ship::SourceKind::PilotedShip,
        ship, identity.char_id, ship_type, system, input.lethal_at, input.position).await?;
    // A non-capsule host may carry one assembled frigate in its escape bay:
    // the unique direct bay child, owned by the pilot, unoccupied and accepted
    // by the shared hold model on a host with positive effective capacity,
    // survives the loss under its own item ID and subtree. Uniqueness is
    // decided before eligibility; zero, multiple or ineligible candidates keep
    // the capsule outcome. The one capacity evaluation runs after the shared
    // preparation and root guards, so it takes the piloted-fit branch.
    let mut survivor = None;
    if !capsule {
        let mut bay = locks.rows().iter().filter(|row| row.location_id == ship && row.flag_id == FLAG_FRIGATE_ESCAPE_BAY && row.jump_clone_id.is_none());
        let candidate = match (bay.next(), bay.next()) { (Some(row), None) => Some(row), _ => None };
        if let Some(candidate) = candidate {
            let eligible = candidate.owner_id == identity.char_id
                && candidate.singleton && candidate.quantity == 1
                && holds::accepts(sde, FLAG_FRIGATE_ESCAPE_BAY, candidate.type_id)
                && boarding::unselected(ex, candidate.item_id).await.map_err(db_err)?
                && matches!(hold_storage::effective(ex, sde, identity.char_id, ship, FLAG_FRIGATE_ESCAPE_BAY).await?,
                    CapacityLimit::Finite(room) if room > 0.0);
            if eligible {
                // A validated survivor that refuses its guarded relocation
                // fails the whole loss; it never falls back to destruction or
                // a spawned capsule.
                if !item_dao::extract_escape_ship(ex, &locks, ship, candidate.item_id, identity.char_id, candidate.type_id, system, input.position).await.map_err(db_err)? {
                    return Err(CallError::Internal(anyhow::anyhow!("escape-bay survivor relocation refused")));
                }
                let item = load_items(ex, sde, ItemQuery::Id(candidate.item_id)).await.map_err(db_err)?
                    .into_iter().next().ok_or_else(|| CallError::Internal(anyhow::anyhow!("escape-bay survivor unavailable")))?;
                outbox.notify_pilot(identity.char_id, "OnItemChange", vec![
                    item.row(),
                    notifications::item_change_dict([
                        (IX_LOCATION_ID, Value::Int(ship)),
                        (IX_FLAG, Value::Int(i64::from(FLAG_FRIGATE_ESCAPE_BAY))),
                    ]),
                    Value::None,
                ]);
                survivor = Some(item.item_id);
            }
        }
    }
    // Internal modifier items (the hidden tactical mode) are never drops or
    // loss value: they leave the killmail, valuation and recovery manifests.
    let mut lost: super::super::crimewatch::Lost = item_dao::lost_items(ex, ship).await.map_err(db_err)?
        .iter().map(|r| (r.type_id, r.flag_id, r.singleton, if r.singleton { 1 } else { r.quantity }))
        .filter(|&(type_id, flag_id, ..)| !(flag_id == FLAG_HIDDEN_MODIFIERS
            && crate::sde::ship_stances::stance_for_mode(ship_type, type_id).is_some()))
        .collect();
    for r in item_dao::lost_charges(ex, ship).await.map_err(db_err)? {
        lost.push((r.type_id, r.flag_id, false, r.quantity));
    }
    let now = input.lethal_at;
    // A piloted capsule death first resolves the home station and deletes the
    // unprotected active augmentation rows; only rows the guarded delete
    // actually removed join the killed manifest, so killmail, insurance,
    // valuation and events below still run against the actual loss.
    let mut body = None;
    let home = if capsule {
        let home = super::super::character::home_station::home(ex, sde, identity.char_id).await?;
        body = Some(super::super::skills::body_change::begin_loss_tx(ex, outbox, sde, identity.char_id, now, |item| {
            lost.push((item.type_id, item.flag_id, true, 1));
        }).await?);
        Some(home)
    } else { None };
    let capture = crate::services::repairable_ship::recovery::capture_loss_in(
        ex, outbox, sde, &locks, &source, input.pending.recovery_seed).await?;
    let (killmail_id, _, prices, observed_at) = super::super::crimewatch::ship_lost_in(ex, outbox, &sde.prices, system, identity.char_id, identity.corp_id, ship, ship_type, &lost, &input.source, now, &input.contributions).await?;
    let payout = super::super::insurance::ship_destroyed_in(ex, outbox, sde, ship, now).await?;
    let valuation = facts::valuation(ship_type, &lost, &prices, observed_at, payout)?;
    let event = facts::events(outbox, ship, killmail_id, input.victim, input.context, now, input.conflict, &input.source, &input.contributions, valuation);

    item_dao::delete_lost(ex, ship, &capture.retained).await.map_err(db_err)?;
    crate::services::repairable_ship::recovery::finish_custody_in(ex, outbox, &locks, &capture).await?;
    let mut refresh = None;
    let replacement_ship = if let Some(survivor) = survivor {
        selection::set_ship(ex, identity.char_id, survivor).await.map_err(db_err)?;
        survivor
    } else if let (Some(home), Some(loss)) = (&home, body) {
        let (capsule, body_refresh) = medical::rebirth_tx(ex, outbox, sde,
            fences.ok_or_else(|| CallError::Internal(anyhow::anyhow!("capsule loss missing medical fences")))?,
            identity, home, now, loss).await?;
        refresh = Some(body_refresh);
        capsule
    } else {
        let capsule = creation::positioned(ex, TYPE_CAPSULE, identity.char_id, system, input.position).await.map_err(db_err)?;
        selection::set_ship(ex, identity.char_id, capsule).await.map_err(db_err)?;
        capsule
    };
    let home = home.map(|home| (home.station_id, home.solar_system_id, home.constellation_id, home.region_id));
    outbox.event_from(SourceKey::new("ShipLost", ship), GameEvent::ShipLost { char_id: identity.char_id, ship_id: ship, type_id: ship_type, system_id: system });
    let outcome = LossOutcome { replacement_ship, home, position: input.position };
    *crate::poison::lock_or_exit(input.pending.prepared.lock()) = Some(facts::PreparedLoss { event, outcome: outcome.clone(), body: refresh });
    Ok(Some(outcome))
}

#[cfg(all(test, feature = "tests-ships"))] mod tests;
