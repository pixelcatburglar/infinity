//! Source image and one attempt's actual postimage for a foreign Board.
//! Recovery compares these rows; this module never moves custody.
//! `revalidate` compares the complete captured state, not the receipt batch:
//! every source row, insurance policy, pending reservation and refund
//! destination/candidate row. An empty batch therefore proves nothing by
//! itself; only an unchanged captured state makes an attempt Aborted, and any
//! difference stays Unknown.

use crate::dao::items::parent_locks;
use crate::dao::items::pending_reloads::{self, PendingReload};
use crate::db::{self, executor::Executor};
use std::sync::Arc;

#[derive(Clone, PartialEq, Eq)]
struct Member {
    item_id: i64,
    type_id: i32,
    owner_id: i64,
    location_id: i64,
    flag_id: i32,
    quantity: i64,
    singleton: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct Policy {
    ship_id: i64,
    owner_id: i64,
    type_id: i32,
    fraction_bits: u64,
    start_date: i64,
    end_date: i64,
}

/// One stack a reload refund wrote in this attempt: the row it left and, when
/// it merged into an existing stack, that stack's quantity before the deposit.
/// A `None` old quantity marks a row this attempt inserted.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RefundRow {
    pub item_id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i32,
    pub quantity: i64,
    pub old_quantity: Option<i64>,
}

/// One physical stack a reload refund touched, folded in receipt order: the
/// quantity before this attempt first deposited and the row its last receipt
/// left. Repeated receipts for the same id are one stack, not several.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Refund {
    item_id: i64,
    old_quantity: Option<i64>,
    last: Member,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BoardOwnershipProof {
    root: i64,
    source: Vec<Member>,
    policies: Vec<Policy>,
    reservations: Vec<PendingReload>,
    destinations: Vec<Member>,
    postimage: Vec<Member>,
    post_destinations: Vec<Member>,
    post_policies: Vec<Policy>,
    post_reservations: Vec<PendingReload>,
    /// The refund stacks this attempt actually wrote: merged destinations and
    /// inserted rows, external ones included, folded by physical id. Proof
    /// only; never custody, and external rows are never seized.
    refunds: Vec<Refund>,
    created: Vec<i64>,
    sealed: bool,
}

