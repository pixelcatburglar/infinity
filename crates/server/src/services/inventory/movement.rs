//! Item movement and authorization for invInventory (eve/client/script/environment/invCache.py).
//! Sources and destinations require authorization and shared physical reachability,
//! using game.items, SDE type data and sampled space state. Successful moves queue
//! item-move events after writes; online modules are offlined transactionally, with
//! bank and cycle effects dispatched after commit. Loot rights and suspect flags,
//! foreign shared-bay permissions and POS tower control are not implemented.

use super::access::{corporate_custody, custody_allowed, Container};
use super::is_fitting_flag;
use super::notifications::{notify_corp_item_change, notify_item_change};
use super::reachability::{Endpoint, Reachability};
use super::stacks::split_stack;
use crate::consts::{CATEGORY_SHIP, CORP_HANGAR_DIVISIONS, FLAG_CARGO, FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, FLAG_HANGAR, IX_FLAG, IX_LOCATION_ID, IX_OWNER_ID, IX_SINGLETON, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::dao::items::{inventory as inventory_dao, loading::{Item, ItemQuery}};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::messages;
use crate::sde::Sde;
use crate::services::{db_err, items::load_items, notify_error, CallError};
use marshal::Value;
use tracing::warn;
use crate::services::identity::Identity;
use crate::db::{Db, outbox::Notification};
use crate::space::{NotifyTx, World};
use std::sync::Arc;

const FLAG_AUTOFIT: i32 = 0;

pub(super) enum MoveEffect {
    Bank(super::super::combat::banks::BankStop),
    Online(super::super::effects::online::OnlineChanged),
    ClearHeat(i64, Vec<i64>),
}

impl From<super::super::effects::online::OnlineChanged> for MoveEffect {
    fn from(change: super::super::effects::online::OnlineChanged) -> Self { Self::Online(change) }
}

pub(super) fn dispatch_move<T>(value: T, outbox: Outbox<MoveEffect>, mut notify: impl FnMut(Notification) -> Result<(), CallError>) -> Result<T, CallError> {
    use crate::db::outbox::Effect;
    for entry in outbox.into_entries() {
        match entry {
            Effect::Operation(MoveEffect::Bank(stop)) => super::super::combat::banks::apply_stop(stop),
            Effect::Operation(MoveEffect::Online(change)) => super::super::effects::online::apply_online_changed(change),
            Effect::Operation(MoveEffect::ClearHeat(ship, modules)) => super::super::effects::heat::clear_modules(ship, &modules),
            Effect::Notification(notification) => notify(notification)?,
            Effect::Event(event) => crate::lua::on_event(event),
        }
    }
    Ok(value)
}


#[derive(Clone, PartialEq, Eq)]
enum SourceProof { Corporate(i64, i64, i32), Personal, ForeignLoot }

async fn authorized_source(ex: &(impl Executor + ?Sized), sde: &Sde, item: &Item, source: Option<i64>, char_id: i64, station: Option<i64>, system: Option<i64>) -> Result<Option<SourceProof>, CallError> {
    if source.is_some_and(|id| id != item.location_id) { return Ok(None); }
    if let Some((corp, location, flag)) = corporate_custody(ex, Some(sde), item.item_id).await? {
        if custody_allowed(ex, sde, item.item_id, char_id, station, true).await? != Some(true) {
            return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE));
        }
        return Ok(Some(SourceProof::Corporate(corp, location, flag)));
    }
    if item.owner_id == char_id { return Ok(Some(SourceProof::Personal)); }
    if station.is_some() || system.is_none() { return Ok(None); }
    let Some(parent) = load_items(ex, sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
    Ok((parent.group_id == i64::from(crate::consts::GROUP_WRECK) || i32::try_from(parent.group_id).ok().is_some_and(|id| crate::consts::CARGO_CONTAINER_GROUPS.contains(&id))).then_some(SourceProof::ForeignLoot))
}

async fn authorized_destination(ex: &(impl Executor + ?Sized), sde: &Sde, container: &Container, flag: Option<i32>, char_id: i64, station: Option<i64>, corporation: Option<i64>) -> Result<Option<(i64, i64, i32, bool)>, CallError> {
    if flag == Some(FLAG_CORP_DELIVERIES) { return Ok(None); }
    match container {
        Container::Deliveries { .. } => Ok(None),
        Container::Hangar { station_id, owner_id } => {
            if *owner_id != char_id || station != Some(*station_id) { return Ok(None); }
            Ok(Some((*station_id, *owner_id, flag.filter(|f| *f != FLAG_AUTOFIT).unwrap_or(FLAG_HANGAR), false)))
        }
        Container::Item(id) => {
            if let Some(corp) = super::super::offices::office_owner(ex, *id).await? {
                if corporation != Some(corp) {
                    return Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), Value::unicode(messages::NOT_YOUR_CORPORATION_HANGAR))] });
                }
                let sag = FLAG_CORP_SAG1;
                return Ok(Some((*id, corp, flag.filter(|f| (sag..sag + CORP_HANGAR_DIVISIONS).contains(f)).unwrap_or(sag), true)));
            }
            if corporate_custody(ex, Some(sde), *id).await?.is_some() { return Ok(None); }
            let Some(item) = load_items(ex, sde, ItemQuery::Id(*id)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
            if item.owner_id != char_id { return Ok(None); }
            let default = if item.category_id == CATEGORY_SHIP as i64 { FLAG_CARGO } else { FLAG_HANGAR };
            Ok(Some((*id, char_id, flag.unwrap_or(default), false)))
        }
    }
}

