//! One prepared take of a foreign ship's active subtree. `move_core` is the only
//! reload settlement; an owned hull never enters here.

use crate::dao::items::boarding::ownership::{BoardOwnershipProof, RefundRow};
use crate::dao::items::loading::ItemQuery;
use crate::dao::items::parent_locks::{LockRequest, LockStrength};
use crate::db::{executor::Executor, Db};
use crate::db::outbox::Outbox;
use crate::sde::Sde;
use crate::services::combat::reload_recovery::TakePreparation;
use crate::services::inventory::destination::{self, Custody, Destination};
use crate::services::inventory::movement;
use crate::services::inventory::notifications::Audience;
use crate::services::{db_err, notify_error, CallError};
use infinity_core::messages;
use std::sync::Arc;

pub(super) struct ForeignTake {
    proof: Arc<BoardOwnershipProof>,
    pub sealed: Arc<std::sync::Mutex<Option<Arc<BoardOwnershipProof>>>>,
    root: i64,
    preparation: TakePreparation,
    audience: Audience,
    ancestry: Vec<crate::dao::items::deliveries::Ancestor>,
}

impl ForeignTake {
    pub(super) async fn capture(
        db: &Db, sde: &Sde, identity: &crate::services::identity::Identity,
        world: &crate::space::World, system: i64, target: i64,
    ) -> Result<Self, CallError> {
        let preparation = TakePreparation::capture(db, sde, target).await?;
        let mut refund_ids = preparation.refund().candidate_ids();
        refund_ids.extend(preparation.refund().mandatory_ids());
        refund_ids.extend(preparation.pending().iter().map(|row| row.source_location_id));
        let proof = BoardOwnershipProof::capture(db, target, &refund_ids).await.map_err(db_err)?;
        let mut endpoints = preparation.endpoints().to_vec();
        endpoints.push(crate::services::inventory::reachability::Endpoint { location: target, flag: 0 });
        let audience = Audience::capture(identity, db, world, Some(system), &endpoints).await?;
        let ancestry = crate::dao::items::deliveries::ancestry(db, target).await.map_err(db_err)?;
        let sealed = Arc::new(std::sync::Mutex::new(Some(Arc::clone(&proof))));
        Ok(Self { proof, sealed, root: target, preparation, audience, ancestry })
    }

    pub(super) fn fresh(&self) -> Self {
        Self {
            proof: Arc::clone(&self.proof), sealed: Arc::clone(&self.sealed), root: self.root,
            preparation: self.preparation.fresh_attempt(), audience: self.audience.clone(), ancestry: self.ancestry.clone(),
        }
    }

