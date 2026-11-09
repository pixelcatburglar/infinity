//! Destroys a player ship and creates its capsule in one database transaction.
//! The transaction has no client RPC or runtime mutation: game.items,
//! game.item_attributes, killmails, insurance and characters are guarded or
//! updated through their services, then ShipLost is handed to the post-commit
//! runtime dispatcher. Client-visible work is limited to replacing the
//! existing session and observing the resulting space membership; no new body
//! or implant loss extension is supported.
use infinity_core::consts::{FLAG_HANGAR, TYPE_CAPSULE};
use crate::dao::{characters::{selection, ship as character_dao}, items::{creation, ship as item_dao}};
use crate::db::{executor::Executor, outbox::{Outbox, SourceKey}};
use crate::lua::events::GameEvent;
use crate::services::{combat::damage::DamageSource, db_err, identity::Identity, CallError};
use std::sync::Arc;
use crate::{db::Db, sde::Sde, space::World};
pub(crate) mod facts;

/// Presence preparation releases its market guard before online registration.
/// Registration can therefore occur between the actor-turn check and mutation.
pub(crate) async fn publish_loss(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>,
    outcome: LossOutcome, token: u64, generation: u64, expected: Option<infinity_core::session::OnlineSession>) -> Result<bool, CallError> {
    let ship = identity.ship_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing lost ship")))?;
    let system = identity.system_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing loss system")))?;
    let character = identity.char_id;
    let mut next = expected.clone();
    if let Some(session) = &mut next {
        session.ship_id = Some(outcome.capsule);
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
            expected.as_ref().and_then(|s| s.alliance_id), outcome.capsule, outcome.position).await?)
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
        if s.destroy_ball(ship).is_none() { return false; }
        s.finalize_dying(ship, token, generation);
        if let Some(prepared) = prepared { prepared.install(s, |s, ball| s.add_ship(ball)); }
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
    pub capsule: i64,
    pub home: Option<(i64, i64, i64, i64)>,
    pub position: [f64; 3],
}

pub(crate) async fn lock_loss_items<'tx>(
    ex: &'tx (impl Executor + ?Sized), sde: &crate::sde::Sde, ship: i64,
) -> Result<Option<crate::dao::items::parent_locks::LockedItems<'tx>>, CallError> {
    use crate::dao::items::parent_locks::{LockRequest, LockStrength};
    let children = crate::dao::combat::item_candidates(ex, ship).await.map_err(db_err)?;
    let mut ids = children.clone();
    ids.push(ship);
    let captured = crate::dao::items::parent_locks::capture(ex, &ids).await.map_err(db_err)?;
    if !captured.iter().any(|row| row.item_id == ship) { return Ok(None); }
    if captured.len() != ids.len() {
        return Err(crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    }
    let requests: Vec<_> = ids.into_iter().map(|item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
    let locks = crate::services::inventory::destination::lock_items(ex, sde, &requests, &[]).await?;
    if captured.iter().any(|old| !locks.row(old.item_id).is_some_and(|row|
        row.type_id == old.type_id && row.owner_id == old.owner_id && row.location_id == old.location_id
            && row.flag_id == old.flag_id && row.quantity == old.quantity && row.singleton == old.singleton
            && row.jump_clone_id == old.jump_clone_id))
        || crate::dao::combat::item_candidates(ex, ship).await.map_err(db_err)? != children {
        return Err(crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    }
    crate::dao::combat::lock_related(ex, ship, &children, &locks).await.map_err(db_err)?;
    Ok(Some(locks))
}

pub(crate) async fn destroy_ship_in_transaction<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, identity: &Identity,
    sde: &crate::sde::Sde, input: &LossInput,
) -> Result<Option<LossOutcome>, CallError> {
    let (Some(ship), Some(system)) = (identity.ship_id, identity.system_id) else { return Ok(None) };
    if identity.char_id <= 0 || input.victim.character_id != Some(identity.char_id) || input.victim.item_id != ship {
        return Err(CallError::Internal(anyhow::anyhow!("loss victim identity mismatch")));
    }
    selection::lock_character(ex, identity.char_id).await.map_err(db_err)?;
    let Some(_locks) = lock_loss_items(ex, sde, ship).await? else { return Ok(None) };
    if !crate::dao::combat::victim_current(ex, identity.char_id, ship, system).await.map_err(db_err)? { return Ok(None); }
    if crate::dao::killmails::victim_exists(ex, ship).await.map_err(db_err)? { return Ok(None); }
    let Some(ship_type) = crate::dao::items::inventory::type_id(ex, ship).await.map_err(db_err)? else { return Ok(None) };
    let mut lost: super::super::crimewatch::Lost = item_dao::lost_items(ex, ship).await.map_err(db_err)?
        .iter().map(|r| (r.type_id, r.flag_id, r.singleton, if r.singleton { 1 } else { r.quantity })).collect();
    for r in item_dao::lost_charges(ex, ship).await.map_err(db_err)? {
        lost.push((r.type_id, r.flag_id, false, r.quantity));
    }
    let now = input.lethal_at;
    let (killmail_id, _, prices, observed_at) = super::super::crimewatch::ship_lost_in(ex, outbox, system, identity.char_id, identity.corp_id, ship, ship_type, &lost, &input.source, now, &input.contributions).await?;
    let payout = super::super::insurance::ship_destroyed_in(ex, outbox, sde, ship, now).await?;
    let valuation = facts::valuation(ship_type, &lost, &prices, observed_at, payout)?;
    let event = facts::events(outbox, ship, killmail_id, input.victim, input.context, now, input.conflict, &input.source, &input.contributions, valuation);
    let home = if ship_type == TYPE_CAPSULE {
        let r = super::super::character::home_station::home(ex, sde, identity.char_id).await?;
        Some((r.station_id, r.solar_system_id, r.constellation_id, r.region_id))
    } else { None };
    for reload in crate::dao::items::pending_reloads::for_ship(ex, ship).await.map_err(db_err)? {
        crate::dao::items::pending_reloads::take(ex, reload.reload_id).await.map_err(db_err)?;
    }
    item_dao::delete_lost(ex, ship).await.map_err(db_err)?;
    let capsule = if let Some((station, _, _, _)) = home {
        creation::rookie_ship(ex, TYPE_CAPSULE, identity.char_id, station, FLAG_HANGAR).await.map_err(db_err)?
    } else {
        creation::positioned(ex, TYPE_CAPSULE, identity.char_id, system, input.position).await.map_err(db_err)?
    };
    if let Some((station, system, constellation, region)) = home {
        character_dao::set_home(ex, identity.char_id, capsule, station, system, constellation, region).await.map_err(db_err)?;
    } else {
        selection::set_ship(ex, identity.char_id, capsule).await.map_err(db_err)?;
    }
    outbox.event_from(SourceKey::new("ShipLost", ship), GameEvent::ShipLost { char_id: identity.char_id, ship_id: ship, type_id: ship_type, system_id: system });
    let outcome = LossOutcome { capsule, home, position: input.position };
    *input.pending.prepared.lock().map_err(|_| CallError::Internal(anyhow::anyhow!("loss receipt lock poisoned")))? = Some(facts::PreparedLoss { event, outcome: outcome.clone() });
    Ok(Some(outcome))
}

#[cfg(test)] mod tests;
