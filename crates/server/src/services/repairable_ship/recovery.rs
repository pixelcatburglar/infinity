//! `repairableShipMgr` recovery lifecycle RPCs: snapshot list, delivery
//! purchase, abort, withdrawal and destination lookup. Mirrors the calls in
//! `repairableShip/client/repairable_ship_svc.py` and the selection dicts built
//! by `repairable_ship_window.py` (`{groupID: [memberID]}`).
//! Writes run in one transaction under the market lock: wallet debit,
//! member status transitions, custody moves and scheduler jobs commit or roll
//! back together. Repeat calls replay the persisted state through the same
//! notifications without new writes, debits or sequences.
//! Billing uses the last service-life value sent to the client.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use infinity_core::consts::{FILETIME_DAY, FILETIME_SECOND, IX_LOCATION_ID, NPC_CORP_INTERBUS, REF_REHABILITATION_COSTS};
use infinity_core::messages::repairable_ship as msg;
use marshal::Value;

use crate::client::{filetime_now, Client};
use crate::dao::items::parent_locks::{self, LockRequest, LockStrength, LockedItems};
use crate::dao::items::repairable_ship as custody;
use crate::dao::repairable_ship::{
    self as dao, Group, ItemId, Payment, RecoveryGroupId, RecoveryItem, RecoveryItemId, Selection, Status,
};
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_sender, Outbox};
use crate::isk::IskAmount;
use crate::sde::Sde;
use crate::services::inventory::{destination::lock_items, notifications};
use crate::services::wallets::{journal, ownership::FinancialOwner};
use crate::services::{db_err, market, notify_error, Call, CallError, CallResult};
use crate::scheduler;

use super::{deadlines, wire};
/// Recovery facts prepared in the same transaction as the original loss.
pub(in crate::services) struct RecoveryCapture {
    pub retained: Vec<i64>,
    rows: Vec<dao::SourceShape>,
    selection: Option<Selection>,
}