    pub(super) async fn transfer<X: Send>(
        &mut self, tx: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>,
        actor: i64, system: i64, old: i64, states: &[crate::space::combat::ActorConditionSnapshot],
        facts: Option<&crate::space::snapshot::FactCheckpoint>,
    ) -> Result<Vec<i64>, CallError> {
        let refuse = || notify_error(messages::fitting::BOARD_UNAVAILABLE);
        self.publish(Arc::clone(&self.proof));
        crate::dao::characters::selection::lock_character(tx, actor).await.map_err(db_err)?;
        let closure = crate::dao::items::inventory::descendants(tx, &[self.root]).await.map_err(db_err)?;
        if closure.as_slice() != self.preparation.closure() || protected(tx, sde, actor, &closure, false).await? { return Err(refuse()); }
        let mut ids = closure.clone();
        ids.extend(self.preparation.ancestors().iter().copied());
        ids.extend(self.ancestry.iter().map(|row| row.item_id));
        ids.push(old);
        ids.sort_unstable();
        ids.dedup();
        let captured = crate::dao::items::parent_locks::capture(tx, &ids).await.map_err(db_err)?;
        let mut requests = destination::parent_requests(sde, captured.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Update)?;
        requests.extend(self.preparation.refund().requests().iter().copied());
        let evidence = self.proof.evidence_ids();
        requests.extend(evidence.iter().copied().map(|item_id| LockRequest { item_id, strength: LockStrength::Update }));
        let mandatory = self.preparation.refund().mandatory_ids();
        let optional: Vec<_> = self.preparation.refund().candidate_ids().into_iter().filter(|id| !ids.contains(id) && !mandatory.contains(id) && !evidence.contains(id)).collect();
        let locks = destination::lock_items(tx, sde, &requests, &optional).await?;
        if self.source_ids().any(|id| !locks.is_write_locked(id)) { return Err(refuse()); }
        self.preparation.revalidate(tx).await?;
        self.preparation.refund().revalidate(tx, sde, &locks).await?;
        if !self.proof.revalidate(tx).await.map_err(db_err)? { return Err(refuse()); }
        if crate::dao::items::deliveries::ancestry(tx, self.root).await.map_err(db_err)? != self.ancestry { return Err(refuse()); }
        if !crate::dao::items::boarding::unselected(tx, self.root).await.map_err(db_err)? { return Err(refuse()); }
        let written = crate::space::system::boarding::transitions::checkpoint(tx, outbox, system, states, facts).await?;
        let root = load_root(tx, sde, self.root).await?;
        let resolved = self.audience.resolve(tx, sde).await?;
        let moved = movement::move_core(tx, sde, outbox, &root, &Destination::owned(actor, system, 0), None,
            Custody::TakeSubtree, &locks, Some(&mut self.preparation), Some(&resolved), actor).await;
        let refunds = refund_rows(self.preparation.take_receipts());
        match moved {
            Ok(Some(moved_root)) if moved_root == self.root => {}
            Ok(_) => { self.publish(self.proof.record_refunds(&refunds)); return Err(refuse()); }
            Err(error) => { self.publish(self.proof.record_refunds(&refunds)); return Err(error); }
        }
        let after = crate::dao::items::inventory::descendants(tx, &[self.root]).await.map_err(db_err)?;
        let created: Vec<_> = after.iter().copied().filter(|id| self.proof.source_owner(*id).is_none()).collect();
        if protected(tx, sde, actor, &created, true).await? { self.publish(self.proof.record_refunds(&refunds)); return Err(refuse()); }
        let changed: Vec<_> = self.source_ids().filter(|&id| self.proof.source_owner(id) != Some(actor)).collect();
        for holder in crate::dao::insurance::void_many(tx, &changed).await.map_err(db_err)? {
            outbox.notify_pilot(holder, "OnShipInsured", vec![]);
        }
        self.publish(self.proof.seal(tx, &refunds).await.map_err(db_err)?);
        Ok(written)
    }

    fn publish(&self, proof: Arc<BoardOwnershipProof>) {
        *self.sealed.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(proof);
    }

    fn source_ids(&self) -> impl Iterator<Item = i64> + '_ { self.proof.source_ids() }
}

/// The refund stacks the deposit authority actually returned for one attempt.
fn refund_rows(receipts: Vec<crate::services::items::DepositReceipt>) -> Vec<RefundRow> {
    receipts.iter().flat_map(|receipt| receipt.rows()).map(|row| RefundRow {
        item_id: row.item.item_id, type_id: row.item.type_id, owner_id: row.item.owner_id,
        location_id: row.item.location_id, flag_id: row.item.flag_id, quantity: row.item.quantity,
        old_quantity: row.old_quantity,
    }).collect()
}

async fn load_root(tx: &(impl Executor + ?Sized), sde: &Sde, id: i64) -> Result<crate::dao::items::loading::Item, CallError> {
    crate::services::items::load_items(tx, sde, ItemQuery::Id(id)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::fitting::BOARD_UNAVAILABLE))
}

async fn protected(tx: &(impl Executor + ?Sized), sde: &Sde, actor: i64, ids: &[i64], created: bool) -> Result<bool, CallError> {
    if ids.is_empty() { return Ok(false); }
    let rows = crate::services::items::load_items(tx, sde, ItemQuery::Ids(ids)).await.map_err(db_err)?;
    Ok(rows.iter().any(|row| !sde.is_tradable(row.type_id) && (created || row.owner_id != actor)))
}
