//! Online state changes for fitted modules, mirroring clientDogmaLocation and godma.
//! Reads effect metadata from the SDE and persists online state in game.items.
//! Resource overloads are reconciled using baseDogmaLocation.py:1114-1183.
use super::lifecycle::subject_core;
use super::notify::attribute_change;
use crate::client::{filetime_now, Client};
use crate::consts::{ATTR_IS_ONLINE, EFFECT_ONLINE};
use crate::db::{executor::Executor, outbox::{Effect as OutboxEffect, Outbox}};
use crate::messages;
use crate::sde::Sde;
use crate::services::{db_err, notify_error, CallError, CallResult};
use crate::space::NotifyTx;
use marshal::Value;
use tracing::info;
use std::sync::Arc;

#[derive(Clone)]
pub(in crate::services) struct OnlinePolicy {
    pub owner: i64,
    pub active_ship: Option<i64>,
    pub station: Option<i64>,
    pub notify: NotifyTx,
}

pub(in crate::services) struct OnlineChanged {
    pub bank_stops: Vec<crate::services::combat::banks::BankStop>,
}

impl OnlinePolicy {
    pub(in crate::services) fn from_client(c: &Client) -> Result<Self, CallError> {
        Ok(Self {
            owner: c.session.int("charid").ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?,
            active_ship: c.session.int("shipid"),
            station: c.session.int("stationid"),
            notify: c.notify_tx.clone(),
        })
    }
}

pub(in crate::services) async fn set_online_core<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, policy: &OnlinePolicy, outbox: &mut Outbox<E>,
    ship: i64, item: i64, online: bool,
) -> Result<bool, CallError> {
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    if !crate::dao::items::ship::lock_reload_ship(ex, ship).await.map_err(db_err)? { return Err(refuse()); }
    let (hull, module) = subject_core(ex, sde, policy, ship, item, EFFECT_ONLINE as i32, false).await?;
    let was_online = crate::dao::items::dogma::online(ex, ship).await.map_err(db_err)?.iter().any(|entry| entry.item_id == item);
    if was_online == online && online {
        reconcile_core(ex, sde, outbox, policy.owner, ship).await?;
        return Ok(false);
    }
    if online { super::fitting::validate_online(ex, sde.clone(), policy.owner, ship, item).await?; }
    let changed = mutate_online(ex, outbox, &hull, &module, online, was_online, Some(&policy.notify)).await?;
    reconcile_core(ex, sde, outbox, policy.owner, ship).await?;
    Ok(changed)
}

async fn mutate_online<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>,
    hull: &crate::dao::items::loading::Item, module: &crate::dao::items::loading::Item,
    online: bool, was_online: bool, notify: Option<&NotifyTx>,
) -> Result<bool, CallError> {
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    let ship = hull.item_id;
    let item = module.item_id;
    let owner = hull.owner_id;
    let before = if online { None } else { Some(crate::services::combat::banks::snapshot(ex, ship, &[item]).await?) };
    let master = if online { item } else { crate::services::combat::targeting_charges::master_for_member(ex, ship, item).await? };
    let item_stop = if online { None } else { crate::services::combat::banks::capture_stop(item) };
    let changed = was_online != online;
    if changed && crate::dao::items::combat::set_online(ex, item, ship, owner, module.flag_id, module.type_id, hull.type_id, hull.location_id, hull.flag_id, hull.singleton, online).await.map_err(db_err)? == 0 { return Err(refuse()); }
    let mut bank_stops = Vec::new();
    let mut bank_changed = false;
    if let Some(before) = before {
        if crate::services::combat::banks::remove_member(ex, ship, item, master, true).await? {
            bank_changed = true;
            bank_stops = crate::services::combat::banks::changes(ex, ship, before).await?;
            queue_notification(outbox, notify, owner, "OnWeaponBanksChanged", vec![Value::Int(ship), crate::services::combat::banks(ex, ship).await?]);
        }
    }
    if changed || bank_changed {
        bank_stops.extend(item_stop);
        outbox.operation(OnlineChanged { bank_stops }.into());
    }
    if changed {
        let now = filetime_now();
        let environment = super::environment::build(item, owner, ship, None, None, EFFECT_ONLINE);
        let effect = super::notify::ship_effect_args(item, EFFECT_ONLINE, environment, online, now, -1.0, -1, now);
        let attr = attribute_change(owner, Value::Int(item), ATTR_IS_ONLINE, Value::Int(online as i64), Value::Int(!online as i64));
        queue_notification(outbox, notify, owner, "OnGodmaShipEffect", effect);
        queue_notification(outbox, notify, owner, "OnModuleAttributeChanges", vec![Value::list([attr])]);
    }
    Ok(changed)
}