/// Capture only the remaining loss closure; the lock token can also contain
/// medical rows, external reservation ancestry and escaped subtrees.
pub(in crate::services) async fn capture_loss_in<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, sde: &Sde,
    locks: &LockedItems<'_>, source: &dao::LossSource, seed: u64,
) -> Result<RecoveryCapture, CallError> {
    let stale = || db_err(crate::db::Error::StaleSnapshot("recovery loss snapshot changed"));
    let closure = crate::dao::items::inventory::descendants(ex, &[source.shape.item.get()]).await.map_err(db_err)?;
    if closure.iter().any(|id| !locks.is_write_locked(*id)) { return Err(stale()); }
    let ids: Vec<ItemId> = closure.iter().map(|id| ItemId::new(*id).ok_or_else(stale)).collect::<Result<_, _>>()?;
    let mut shapes = custody::capture_many(ex, &ids).await.map_err(db_err)?;
    if shapes.len() != ids.len() { return Err(stale()); }
    shapes.sort_unstable_by_key(|row| row.item);
    let lives = crate::dao::items::service_life::get_many(ex, &ids).await.map_err(db_err)?;
    let by_id: BTreeMap<_, _> = shapes.iter().map(|row| (row.item.get(), row)).collect();
    let mut retained = Vec::new();
    let mut rows = Vec::new();
    let mut staged = Vec::new();
    let mut changes = BTreeMap::new();
    for shape in &shapes {
        let mut cursor = shape;
        let mut remaining = shapes.len();
        let valid_path = loop {
            if cursor.owner != source.beneficiary || remaining == 0 { break false; }
            remaining -= 1;
            if cursor.item == source.shape.item { break true; }
            if !sde.repairable_ship.repair_flag(cursor.flag) { break false; }
            match by_id.get(&cursor.location) { Some(parent) => cursor = parent, None => break false }
        };
        if !valid_path || !shape.singleton || shape.quantity != 1 || shape.brain_slot.is_some() || shape.jump_clone.is_some() { continue; }
        let Some(ty) = sde.types.get(shape.type_id) else { return Err(stale()) };
        let (Some(group), Some(category)) = (ty.group_id, ty.category_id) else { return Err(stale()) };
        if !sde.repairable_ship.enters_recovery(shape.type_id, group, category)
            || sde.repairable_ship.unrepairable(category, group, shape.type_id) { continue; }
        let Some(seconds) = sde.repairable_ship.repair_seconds(shape.type_id) else { continue; };
        let max = sde.repairable_ship.max_life();
        let prior = lives.get(&shape.item).copied().unwrap_or(0.0);
        if !prior.is_finite() || prior < 0.0 || prior > max { return Err(stale()); }
        let damage = if shape.item == source.shape.item || sde.repairable_ship.reduce_flag(shape.flag) {
            super::service_life::loss_damage(seed, shape.item, prior, max, sde.repairable_ship.max_reduce())
        } else { prior };
        if damage >= max { continue; }
        let remaining = max - prior;
        if u32::try_from(shape.quantity).is_err() || !wire::last_service_life(remaining).is_finite() { return Err(stale()); }
        let ms = crate::space::seconds_to_ms_f64(seconds);
        if !ms.is_finite() || ms < 0.0 || ms >= crate::client::ms_from_filetime_ticks(i64::MAX) as f64 { return Err(stale()); }
        let repair_at = source.lethal_at.checked_add(crate::client::filetime_ticks_from_ms(ms)).ok_or_else(stale)?;
        if damage != prior { changes.insert(shape.item, damage); }
        retained.push(shape.item.get());
        rows.push(shape.clone());
        staged.push((shape, remaining, repair_at));
    }
    if staged.is_empty() { return Ok(RecoveryCapture { retained, rows, selection: None }); }
    let home = crate::services::character::home_station::home(ex, sde, source.beneficiary).await?;
    let duration = crate::config::game().repairable_ship.payment_retention_days.checked_mul(FILETIME_DAY).ok_or_else(stale)?;
    let deadline = source.lethal_at.checked_add(duration).ok_or_else(stale)?;
    for (&item, &damage) in &changes {
        if !crate::dao::items::service_life::set_if(ex, item, lives.get(&item).copied(), damage).await.map_err(db_err)? { return Err(stale()); }
    }
    let loss_id = (source.kind == dao::SourceKind::PilotedShip).then_some(source.shape.item.get());
    let group_id = dao::insert_group(ex, &dao::NewGroup { loss: source.clone(), loss_id,
        destination_station: home.station_id, payment_expires_at: deadline }).await.map_err(db_err)?;
    let members: Vec<_> = staged.iter().map(|(shape, remaining, repair_at)| dao::NewItem {
        group: group_id, item: shape.item, original_owner: shape.owner, type_id: shape.type_id,
        flag: shape.flag, quantity: shape.quantity, singleton: shape.singleton,
        last_service_life: *remaining, repair_completed_at: *repair_at,
    }).collect();
    if dao::insert_items(ex, &members).await.map_err(db_err)?.len() != members.len() { return Err(stale()); }
    let items = dao::members_of(ex, group_id).await.map_err(db_err)?;
    if items.len() != members.len() { return Err(stale()); }
    for item in &items { scheduler::schedule(ex, &deadlines::payment_job(item.id, deadline)).await.map_err(db_err)?; }
    let group = Group { id: group_id, beneficiary: source.beneficiary, kind: source.kind, shape: source.shape.clone(),
        system: source.system, occurred_at: source.lethal_at, position: source.sampled_position.map(Some),
        loss_id, destination_station: home.station_id, payment_expires_at: deadline };
    if !changes.is_empty() {
        outbox.notify_pilot(source.beneficiary, "OnItemServiceLifeDamageChanged", vec![super::service_life::damage_dict(&changes)]);
    }
    Ok(RecoveryCapture { retained, rows, selection: Some(Selection { group, items }) })
}

