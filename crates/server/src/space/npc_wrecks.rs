//! World-side finite coordination for the actor-owned transient NPC wreck
//! state: deferred reserved-ID refills, the cancellation-safe claim guard and
//! the one exact commit proof for receipt-carrying claims. Workers never load
//! or restart an actor: results are posted back to the same system carrying
//! the original actor incarnation, so stale completions die at the mailbox.

use super::*;
use crate::dao::items::npc_loot;
use crate::services::CallError;
use crate::db::outbox::Outbox;
use crate::lua::events::GameEvent;
use crate::space::system::npc_wrecks::{NpcWreckClaim, NpcWreckClaimState, NpcWreckDisposition};
use std::sync::{Arc, Weak};

/// What the guard observed of the business transaction before being dropped.
/// `Fresh` attempts provably never executed; `Executing` may have committed;
/// `Committed` ran the post-commit dispatch entry.
enum Witness { Fresh, Executing, Committed, Released }

/// Cancellation-safe owner of one root claim. It is constructed inside the
/// claiming actor callback and returned with the lookup result, so a cancelled
/// RPC can never strand a claim whose oneshot receiver was already gone: Drop
/// posts exactly the witnessed disposition to the original system only.
pub(crate) struct NpcWreckGuard {
    world: Weak<World>,
    claim: Arc<NpcWreckClaim>,
    witness: Witness,
}

impl NpcWreckGuard {
    /// Actor-turn construction; the claim is already registered on the root.
    pub(crate) fn new(world: Weak<World>, claim: Arc<NpcWreckClaim>) -> Self {
        Self { world, claim, witness: Witness::Fresh }
    }
    pub(crate) fn claim(&self) -> &Arc<NpcWreckClaim> { &self.claim }
    /// The transaction may now execute; dropping after this posts Unknown.
    pub(crate) fn may_execute(&mut self) { self.witness = Witness::Executing; }
    /// The durable commit is proven; call immediately on dispatch entry so a
    /// later dispatch failure still finishes the actor delta, never rolls back.
    pub(crate) fn committed(&mut self) { self.witness = Witness::Committed; }
    /// Keep a never-executed WaitingIds plan parked in the actor. Only an
    /// unprepared claim may park; any other use consumes the guard and lets
    /// Drop settle it normally.
    pub(crate) fn park(mut self) {
        if !self.claim.prepared() && matches!(self.witness, Witness::Fresh) {
            self.witness = Witness::Released;
        }
    }
    /// Await the actor's settle: returns whether the disposition was applied
    /// (false when the original actor is gone or the claim already resolved).
    /// The witness stays armed until the actor confirms: a cancelled await
    /// lets Drop post the witnessed disposition again, which the idempotent
    /// claim state machine absorbs.
    pub(crate) async fn finish(mut self, outcome: NpcWreckDisposition) -> bool {
        let Some(world) = self.world.upgrade() else { return false };
        let claim = self.claim.clone();
        let applied = world.with(claim.system(), move |s| s.finish_npc_wreck(&claim, outcome)).await.unwrap_or(false);
        if applied { self.witness = Witness::Released; }
        applied
    }
}

impl Drop for NpcWreckGuard {
    fn drop(&mut self) {
        let outcome = match self.witness {
            Witness::Released => return,
            Witness::Fresh => NpcWreckDisposition::RolledBack,
            Witness::Executing => NpcWreckDisposition::Unknown,
            Witness::Committed => NpcWreckDisposition::Committed,
        };
        if let Some(world) = self.world.upgrade() {
            let claim = self.claim.clone();
            world.post(claim.system(), move |s| { s.finish_npc_wreck(&claim, outcome); });
        }
    }
}

/// Cancellation-safe delivery of one reserve attempt: whatever the worker
/// leaves undelivered posts back to the same actor incarnation and serial.
/// `None` is the shared failure/cancel outcome; it clears the in-flight
/// marker and arms the configured cooldown in `npc_wreck_refill_delivered`;
/// a stale delivery is ignored there.
struct RefillWitness {
    world: Weak<World>,
    system: i64,
    incarnation: Arc<()>,
    serial: u64,
    block: Option<Vec<i64>>,
}

impl Drop for RefillWitness {
    fn drop(&mut self) {
        let Some(world) = self.world.upgrade() else { return };
        let (system, incarnation, serial, block) = (self.system, self.incarnation.clone(), self.serial, self.block.take());
        world.post(system, move |s| s.npc_wreck_refill_delivered(incarnation, serial, block));
    }
}

/// One bounded deferred reserve attempt for the actor that armed `serial`.
/// The whole reserved block is only handed back after a committed transaction;
/// an error, unknown commit or cancelled worker burns it entirely and always
/// releases the actor's in-flight marker through the same delivery.
pub(crate) async fn refill_ids(world: Weak<World>, system: i64, incarnation: Arc<()>, serial: u64) {
    let mut witness = RefillWitness { world, system, incarnation, serial, block: None };
    let Some(world) = witness.world.upgrade() else { return };
    let block = crate::config::game().combat.npc_wreck_id_block_size;
    let result = world.db().transaction(
        move |tx, _: &mut Outbox<()>| Box::pin(async move {
            npc_loot::reserve_ids(tx, block).await.map_err(CallError::from)
        }),
        |ids, _| async move { Ok::<Vec<i64>, CallError>(ids) },
    ).await;
    if let Err(error) = &result {
        tracing::warn!(system, ?error, "npc wreck id reservation failed");
    }
    witness.block = result.ok();
}