/// Move `item_id` (all of it, or `qty` split off a stack) into `container` under `flag`.
/// Returns the ID of the item now at the destination.
pub(crate) async fn move_item(
    identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, space_system: Option<i64>, sender: NotifyTx,
    container: &Container, item_id: i64, source: Option<i64>, flag: Option<i32>, qty: Option<i64>,
    notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> Result<Option<i64>, CallError> {
    let _lock = super::super::market::lock().await;
    let char_id = identity.char_id;
    let Some(item) = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
    let station = identity.station_id;
    let system = space_system;
    let corporation = (identity.corp_id != 0).then_some(identity.corp_id);
    let source_proof = authorized_source(db, sde, &item, source, char_id, station, system).await?;
    let destination = if source_proof.is_some() { authorized_destination(db, sde, container, flag, char_id, station, corporation).await? } else { None };
    let Some((location, owner, destination_flag, corporate_destination)) = destination else {
        let destination = match container { Container::Hangar { station_id, .. } | Container::Deliveries { station_id, .. } => *station_id, Container::Item(id) => *id };
        warn!(char_id, item_id, destination, "inventory move authorization refused");
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    };
    if Some(item_id) == identity.ship_id && matches!(container, Container::Item(_)) {
        return Err(CallError::User { msg: messages::MSG_CAN_NOT_PLACE_ACTIVE_SHIP_IN_ANOTHER_HANGAR.into(), dict: vec![] });
    }
    let source_endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
    let destination_endpoint = Endpoint { location, flag: destination_flag };
    let reachability = Reachability::capture(identity, db, world, space_system, &[source_endpoint, destination_endpoint]).await?;
    let sde = sde.clone();
    let active_ship = identity.ship_id;
    let container = container.clone();
    let policy = super::super::effects::online::OnlinePolicy::from_identity(identity, sender)?;
    db.transaction(
        move |tx, outbox: &mut Outbox<MoveEffect>| {
            let item = item.clone();
            let container = container.clone();
            let source_proof = source_proof.clone();
            let sde = sde.clone();
            let policy = policy.clone();
            let reachability = reachability.clone();
            Box::pin(async move {
                let current = load_items(tx, &sde, ItemQuery::Id(item.item_id)).await.map_err(db_err)?.into_iter().next()
                    .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                if current.owner_id != item.owner_id || current.location_id != item.location_id || current.flag_id != item.flag_id
                    || current.type_id != item.type_id || current.singleton != item.singleton || current.quantity != item.quantity
                    || authorized_source(tx, &sde, &current, source, char_id, station, system).await? != source_proof
                    || authorized_destination(tx, &sde, &container, flag, char_id, station, corporation).await?
                        != Some((location, owner, destination_flag, corporate_destination)) {
                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                let item = current;
                let flag = destination_flag;
                reachability.check(tx, &sde, source_endpoint).await?;
                reachability.check(tx, &sde, destination_endpoint).await?;
                let destination_fitting = is_fitting_flag(flag) || (flag == FLAG_AUTOFIT
                    && (item.category_id == i64::from(crate::consts::CATEGORY_MODULE) || item.category_id == i64::from(crate::consts::CATEGORY_SUBSYSTEM)));
                let source_fitting = is_fitting_flag(item.flag_id);
                let mut locks = Vec::new();
                if source_fitting { locks.push(item.location_id); }
                if destination_fitting { locks.push(location); }
                locks.sort_unstable();
                locks.dedup();
                for ship in locks {
                    let exists = crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?;
                    if ship == location && destination_fitting && !exists {
                        return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
                    }
                }
                if destination_fitting {
                    let hull = load_items(tx, &sde, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                    if hull.owner_id != owner || hull.category_id != i64::from(CATEGORY_SHIP) || !hull.singleton
                        || (active_ship != Some(location) && !(station == Some(hull.location_id) && hull.flag_id == FLAG_HANGAR)) {
                        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                    }
                }
                let flag = if flag == FLAG_AUTOFIT && destination_fitting {
                    super::super::effects::fitting::free_slot(tx, sde.clone(), owner, location, item.item_id, item.type_id).await?
                        .ok_or_else(|| notify_error(messages::fitting::SLOTS))?
                } else if flag == FLAG_AUTOFIT { FLAG_CARGO } else { flag };
                let take = if is_fitting_flag(flag) { Some(1) } else { qty }
                    .filter(|q| *q > 0 && !item.singleton && (*q as i32) < item.quantity);
                let departure = source_fitting && take.is_none() && (item.location_id != location || item.flag_id != flag || item.owner_id != owner);
                let leaving_fit = departure && (item.location_id != location || !is_fitting_flag(flag) || item.owner_id != owner);
                let online = leaving_fit && item.category_id == i64::from(crate::consts::CATEGORY_MODULE)
                    && crate::dao::items::dogma::online(tx, item.location_id).await.map_err(db_err)?.iter().any(|entry| entry.item_id == item.item_id);
                let bank_departure = if departure {
                    let before = super::super::combat::banks::snapshot(tx, item.location_id, &[item.item_id]).await?;
                    let master = super::super::combat::targeting_charges::master_for_member(tx, item.location_id, item.item_id).await?;
                    if !online {
                        let mut modules = super::super::combat::targeting_charges::bank(tx, item.location_id, master).await?;
                        if !modules.contains(&item.item_id) { modules.push(item.item_id); }
                        outbox.operation(MoveEffect::ClearHeat(item.location_id, modules));
                    }
                    let changed = super::super::combat::banks::remove_member(tx, item.location_id, item.item_id, master, false).await?;
                    Some((before, changed))
                } else { None };
                if online {
                    super::super::effects::online::set_online_core(tx, &sde, &policy, outbox, item.location_id, item.item_id, false).await?;
                }
                let moved_id = move_core(tx, &sde, outbox, &item, owner, location, flag, take).await?;
                if is_fitting_flag(flag) {
                    super::super::effects::fitting::validate_fit(tx, sde.clone(), owner, location, moved_id).await?;
                }
                if let Some((before, true)) = bank_departure {
                    for stop in super::super::combat::banks::changes(tx, item.location_id, before).await? { outbox.operation(MoveEffect::Bank(stop)); }
                    outbox.notify_client("OnWeaponBanksChanged", vec![Value::Int(item.location_id), super::super::combat::banks(tx, item.location_id).await?]);
                }
                if departure { super::super::effects::online::reconcile_core(tx, &sde, outbox, item.owner_id, item.location_id).await?; }
                if is_fitting_flag(flag) { super::super::effects::online::reconcile_core(tx, &sde, outbox, owner, location).await?; }
                if take.is_some() || item.location_id != location || item.flag_id != flag || item.owner_id != owner || (is_fitting_flag(flag) && !item.singleton) {
                    outbox.event(crate::lua::events::GameEvent::ItemsMoved {
                        char_id, item_id: moved_id, from_location_id: item.location_id,
                        to_location_id: location, quantity: take.map_or(crate::services::item_volume::stack_size(item.singleton, i64::from(item.quantity)) as i32, |q| q as i32),
                    });
                }
                Ok(Some(moved_id))
            })
        },
        |value, outbox| async move { dispatch_move(value, outbox, notify) },
    ).await
}

pub(super) async fn move_core<X: Send>(
    tx: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>,
    item: &Item, owner: i64, location: i64, flag: i32, take: Option<i64>,
) -> Result<i64, CallError> {
    if !crate::dao::trades::reserved(tx, &[item.item_id, location]).await.map_err(db_err)?.is_empty() {
        return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
    }
    if crate::dao::industry::has_active_blueprint_job(tx, &[item.item_id]).await.map_err(db_err)? {
        return Err(notify_error(messages::industry::BLUEPRINT_ACTIVE));
    }
    let item_id = item.item_id;
    let fitting = is_fitting_flag(flag);
    let corporate = corporate_custody(tx, Some(sde), item.item_id).await?.is_some();
    if let Some(q) = take {
        let id = split_stack(tx, item, q as i32, owner, location, flag, fitting).await?;
        if item.owner_id != owner && corporate {
            let left = Item { quantity: item.quantity - q as i32, ..item.clone() };
            notify_corp_item_change(tx, outbox, &left, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
        } else {
            notify_item_change(tx, sde, outbox, item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
        }
        let mut change = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))];
        if item.owner_id != owner { change.push((IX_OWNER_ID, Value::Int(item.owner_id))); }
        notify_item_change(tx, sde, outbox, id, change).await?;
        Ok(id)
    } else {
        if item.owner_id != owner && corporate {
            let gone = Item { location_id: LOCATION_JUNKYARD, ..item.clone() };
            notify_corp_item_change(tx, outbox, &gone, vec![(IX_LOCATION_ID, Value::Int(item.location_id))]).await?;
        }
        let singleton = item.singleton || fitting;
        if !inventory_dao::move_item(tx, item, location, flag, owner, singleton).await.map_err(db_err)? {
            return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
        }
        if item.owner_id != owner && corporate {
            crate::dao::items::transfers::hand_over(tx, &[crate::dao::items::transfers::Transfer { item_id, owner, location, flag }]).await.map_err(db_err)?;
        }
        let mut change = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))];
        if item.owner_id != owner { change.push((IX_OWNER_ID, Value::Int(item.owner_id))); }
        if singleton != item.singleton {
            change.push((IX_SINGLETON, Value::Bool(item.singleton)));
        }
        notify_item_change(tx, sde, outbox, item_id, change).await?;
        Ok(item_id)
    }
}