/// Complete the single guarded custody move after the loss cleanup.
pub(in crate::services) async fn finish_custody_in<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, locks: &LockedItems<'_>, capture: &RecoveryCapture,
) -> Result<(), CallError> {
    if let Some(selection) = &capture.selection {
        if !custody::move_to_custody(ex, locks, selection.group.id, &capture.rows).await.map_err(db_err)? {
            return Err(db_err(crate::db::Error::StaleSnapshot("recovery custody changed")));
        }
        let text = wire::exploded(selection.group.beneficiary, selection).map_err(CallError::Internal)?;
        outbox.notify_pilot(selection.group.beneficiary, "OnAddRecoveryGroup", vec![Value::str(text)]);
    }
    Ok(())
}

fn invalid() -> CallError {
    notify_error(msg::SELECTION_INVALID)
}

fn changed() -> CallError {
    notify_error(msg::STATE_CHANGED)
}

fn withdrawal_window(delivered: Option<i64>, expires: Option<i64>, now: i64) -> Result<(), CallError> {
    let delivered = delivered.ok_or_else(changed)?;
    let expires = expires.ok_or_else(changed)?;
    if now >= expires { return Err(notify_error(msg::WITHDRAWAL_EXPIRED)); }
    if now < delivered { return Err(notify_error(msg::NOT_DELIVERED)); }
    Ok(())
}

/// `{groupID: [memberID]}` from the client. Duplicate members inside a group
/// collapse; malformed values, out-of-band IDs and repeated group keys refuse
/// the whole batch before any state is read.
fn requested_groups(call: &Call<'_>) -> Result<BTreeMap<RecoveryGroupId, Vec<RecoveryItemId>>, CallError> {
    let mut groups: BTreeMap<RecoveryGroupId, Vec<RecoveryItemId>> = BTreeMap::new();
    let arg = call.args.first().ok_or_else(invalid)?;
    let Value::Dict(pairs) = arg else { return Err(invalid()) };
    for (key, value) in pairs {
        let group = key.as_int().and_then(RecoveryGroupId::new).ok_or_else(invalid)?;
        let members = value.items().ok_or_else(invalid)?;
        let mut ids = Vec::with_capacity(members.len());
        for value in members {
            let member = value.as_int().and_then(RecoveryItemId::new).ok_or_else(invalid)?;
            if !ids.contains(&member) {
                ids.push(member);
            }
        }
        if groups.insert(group, ids).is_some() {
            return Err(invalid());
        }
    }
    Ok(groups)
}