fn queue_notification<E>(outbox: &mut Outbox<E>, sender: Option<&NotifyTx>, owner: i64, event: &str, args: Vec<Value>) {
    if let Some(sender) = sender { outbox.notify_sender(sender.clone(), event, args); }
    else { outbox.notify_pilot(owner, event, args); }
}

pub(in crate::services) fn apply_online_changed(change: OnlineChanged) {
    for stop in change.bank_stops { crate::services::combat::banks::apply_stop(stop); }
}
pub(in crate::services) fn dispatch_notification(notification: crate::db::outbox::Notification, c: &Client) -> Result<(), CallError> {
    match notification {
        crate::db::outbox::Notification::Pilot { char_id, event, args } if c.session.int("charid") == Some(char_id) => {
            let _ = c.notify_tx.send((event, args));
            Ok(())
        }
        notification => notification.dispatch_sender(&c.world, &c.notify_tx),
    }
}

pub(in crate::services) fn dispatch_changed<T>(value: T, outbox: Outbox<OnlineChanged>, c: &Client) -> Result<T, CallError> {
    for entry in outbox.into_entries() {
        match entry {
            OutboxEffect::Operation(change) => apply_online_changed(change),
            OutboxEffect::Notification(notification) => dispatch_notification(notification, c)?,
            OutboxEffect::Event(event) => crate::lua::on_event(event),
        }
    }
    Ok(value)
}


pub(in crate::services) async fn reconcile_core<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<E>, owner: i64, ship: i64,
) -> Result<(), CallError> {
    use crate::dao::items::loading::ItemQuery;
    use crate::services::items::load_items;
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    if !crate::dao::items::ship::lock_reload_ship(ex, ship).await.map_err(db_err)? { return Err(refuse()); }
    let hull = load_items(ex, sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
    if hull.owner_id != owner || hull.category_id != i64::from(crate::consts::CATEGORY_SHIP) || !hull.singleton { return Err(refuse()); }
    let mut remaining = crate::dao::items::dogma::online(ex, ship).await.map_err(db_err)?.len();
    while let Some(item) = super::fitting::overload_candidate(ex, sde.clone(), owner, ship).await? {
        if remaining == 0 { return Err(refuse()); }
        let module = load_items(ex, sde, ItemQuery::Id(item)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
        if module.owner_id != owner || module.location_id != ship || !module.singleton || !crate::services::inventory::is_fitting_flag(module.flag_id) { return Err(refuse()); }
        if !mutate_online(ex, outbox, &hull, &module, false, true, None).await? { return Err(refuse()); }
        remaining -= 1;
    }
    Ok(())
}

pub(in crate::services) async fn reconcile_ship(c: &Client, owner: i64, ship: i64) -> Result<(), CallError> {
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            Box::pin(async move { reconcile_core(tx, &sde, outbox, owner, ship).await })
        },
        |value, outbox| async move { dispatch_changed(value, outbox, c) },
    ).await
}

/// Change online state for an authorized fitted module, including docked hangar ships.
pub(crate) async fn set_online(c: &mut Client, ship: i64, item_id: i64, online: bool) -> CallResult {
    let _lock = crate::services::market::lock().await;
    let policy = OnlinePolicy::from_client(c)?;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let policy = policy.clone();
            let sde = sde.clone();
            Box::pin(async move { set_online_core(tx, &sde, &policy, outbox, ship, item_id, online).await })
        },
        |changed, outbox| async move {
            dispatch_changed((), outbox, c)?;
            if changed { info!(user = c.login.user_id, "module {item_id} {}", if online { "online" } else { "offline" }); }
            Ok(Value::None)
        },
    ).await
}
