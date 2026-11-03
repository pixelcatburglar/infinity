//! Online state changes for fitted modules, mirroring clientDogmaLocation and godma.
//! Reads effect metadata from the SDE and persists online state in game.items.
//! Resource overloads are reconciled using baseDogmaLocation.py:1114-1183;
//! committed changes dispatch to clients or the world in outbox order.
use super::lifecycle::subject_core;
use super::notify::attribute_change;
use crate::client::{filetime_now, Client};
use crate::consts::{dogma::ATTR_IS_ONLINE, dogma::EFFECT_ONLINE};
use crate::db::{executor::Executor, outbox::{Effect as OutboxEffect, Outbox}};
use crate::messages;
use crate::sde::Sde;
use crate::services::{db_err, notify_error, CallError, CallResult};
use crate::services::identity::Identity;
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
    pub ship: i64,
    pub clear_heat: Vec<i64>,
}

impl OnlinePolicy {
    pub(in crate::services) fn from_identity(identity: &Identity, sender: NotifyTx) -> Result<Self, CallError> {
        if identity.char_id == 0 { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
        Ok(Self {
            owner: identity.char_id,
            active_ship: identity.ship_id,
            station: identity.station_id,
            notify: sender,
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
    if online {
        let fit = crate::services::fit::Fit::load(ex, sde.clone(), policy.owner, ship).await?;
        super::heat::damage::validate(&fit, item)?;
    }
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
    let clear_heat = if online { Vec::new() } else { crate::services::combat::targeting_charges::bank(ex, ship, master).await? };
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
    if changed || bank_changed || !clear_heat.is_empty() {
        bank_stops.extend(item_stop);
        outbox.operation(OnlineChanged { bank_stops, ship, clear_heat }.into());
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
    if !change.clear_heat.is_empty() { super::heat::clear_modules(change.ship, &change.clear_heat); }
}
/// Crate-visible so the Lua host move adapter can deliver its notifications.
pub(crate) fn dispatch_notification(notification: crate::db::outbox::Notification, world: &crate::space::World, sender: &NotifyTx, current_character: Option<i64>) -> Result<(), CallError> {
    match notification {
        crate::db::outbox::Notification::Pilot { char_id, event, args } if current_character == Some(char_id) => {
            let _ = sender.send((event, args));
            Ok(())
        }
        notification => notification.dispatch_sender(world, sender),
    }
}

pub(in crate::services) fn dispatch_entries<T>(value: T, outbox: Outbox<OnlineChanged>, mut notify: impl FnMut(crate::db::outbox::Notification) -> Result<(), CallError>) -> Result<T, CallError> {
    for entry in outbox.into_entries() {
        match entry {
            OutboxEffect::Operation(change) => apply_online_changed(change),
            OutboxEffect::Notification(notification) => notify(notification)?,
            OutboxEffect::Event(event) => crate::lua::on_event(event),
        }
    }
    Ok(value)
}

pub(in crate::services) fn dispatch_changed<T>(value: T, outbox: Outbox<OnlineChanged>, c: &Client) -> Result<T, CallError> {
    dispatch_entries(value, outbox, |notification| dispatch_notification(notification, &c.world, &c.notify_tx, c.session.int("charid")))
}

pub(in crate::services) fn dispatch_changed_world<T>(value: T, outbox: Outbox<OnlineChanged>, world: &crate::space::World) -> Result<T, CallError> {
    dispatch_entries(value, outbox, |notification| notification.dispatch(world, None))
}

/// Reconcile all owned online hulls while the caller holds the market lock.
pub(in crate::services) async fn reconcile_owner<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<E>, owner: i64,
) -> Result<(), CallError> {
    let candidates = crate::dao::items::fit::owner_online_ships(ex, owner).await.map_err(db_err)?;
    let mut ships: Vec<_> = crate::services::items::load_items(ex, sde, crate::dao::items::loading::ItemQuery::Ids(&candidates)).await.map_err(db_err)?
        .into_iter().filter(|item| item.owner_id == owner && item.singleton && item.category_id == i64::from(crate::consts::CATEGORY_SHIP))
        .map(|item| item.item_id).collect();
    ships.sort_unstable();
    for &ship in &ships {
        if !crate::dao::items::ship::lock_reload_ship(ex, ship).await.map_err(db_err)? {
            return Err(CallError::Internal(anyhow::anyhow!("ship disappeared")));
        }
    }
    for ship in ships {
        reconcile_core(ex, sde, outbox, owner, ship).await?;
    }
    Ok(())
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
    let fit = crate::services::fit::Fit::load(ex, sde.clone(), owner, ship).await?;
    let mut burnt = crate::dao::items::dogma::online(ex, ship).await.map_err(db_err)?;
    burnt.sort_unstable_by_key(|module| module.item_id);
    for entry in burnt {
        if !super::heat::damage::burnt(&fit, entry.item_id) { continue; }
        let module = load_items(ex, sde, ItemQuery::Id(entry.item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
        if module.owner_id != owner || module.location_id != ship || !module.singleton || !crate::services::inventory::is_fitting_flag(module.flag_id) { return Err(refuse()); }
        if !mutate_online(ex, outbox, &hull, &module, false, true, None).await? { return Err(refuse()); }
    }
    let mut remaining = crate::dao::items::dogma::online(ex, ship).await.map_err(db_err)?.len();
    let modules = crate::dao::items::fit::modules(ex, ship).await.map_err(db_err)?;
    let captured_at = filetime_now();
    let skills = crate::services::skills::effective::snapshot_at(ex, sde, owner, captured_at).await?;
    for module in modules.into_iter().filter(|module| module.online) {
        let missing_prerequisite = crate::services::skills::requirements::requirements(sde, module.type_id)
            .any(|(skill, level)| skills.level(skill) < level);
        if !missing_prerequisite { continue; }
        let module = load_items(ex, sde, ItemQuery::Id(module.item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
        if module.owner_id != owner || module.location_id != ship || !module.singleton || !crate::services::inventory::is_fitting_flag(module.flag_id) { return Err(refuse()); }
        if !mutate_online(ex, outbox, &hull, &module, false, true, None).await? { return Err(refuse()); }
    }
    while let Some(item) = super::fitting::overload_candidate(ex, sde.clone(), owner, ship).await? {
        if remaining == 0 { return Err(refuse()); }
        let module = load_items(ex, sde, ItemQuery::Id(item)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
        if module.owner_id != owner || module.location_id != ship || !module.singleton || !crate::services::inventory::is_fitting_flag(module.flag_id) { return Err(refuse()); }
        if !mutate_online(ex, outbox, &hull, &module, false, true, None).await? { return Err(refuse()); }
        remaining -= 1;
    }
    Ok(())
}

pub(in crate::services) async fn reconcile_ship(
    db: &crate::db::Db, sde: &Arc<Sde>, owner: i64, ship: i64,
    notify: impl FnMut(crate::db::outbox::Notification) -> Result<(), CallError> + Send,
) -> Result<(), CallError> {
    let _lock = crate::services::market::lock().await;
    let sde = sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            Box::pin(async move { reconcile_core(tx, &sde, outbox, owner, ship).await })
        },
        |value, outbox| async move { dispatch_entries(value, outbox, notify) },
    ).await
}

/// Change online state for an authorized fitted module; login_user comes from the owning account and is read only by the module-online/offline info log.
pub(crate) async fn set_online(
    identity: &Identity, db: &crate::db::Db, sde: &Arc<Sde>, sender: NotifyTx, login_user: i64,
    ship: i64, item_id: i64, online: bool,
    notify: impl FnMut(crate::db::outbox::Notification) -> Result<(), CallError> + Send,
) -> CallResult {
    let _lock = crate::services::market::lock().await;
    let policy = OnlinePolicy::from_identity(identity, sender)?;
    let sde = sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let policy = policy.clone();
            let sde = sde.clone();
            Box::pin(async move { set_online_core(tx, &sde, &policy, outbox, ship, item_id, online).await })
        },
        |changed, outbox| async move {
            dispatch_entries((), outbox, notify)?;
            if changed { info!(user = login_user, "module {item_id} {}", if online { "online" } else { "offline" }); }
            Ok(Value::None)
        },
    ).await
}