/// Every requested group and member must exist under the caller's ownership.
/// A member requested under a foreign group surfaces only under its true
/// group, so the lookup fails here.
fn verify_selection(
    selections: &[Selection], requested: &BTreeMap<RecoveryGroupId, Vec<RecoveryItemId>>,
) -> Result<(), CallError> {
    for (group, member_ids) in requested {
        let selection = selections.iter().find(|s| s.group.id == *group).ok_or_else(invalid)?;
        for member_id in member_ids {
            if !selection.items.iter().any(|member| member.id == *member_id) {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

fn wanted<'a>(selection: &'a Selection, requested: &BTreeMap<RecoveryGroupId, Vec<RecoveryItemId>>) -> Vec<&'a RecoveryItem> {
    let Some(wanted) = requested.get(&selection.group.id) else { return Vec::new() };
    selection.items.iter().filter(|member| wanted.contains(&member.id)).collect()
}

/// `CallRequested()`: one owned joined snapshot, then a legal
/// `OnRepairableShipResponsed` even when the snapshot is empty.
pub(super) async fn list(c: &mut Client) -> CallResult {
    let identity = crate::services::identity::Identity::of_client(c);
    let text = list_for(&c.db, identity).await?;
    c.notify("OnRepairableShipResponsed", vec![Value::str(text)]);
    Ok(Value::None)
}

async fn list_for(db: &crate::db::Db, identity: crate::services::identity::Identity) -> Result<String, CallError> {
    let mut selections = dao::list_owned(db, identity.char_id).await.map_err(db_err)?;
    let now = filetime_now();
    for selection in &mut selections {
        let payment_expiry = selection.group.payment_expires_at;
        selection.items.retain(|item| match item.status {
            Status::AwaitingPayment => now < payment_expiry,
            Status::Delivering | Status::Ready => item.withdraw_expires_at.is_some_and(|deadline| now < deadline),
            _ => false,
        });
    }
    selections.retain(|selection| !selection.items.is_empty());
    wire::responsed(identity.char_id, &selections).map_err(db_err)
}

fn delivery_item_cost(
    last_service_life: f64, life: f64, max_life: f64,
    unit_price: f64, quantity: i64, percentage: f64,
) -> f64 {
    let life_share = ((f64::from(wire::last_service_life(last_service_life)) - life) / max_life).abs();
    unit_price * quantity as f64 * percentage * life_share
}

/// Current server price of one unpaid member, mirroring
/// `repairableShip/common/methods.py:GetDeliveryItemCost`: the larger of SDE
/// base price and the stored retail average, scaled by quantity, the FSD
/// percentage and the share of service life already lost.
async fn price_payments(
    ex: &(impl Executor + ?Sized), sde: &Sde, members: &[&RecoveryItem],
) -> Result<Vec<Payment>, CallError> {
    if members.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<ItemId> = members.iter().map(|member| member.item).collect();
    let sources = custody::dynamic_sources(ex, &ids).await.map_err(db_err)?;
    let damages = crate::dao::items::service_life::get_many(ex, &ids).await.map_err(db_err)?;
    let config = &sde.repairable_ship;
    let max_life = config.max_life();
    let mut payments = Vec::with_capacity(members.len());
    for member in members {
        let source_type = sources.get(&member.item).copied().unwrap_or(member.type_id);
        let base = sde.types.get(source_type).and_then(|ty| ty.base_price).unwrap_or(0.0);
        let average = sde.prices.get(source_type).map(|price| price.average).unwrap_or(0.0);
        if !base.is_finite() || !average.is_finite() || base < 0.0 || average < 0.0 {
            return Err(CallError::Internal(anyhow::anyhow!("invalid recovery price for type {source_type}")));
        }
        let life = max_life - damages.get(&member.item).copied().unwrap_or(0.0);
        let cost = delivery_item_cost(
            member.last_service_life, life, max_life,
            base.max(average), member.quantity, config.price_percentage(),
        );
        if !cost.is_finite() || cost < 0.0 {
            return Err(CallError::Internal(anyhow::anyhow!(
                "recovery price for member {} is not a finite non-negative value",
                member.id.get(),
            )));
        }
        let amount = IskAmount::from_whole_isk(cost.trunc() as i128).ok_or_else(|| {
            CallError::Internal(anyhow::anyhow!("recovery price for member {} overflows", member.id.get()))
        })?;
        payments.push(Payment { member: member.id, amount });
    }
    Ok(payments)
}

/// `CallDeliveryPurchased({group: [member]})`: one debit, guarded status
/// transition and per-member delivery/withdrawal jobs in one transaction.
/// Already-paid members replay their persisted deadlines without a new charge.
pub(super) async fn purchase(c: &mut Client, call: &Call<'_>) -> CallResult {
    let identity = crate::services::identity::Identity::of_client(c);
    purchase_for(identity, &c.db, &c.world, &c.sde, &c.notify_tx, requested_groups(call)?).await?;
    Ok(Value::None)
}

async fn purchase_for(
    identity: crate::services::identity::Identity, db: &crate::db::Db,
    world: &crate::space::World, sde: &Arc<Sde>, sender: &crate::space::NotifyTx,
    requested: BTreeMap<RecoveryGroupId, Vec<RecoveryItemId>>,
) -> Result<(), CallError> {
    let me = identity.char_id;
    let _lock = market::lock().await;
    let sde = Arc::clone(sde);
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let requested = requested.clone();
            let sde = Arc::clone(&sde);
            Box::pin(async move {
                let selections = dao::load_selection(tx, me, &requested, true).await.map_err(db_err)?;
                verify_selection(&selections, &requested)?;
                let now = filetime_now();
                let mut payable: Vec<&RecoveryItem> = Vec::new();
                for selection in &selections {
                    for member in wanted(selection, &requested) {
                        match member.status {
                            Status::AwaitingPayment => {
                                if now >= selection.group.payment_expires_at {
                                    return Err(notify_error(msg::PAYMENT_EXPIRED));
                                }
                                payable.push(member);
                            }
                            Status::Delivering | Status::Ready => {}
                            _ => return Err(changed()),
                        }
                    }
                }
                if !payable.is_empty() {
                    let members: Vec<_> = payable.iter().map(|member| (**member).clone()).collect();
                    let (_locks, _) = lock_escrow(tx, &sde, &members).await?;
                    let ids: Vec<_> = members.iter().map(|member| member.item).collect();
                    let shapes = custody::capture_many(tx, &ids).await.map_err(db_err)?;
                    if shapes.len() != members.len() || members.iter().any(|member| {
                        let Some(shape) = shapes.iter().find(|shape| shape.item == member.item) else { return true };
                        shape.owner != NPC_CORP_INTERBUS || shape.location != member.group.get()
                            || shape.type_id != member.type_id || shape.flag != member.flag
                            || shape.quantity != member.quantity || shape.singleton != member.singleton
                            || shape.brain_slot.is_some() || shape.jump_clone.is_some()
                    }) { return Err(changed()); }
                }
                let payments = price_payments(tx, &sde, &payable).await?;
                let total = payments
                    .iter()
                    .try_fold(IskAmount::ZERO, |sum, payment| sum.checked_add(payment.amount))
                    .ok_or_else(|| CallError::Internal(anyhow::anyhow!("recovery payment total overflow")))?;
                if !payments.is_empty() {
                    let entry = crate::dao::wallets::journal::Entry {
                        actor_id: me,
                        entry_type_id: REF_REHABILITATION_COSTS,
                        owner_id1: me,
                        owner_id2: NPC_CORP_INTERBUS,
                        reference_id: selections.iter().map(|selection| selection.group.id.get()).min().unwrap_or(0),
                        created_at: now,
                        description: "",
                        market_transaction_id: None,
                        order_id: None,
                        side: 0,
                    };
                    journal::debit(tx, outbox, FinancialOwner::cash(me), total, &entry).await?;
                    let config = &crate::config::game().repairable_ship;
                    let delivery = config.delivery_seconds * FILETIME_SECOND;
                    let withdrawal = config.withdrawal_retention_days * FILETIME_DAY;
                    let moved = dao::start_delivery(tx, now, delivery, withdrawal, &payments).await.map_err(db_err)?;
                    if moved.len() != payments.len() {
                        return Err(changed());
                    }
                }
                // Jobs and the notification use the persisted deadlines, not
                // the formula recomputed here.
                let mut notified = Vec::with_capacity(selections.len());
                for selection in &selections {
                    let wanted_ids = &requested[&selection.group.id];
                    let fresh = dao::members_of(tx, selection.group.id).await.map_err(db_err)?;
                    let fresh: Vec<RecoveryItem> = fresh.into_iter().filter(|m| wanted_ids.contains(&m.id)).collect();
                    for member in &fresh {
                        if payable.iter().any(|paid| paid.id == member.id) {
                            scheduler::cancel(tx, &deadlines::job_id(deadlines::PAYMENT, member.id)).await.map_err(db_err)?;
                            let delivery_at = member.delivery_completed_at.ok_or_else(changed)?;
                            let withdraw_at = member.withdraw_expires_at.ok_or_else(changed)?;
                            scheduler::schedule(tx, &deadlines::delivery_job(member.id, delivery_at)).await.map_err(db_err)?;
                            scheduler::schedule(tx, &deadlines::withdraw_job(member.id, withdraw_at)).await.map_err(db_err)?;
                        }
                    }
                    notified.push(Selection { group: selection.group.clone(), items: fresh });
                }
                let text = wire::delivery_started(me, &notified).map_err(db_err)?;
                outbox.notify_client("OnDeliveryStarted", vec![Value::str(text)]);
                Ok(())
            })
        },
        |value, outbox| async move {
            let result = dispatch_sender(value, outbox, world, Some(sender));
            scheduler::wake();
            result
        },
    ).await
}