/// Exact canonical proof shared by every consumer's first transaction: the
/// stored envelope must be the same source (namespace, key, schema, kind,
/// ordinal and subject) carrying the complete frozen event; resource events
/// additionally match occurred_at, context and origin, while plain events
/// prove the default context and origin. Any contradiction is unknown, not
/// a rollback.
pub(crate) fn receipt_matches(stored: &crate::progress::EventEnvelope, receipt: &crate::space::system::npc_wrecks::NpcWreckReceipt) -> Result<bool, CallError> {
    let expected = receipt.event();
    if stored.source_namespace != "business" || stored.source_key != receipt.source_key().as_str()
        || stored.schema_version != crate::progress::codec::SCHEMA_VERSION || stored.kind != expected.kind()
        || stored.ordinal != 0 || stored.event != *expected { return Ok(false); }
    let subject = match expected {
        GameEvent::ItemsMoved { char_id, .. } | GameEvent::WreckSalvaged { char_id, .. } => Some(*char_id),
        event => crate::lua::events::combat::subject(event),
    };
    if stored.subject_character_id != subject { return Ok(false); }
    if let Some((occurred_at, context)) = expected.resource_metadata().or_else(|| crate::lua::events::combat::metadata(expected)) {
        if stored.occurred_at != occurred_at { return Ok(false); }
        let actual: serde_json::Value = serde_json::from_str(&stored.context)
            .map_err(|error| CallError::Internal(error.into()))?;
        if actual != serde_json::to_value(context).map_err(|error| CallError::Internal(error.into()))? { return Ok(false); }
        return Ok(stored.origin == context.origin.as_str());
    }
    let context: serde_json::Value = serde_json::from_str(&stored.context)
        .map_err(|error| CallError::Internal(error.into()))?;
    Ok(context.as_object().is_some_and(|map| map.is_empty()) && stored.origin == "server")
}
impl World {
    /// One fenced proof for a receipt-carrying claim (Take/Salvage), gated
    /// twice by the actor's own claim state: only an already-sealed Unknown
    /// attempt may prove at all, and the frozen delta applies only while the
    /// registered slot is still that same Unknown claim. An active writer is
    /// always busy (never rolled back by a missing source) and the actor
    /// only finishes the stored delta; nothing replays the business write.
    pub(crate) async fn reconcile_npc_wreck(&self, claim: Arc<NpcWreckClaim>) -> Result<NpcWreckDisposition, CallError> {
        let Some(receipt) = claim.receipt() else {
            return Err(CallError::Internal(anyhow::anyhow!("npc wreck claim carries no receipt")));
        };
        // Hard gate before any fence or source read: Claimed/WaitingIds and
        // stale identities are busy without touching the database.
        let busy = || crate::services::notify_error(infinity_core::messages::fitting::CHARGES_SETTLING);
        let status = self.with(claim.system(), { let claim = claim.clone(); move |s| s.npc_wreck_claim_status(&claim) }).await;
        match status {
            Some(Some(NpcWreckClaimState::Unknown)) => {}
            Some(_) => return Err(busy()),
            None => return Err(CallError::Internal(anyhow::anyhow!("npc wreck actor unavailable"))),
        }
        let key = receipt.source_key().as_str().to_owned();
        let result = self.db().transaction(
            move |tx, _: &mut Outbox<()>| {
                let key = key.clone();
                Box::pin(async move {
                    npc_loot::fence(tx, &key).await.map_err(CallError::from)?;
                    crate::dao::progress::source(tx, "business", &key).await.map_err(CallError::from)
                })
            },
            |stored, _| async move { Ok::<_, CallError>(stored) },
        ).await;
        let disposition = match result {
            Ok(Some(stored)) => match receipt_matches(&stored, receipt) {
                Ok(true) => NpcWreckDisposition::Committed,
                Ok(false) | Err(_) => NpcWreckDisposition::Unknown,
            },
            Ok(None) => NpcWreckDisposition::RolledBack,
            // A read or fence failure changes nothing: the claim stays sealed
            // Unknown (never forced from a live state) and the error stands.
            Err(error) => return Err(error),
        };
        // Apply only while the registered slot is still the same Unknown
        // claim; a changed status or identity is busy, never applied.
        let applied = self.with(claim.system(), move |s| {
            if s.npc_wreck_claim_status(&claim) != Some(NpcWreckClaimState::Unknown) { return false; }
            s.finish_npc_wreck(&claim, disposition)
        }).await;
        match applied {
            Some(true) => Ok(disposition),
            Some(false) => Err(busy()),
            None => Err(CallError::Internal(anyhow::anyhow!("npc wreck actor unavailable"))),
        }
    }
}

#[cfg(all(test, feature = "tests-space"))]
mod tests;