impl BoardOwnershipProof {
    pub(crate) fn source_ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.source.iter().map(|row| row.item_id)
    }

    /// Source rows, the pre-existing rows this attempt refunds into, and the
    /// rows it created: the complete locked evidence for one Board attempt.
    pub(crate) fn evidence_ids(&self) -> Vec<i64> {
        let mut ids: Vec<_> = self.source_ids()
            .chain(self.destinations.iter().map(|row| row.item_id))
            .chain(self.created.iter().copied()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    pub(crate) fn created_ids(&self) -> &[i64] { &self.created }

    pub(crate) fn source_owner(&self, id: i64) -> Option<i64> {
        self.source.binary_search_by_key(&id, |row| row.item_id).ok().map(|index| self.source[index].owner_id)
    }

    /// `refund_ids` are the rows the caller's captured refund candidates and
    /// their mandatory ancestry name, read before any mutation.
    pub(crate) async fn capture(ex: &(impl Executor + ?Sized), root: i64, refund_ids: &[i64]) -> Result<Arc<Self>, db::Error> {
        let source = members(ex, &closure(ex, root).await?).await?;
        let ids: Vec<_> = source.iter().map(|row| row.item_id).collect();
        let reservations = pending(ex, &ids).await?;
        let mut destination_ids = refund_ids.to_vec();
        destination_ids.extend(ordinary_stack_ids(ex, &reservations).await?);
        destination_ids.sort_unstable();
        destination_ids.dedup();
        Ok(Arc::new(Self {
            root, source, policies: policies(ex, &ids).await?, reservations,
            destinations: members(ex, &destination_ids).await?,
            postimage: Vec::new(), post_destinations: Vec::new(), post_policies: Vec::new(), post_reservations: Vec::new(),
            refunds: Vec::new(), created: Vec::new(), sealed: false,
        }))
    }

    /// Locked rows are still exactly the captured source and refund image.
    pub(crate) async fn revalidate(&self, ex: &(impl Executor + ?Sized)) -> Result<bool, db::Error> {
        let live = closure(ex, self.root).await?;
        let ids: Vec<_> = self.source_ids().collect();
        let destination_ids: Vec<_> = self.destinations.iter().map(|row| row.item_id).collect();
        Ok(live == ids && members(ex, &live).await? == self.source
            && policies(ex, &ids).await? == self.policies
            && pending(ex, &ids).await? == self.reservations
            && members(ex, &destination_ids).await? == self.destinations)
    }

    /// Record the refund stacks this attempt wrote. A failed attempt keeps the
    /// created ids that must be gone after rollback, so `committed` stays false.
    pub(crate) fn record_refunds(self: &Arc<Self>, refunds: &[RefundRow]) -> Arc<Self> {
        let folded = fold_refunds(refunds);
        Arc::new(Self { created: refund_created(&folded), refunds: folded, ..(**self).clone() })
    }

    /// Read the closure this attempt actually left, including created rows.
    pub(crate) async fn seal(self: &Arc<Self>, ex: &(impl Executor + ?Sized), refunds: &[RefundRow]) -> Result<Arc<Self>, db::Error> {
        let live = closure(ex, self.root).await?;
        let folded = fold_refunds(refunds);
        let created = refund_created(&folded);
        Ok(Arc::new(Self {
            postimage: members(ex, &live).await?,
            post_destinations: members(ex, &self.destinations.iter().map(|row| row.item_id).collect::<Vec<_>>()).await?,
            post_policies: policies(ex, &live).await?,
            post_reservations: pending(ex, &self.source_ids().collect::<Vec<_>>()).await?,
            refunds: folded, created, sealed: true,
            ..(**self).clone()
        }))
    }

    pub(crate) async fn committed(&self, ex: &(impl Executor + ?Sized), character: i64, target: i64) -> Result<bool, db::Error> {
        if !self.sealed || crate::dao::characters::selection::ship_id(ex, character).await? != Some(target) { return Ok(false); }
        let live = closure(ex, self.root).await?;
        let ids: Vec<_> = self.postimage.iter().map(|row| row.item_id).collect();
        // A refund stack inside the taken closure changes owner with it, so the
        // sealed postimage row is its final expectation; a stack outside keeps
        // exactly the row its last receipt left.
        let mut external = Vec::new();
        for row in &self.refunds {
            match self.postimage.binary_search_by_key(&row.item_id, |member| member.item_id) {
                // Custody rewrote the owner of an acquired stack after the
                // receipt; every other final fact must match exactly.
                Ok(index) => {
                    let member = &self.postimage[index];
                    if member.type_id != row.last.type_id || member.location_id != row.last.location_id
                        || member.flag_id != row.last.flag_id || member.quantity != row.last.quantity {
                        return Ok(false);
                    }
                }
                Err(_) => external.push(row.last.clone()),
            }
        }
        let external_ids: Vec<_> = external.iter().map(|row| row.item_id).collect();
        let destination_ids: Vec<_> = self.post_destinations.iter().map(|row| row.item_id).collect();
        Ok(live == ids && members(ex, &live).await? == self.postimage
            && policies(ex, &ids).await? == self.post_policies
            && pending(ex, &self.source_ids().collect::<Vec<_>>()).await? == self.post_reservations
            && members(ex, &external_ids).await? == external
            && members(ex, &destination_ids).await? == self.post_destinations)
    }

    /// Unchanged source image. Created ids must be gone from the whole item table.
    pub(crate) async fn aborted(&self, ex: &(impl Executor + ?Sized)) -> Result<bool, db::Error> {
        Ok(self.revalidate(ex).await? && parent_locks::capture(ex, &self.created).await?.is_empty())
    }
}

async fn closure(ex: &(impl Executor + ?Sized), root: i64) -> Result<Vec<i64>, db::Error> {
    crate::dao::items::inventory::descendants(ex, &[root]).await
}

/// Fold receipt rows by physical stack, keeping the first original quantity
/// and the last actual row in receipt order, in ascending id order.
fn fold_refunds(rows: &[RefundRow]) -> Vec<Refund> {
    let mut folded: std::collections::BTreeMap<i64, Refund> = std::collections::BTreeMap::new();
    for row in rows {
        let member = Member {
            item_id: row.item_id, type_id: row.type_id, owner_id: row.owner_id, location_id: row.location_id,
            flag_id: row.flag_id, quantity: row.quantity, singleton: false,
        };
        folded.entry(row.item_id)
            .and_modify(|entry| entry.last = member.clone())
            .or_insert(Refund { item_id: row.item_id, old_quantity: row.old_quantity, last: member });
    }
    folded.into_values().collect()
}

/// Ids this attempt inserted. They must be gone again before rollback can count.
fn refund_created(rows: &[Refund]) -> Vec<i64> {
    rows.iter().filter(|row| row.old_quantity.is_none()).map(|row| row.item_id).collect()
}
async fn ordinary_stack_ids(ex: &(impl Executor + ?Sized), reservations: &[PendingReload]) -> Result<Vec<i64>, db::Error> {
    let groups: std::collections::BTreeSet<_> = reservations.iter()
        .map(|row| (row.owner_id, row.source_location_id, row.source_flag, row.type_id)).collect();
    let mut ids = Vec::new();
    for (owner, location, flag, type_id) in groups {
        ids.extend(ex.query(
            "SELECT item_id FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4 AND NOT singleton AND jump_clone_id IS NULL AND quantity >= 0 ORDER BY item_id",
            &[&owner, &location, &flag, &type_id],
        ).await?.iter().map(|found| found.get::<_, i64>("item_id")));
    }
    Ok(ids)
}

async fn members(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<Member>, db::Error> {
    Ok(parent_locks::capture(ex, ids).await?.into_iter().filter(|row| row.jump_clone_id.is_none()).map(|row| Member {
        item_id: row.item_id, type_id: row.type_id, owner_id: row.owner_id, location_id: row.location_id,
        flag_id: row.flag_id, quantity: row.quantity, singleton: row.singleton,
    }).collect())
}

async fn policies(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<Policy>, db::Error> {
    Ok(ex.query(
        "SELECT ship_id, owner_id, type_id, fraction, start_date, end_date FROM game.insurance WHERE ship_id = ANY($1) ORDER BY ship_id",
        &[&ids],
    ).await?.iter().map(|row| Policy {
        ship_id: row.get("ship_id"), owner_id: row.get("owner_id"), type_id: row.get("type_id"),
        fraction_bits: row.get::<_, f64>("fraction").to_bits(), start_date: row.get("start_date"), end_date: row.get("end_date"),
    }).collect())
}

async fn pending(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<PendingReload>, db::Error> {
    pending_reloads::intersecting(ex, ids).await
}