/// Every active member must still have a lockable physical custody row.
async fn lock_escrow<'tx>(
    ex: &'tx (impl Executor + ?Sized), sde: &Sde, members: &[RecoveryItem],
) -> Result<(LockedItems<'tx>, BTreeSet<i64>), CallError> {
    let ids: Vec<i64> = members.iter().map(|member| member.item.get()).collect();
    let existing = parent_locks::capture(ex, &ids).await.map_err(db_err)?;
    let existing_ids: BTreeSet<i64> = existing.iter().map(|row| row.item_id).collect();
    if existing_ids.len() != ids.len() { return Err(changed()); }
    let requests: Vec<LockRequest> = existing
        .iter()
        .map(|row| LockRequest { item_id: row.item_id, strength: LockStrength::Update })
        .collect();
    let locks = lock_items(ex, sde, &requests, &[]).await?;
    Ok((locks, existing_ids))
}

/// Guarded terminal transition plus physical custody delete for one group's
/// members. RPCs and deadline jobs share this path; a member whose stored
/// state moved on fails the batch instead of silently skipping.
pub(super) async fn discard_members(
    ex: &(impl Executor + ?Sized), sde: &Sde, group: &Group, members: &[RecoveryItem],
    from: &[Status], to: Status, now: i64,
) -> Result<Vec<RecoveryItemId>, CallError> {
    if members.is_empty() {
        return Ok(Vec::new());
    }
    let (locks, _) = lock_escrow(ex, sde, members).await?;
    let pairs: Vec<(RecoveryItemId, Option<ItemId>)> = members.iter().map(|member| (member.id, None)).collect();
    let moved = dao::terminalize(ex, &pairs, from, to, now).await.map_err(db_err)?;
    if moved.len() != members.len() {
        return Err(changed());
    }
    if !custody::delete_custody(ex, &locks, &Selection { group: group.clone(), items: members.to_vec() }).await.map_err(db_err)?
    {
        return Err(changed());
    }
    Ok(moved)
}

/// `CallAbortRequested({group: [member]})`: unpaid members only; the custody
/// row is deleted and the member marked `abandoned` in one transaction.
/// Re-aborting an abandoned member replays the notification without writes.
pub(super) async fn abort(c: &mut Client, call: &Call<'_>) -> CallResult {
    let identity = crate::services::identity::Identity::of_client(c);
    abort_for(identity, &c.db, &c.world, &c.sde, &c.notify_tx, requested_groups(call)?).await?;
    Ok(Value::None)
}

async fn abort_for(
    identity: crate::services::identity::Identity, db: &crate::db::Db,
    world: &crate::space::World, sde: &Arc<Sde>, sender: &crate::space::NotifyTx,
    requested: BTreeMap<RecoveryGroupId, Vec<RecoveryItemId>>,
) -> Result<(), CallError> {
    let me = identity.char_id;
    let _lock = market::lock().await;
    let sde = Arc::clone(sde);
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let requested = requested.clone();
            let sde = Arc::clone(&sde);
            Box::pin(async move {
                let selections = dao::load_selection(tx, me, &requested, true).await.map_err(db_err)?;
                verify_selection(&selections, &requested)?;
                let now = filetime_now();
                for selection in &selections {
                    let mut targets = Vec::new();
                    for member in wanted(selection, &requested) {
                        match member.status {
                            Status::AwaitingPayment => {
                                if now >= selection.group.payment_expires_at { return Err(notify_error(msg::PAYMENT_EXPIRED)); }
                                targets.push(member.clone());
                            }
                            Status::Abandoned => {}
                            _ => return Err(changed()),
                        }
                    }
                    if !targets.is_empty() {
                        discard_members(tx, &sde, &selection.group, &targets, &[Status::AwaitingPayment], Status::Abandoned, now).await?;
                        for member in &targets {
                            scheduler::cancel(tx, &deadlines::job_id(deadlines::PAYMENT, member.id)).await.map_err(db_err)?;
                        }
                    }
                }
                // Only the members the caller selected are named; the encoder
                // reads member ids, so pre-abort rows replay unchanged.
                let notified: Vec<Selection> = selections
                    .iter()
                    .map(|selection| Selection {
                        group: selection.group.clone(),
                        items: wanted(selection, &requested).into_iter().cloned().collect(),
                    })
                    .collect();
                let text = wire::abort_started(me, &notified).map_err(db_err)?;
                outbox.notify_client("OnAbortStarted", vec![Value::str(text)]);
                Ok(())
            })
        },
        |value, outbox| async move {
            let result = dispatch_sender(value, outbox, world, Some(sender));
            scheduler::wake();
            result
        },
    ).await
}

/// `CallItemsWithdrawalRequested({group: [member]})`: delivered members get a
/// new inventory row with every attachment migrated, then the escrow row is
/// deleted in the same transaction. A late job never blocks a withdrawal that
/// is still inside its persisted window; a repeat call replays the stored
/// `OnItemsWithdrawn` without touching inventory or jobs.
pub(super) async fn withdraw(c: &mut Client, call: &Call<'_>) -> CallResult {
    let identity = crate::services::identity::Identity::of_client(c);
    withdraw_for(identity, &c.db, &c.world, &c.sde, &c.notify_tx, requested_groups(call)?).await?;
    Ok(Value::None)
}

async fn withdraw_for(
    identity: crate::services::identity::Identity, db: &crate::db::Db,
    world: &crate::space::World, sde: &Arc<Sde>, sender: &crate::space::NotifyTx,
    requested: BTreeMap<RecoveryGroupId, Vec<RecoveryItemId>>,
) -> Result<(), CallError> {
    let me = identity.char_id;
    let _lock = market::lock().await;
    let sde = Arc::clone(sde);
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let requested = requested.clone();
            let sde = Arc::clone(&sde);
            Box::pin(async move {
                let selections = dao::load_selection(tx, me, &requested, true).await.map_err(db_err)?;
                verify_selection(&selections, &requested)?;
                let now = filetime_now();
                let mut withdrawn: Vec<(usize, RecoveryItem)> = Vec::new();
                let mut fresh: Vec<(usize, RecoveryItem)> = Vec::new();
                for (index, selection) in selections.iter().enumerate() {
                    for member in wanted(selection, &requested) {
                        match member.status {
                            Status::Withdrawn => {
                                if member.withdrawn_item.is_none_or(|id| id == member.item) {
                                    return Err(changed());
                                }
                                withdrawn.push((index, member.clone()));
                            }
                            Status::Ready | Status::Delivering => {
                                withdrawal_window(member.delivery_completed_at, member.withdraw_expires_at, now)?;
                                fresh.push((index, member.clone()));
                            }
                            Status::AwaitingPayment => return Err(notify_error(msg::NOT_DELIVERED)),
                            _ => return Err(changed()),
                        }
                    }
                }
                if !fresh.is_empty() {
                    let ids: Vec<i64> = fresh.iter().map(|(_, member)| member.item.get()).collect();
                    let captured = parent_locks::capture(tx, &ids).await.map_err(db_err)?;
                    if captured.len() != ids.len() {
                        return Err(changed());
                    }
                    let requests: Vec<LockRequest> = ids
                        .iter()
                        .map(|&item_id| LockRequest { item_id, strength: LockStrength::Update })
                        .collect();
                    let locks = lock_items(tx, &sde, &requests, &[]).await?;
                    for (index, member) in fresh {
                        let selection = &selections[index];
                        let single = Selection { group: selection.group.clone(), items: vec![member.clone()] };
                        let new_id = custody::restore_new(tx, &locks, &single, now)
                            .await
                            .map_err(db_err)?
                            .ok_or_else(changed)?;
                        let moved = dao::terminalize(
                            tx,
                            &[(member.id, Some(new_id))],
                            &[Status::Delivering, Status::Ready],
                            Status::Withdrawn,
                            now,
                        )
                        .await
                        .map_err(db_err)?;
                        if !moved.contains(&member.id) {
                            return Err(changed());
                        }
                        if !custody::delete_custody(tx, &locks, &single).await.map_err(db_err)? {
                            return Err(changed());
                        }
                        scheduler::cancel(tx, &deadlines::job_id(deadlines::DELIVERY, member.id)).await.map_err(db_err)?;
                        scheduler::cancel(tx, &deadlines::job_id(deadlines::WITHDRAW, member.id)).await.map_err(db_err)?;
                        notifications::notify_item_change(
                            tx, &sde, outbox, new_id.get(),
                            vec![(IX_LOCATION_ID, Value::Int(0))],
                        )
                        .await?;
                        withdrawn.push((index, RecoveryItem {
                            status: Status::Withdrawn,
                            withdrawn_item: Some(new_id),
                            ..member
                        }));
                    }
                }
                let notified: Vec<Selection> = selections
                    .iter()
                    .enumerate()
                    .filter_map(|(index, selection)| {
                        let mut items: Vec<RecoveryItem> = withdrawn
                            .iter()
                            .filter(|(i, _)| *i == index)
                            .map(|(_, member)| member.clone())
                            .collect();
                        items.sort_unstable_by_key(|member| member.id);
                        (!items.is_empty()).then(|| Selection { group: selection.group.clone(), items })
                    })
                    .collect();
                let text = wire::items_withdrawn(me, &notified).map_err(db_err)?;
                outbox.notify_client("OnItemsWithdrawn", vec![Value::str(text)]);
                Ok(())
            })
        },
        |value, outbox| async move {
            let result = dispatch_sender(value, outbox, world, Some(sender));
            scheduler::wake();
            result
        },
    ).await
}

/// `GetBatchDestinationSolarSystem({group: (station, structure)})`: returns
/// `{group: (solarSystemID, destinationID)}` for owned groups whose stored
/// destination still matches the client's declaration. Structures are not
/// supported destinations and are refused, never answered with a fake zero.
pub(super) async fn destinations(c: &mut Client, call: &Call<'_>) -> CallResult {
    destinations_for(&c.db, &c.sde, crate::services::identity::Identity::of_client(c), call).await
}

async fn destinations_for(db: &crate::db::Db, sde: &Sde, identity: crate::services::identity::Identity, call: &Call<'_>) -> CallResult {
    let me = identity.char_id;
    let mut requested: BTreeMap<RecoveryGroupId, (i64, i64)> = BTreeMap::new();
    let Value::Dict(pairs) = call.args.first().ok_or_else(invalid)? else { return Err(invalid()) };
    for (key, value) in pairs {
        let group = key.as_int().and_then(RecoveryGroupId::new).ok_or_else(invalid)?;
        let pair = value.items().ok_or_else(invalid)?;
        if pair.len() != 2 { return Err(invalid()); }
        let station = pair[0].as_int().ok_or_else(invalid)?;
        let structure = if pair[1].is_none() { 0 } else { pair[1].as_int().ok_or_else(invalid)? };
        if requested.insert(group, (station, structure)).is_some() { return Err(invalid()); }
    }
    if requested.is_empty() {
        return Ok(Value::Dict(vec![]));
    }
    let groups: BTreeMap<RecoveryGroupId, Vec<RecoveryItemId>> =
        requested.keys().map(|group| (*group, Vec::new())).collect();
    let selections = dao::load_selection(db, me, &groups, false).await.map_err(db_err)?;
    let mut reply = Vec::with_capacity(requested.len());
    for (group, (station, structure)) in &requested {
        if *structure != 0 {
            return Err(notify_error(msg::DESTINATION_INVALID));
        }
        let selection = selections.iter().find(|s| s.group.id == *group).ok_or_else(invalid)?;
        if *station != selection.group.destination_station {
            return Err(notify_error(msg::DESTINATION_INVALID));
        }
        let destination = sde.map.station(*station).ok_or_else(|| notify_error(msg::DESTINATION_INVALID))?;
        let system = destination
            .solar_system_id
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("destination station {station} has no solar system")))?;
        reply.push((Value::Int(group.get()), Value::tuple([Value::Int(system), Value::Int(*station)])));
    }
    Ok(Value::Dict(reply))
}

#[cfg(all(test, feature = "tests-services"))] mod tests;
