//! Online state changes for fitted modules, mirroring clientDogmaLocation and godma.
//! Reads effect metadata from the SDE and persists online state in game.items.
//! Resource overloads are reconciled using baseDogmaLocation.py:1114-1183;
//! Transactions publish one final fit per hull after commit. Live condition is
//! owned by the system actor; stored shield rebases are checkpointed transactionally.
use super::lifecycle::subject_core;
use super::notify::attribute_change;
use crate::client::{filetime_now, Client};
use infinity_core::consts::{dogma::ATTR_IS_ONLINE, dogma::EFFECT_ONLINE};
use crate::db::{executor::Executor, outbox::{Effect as OutboxEffect, Outbox}};
use infinity_core::messages;
use crate::sde::Sde;
use crate::services::{db_err, notify_error, CallError, CallResult};
use crate::services::identity::Identity;
use crate::space::NotifyTx;
use marshal::Value;
use tracing::info;
use std::sync::Arc;
pub(in crate::services) mod fitting;

#[derive(Clone)]
pub(in crate::services) struct OnlinePolicy {
    pub owner: i64,
    pub active_ship: Option<i64>,
    pub station: Option<i64>,
    pub notify: NotifyTx,
}
/// Memberships captured before a cold transaction; never retained globally.
#[derive(Clone)]
pub(crate) struct OnlineFences(Arc<[(i64, i64, u64)]>);

impl OnlineFences {
    pub(in crate::services) fn membership(&self, ship: i64) -> Option<(i64, u64)> {
        self.0.iter().find_map(|&(system, id, generation)| (id == ship).then_some((system, generation)))
    }

    pub(in crate::services) fn single(system: i64, ship: i64, generation: Option<u64>) -> Self {
        Self(match generation {
            Some(generation) => Arc::from([(system, ship, generation)]),
            None => Arc::from([]),
        })
    }
    /// Membership systems in capture order; callers sort and dedup for gates.
    pub(in crate::services) fn systems(&self) -> impl Iterator<Item = i64> + '_ {
        self.0.iter().map(|&(system, _, _)| system)
    }
}

pub(crate) async fn capture_online_fences(world: &crate::space::World, owners: &[i64]) -> OnlineFences {
    OnlineFences(world.online_fences(owners).await.into())
}

/// Owned in-space drone instances captured before a cold ship-change boundary.
///
/// Each tuple is (system, drone, owner, drone generation, membership): the
/// membership is the same online-fence value hulls use, while the drone record
/// generation pins the exact instance so a relaunched replacement refuses the
/// publish. No new world enumeration is added; the actor records are rechecked
/// inside their own systems.
pub(in crate::services) async fn capture_owned_drone_fences(
    world: &crate::space::World, owner: i64,
) -> Vec<(i64, i64, i64, u64, u64)> {
    let mut by_system: std::collections::BTreeMap<i64, Vec<(i64, u64)>> = std::collections::BTreeMap::new();
    for (system, item, membership) in world.online_fences(&[owner]).await {
        by_system.entry(system).or_default().push((item, membership));
    }
    let mut captured = Vec::new();
    for (system, items) in by_system {
        let Some(rows) = world.with(system, move |s| {
            items.iter().filter_map(|&(id, membership)| {
                let drone = s.drone(id)?;
                (drone.owner == owner && owner != infinity_core::consts::OWNER_UNOWNED
                    && s.combat_membership_generation(id) == Some(membership))
                    .then_some((system, id, owner, drone.generation, membership))
            }).collect::<Vec<_>>()
        }).await else { continue };
        captured.extend(rows);
    }
    captured
}


pub(crate) struct OnlineMutation {
    bank_stops: Vec<crate::services::combat::banks::BankStop>,
    ship: i64,
    clear_heat: Vec<i64>,
    membership: Option<(i64, u64)>,
    module_online: Option<(i64, bool)>,
}

pub(crate) enum OnlineChanged {
    Normal(OnlineMutation),
    Fitting {
        system: i64,
        ship: i64,
        owner: i64,
        generation: u64,
        patch: crate::services::fit::attributes::CommittedFitting,
    },
    StoredFitting { owner: i64, changes: Vec<Value>, brain: Option<(Value, Value, Value)> },
    /// A committed full replacement of the owner's character domain, delivered
    /// to the recipients the pre-transaction fences captured.
    Character {
        owner: i64,
        fences: OnlineFences,
        inputs: crate::services::fit::character::CharacterInputs,
    },
}
crate::progress::leaf_effect!(OnlineChanged);

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
    ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, policy: &OnlinePolicy, outbox: &mut Outbox<E>, fences: &OnlineFences,
    ship: i64, item: i64, online: bool,
) -> Result<(bool, crate::services::fit::Fit), CallError> {
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    if !crate::dao::items::ship::lock_reload_ship(ex, ship).await.map_err(db_err)? { return Err(refuse()); }
    let (hull, module) = subject_core(ex, sde, policy, ship, item, EFFECT_ONLINE as i32, false).await?;
    let was_online = crate::dao::items::dogma::online(ex, ship).await.map_err(db_err)?.iter().any(|entry| entry.item_id == item);
    if online {
        let fit = crate::services::fit::Fit::load(ex, sde.clone(), policy.owner, ship).await?;
        super::heat::damage::validate(&fit, item)?;
    }
    if was_online == online && online {
        let fit = reconcile_core(ex, sde, outbox, fences, policy.owner, ship).await?;
        return Ok((false, fit));
    }
    if online { super::fitting::validate_online(ex, sde.clone(), policy.owner, ship, item).await?; }
    let changed = mutate_online(ex, outbox, fences, &hull, &module, online, was_online, Some(&policy.notify)).await?;
    let fit = reconcile_core(ex, sde, outbox, fences, policy.owner, ship).await?;
    Ok((changed, fit))
}

async fn mutate_online<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, fences: &OnlineFences,
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
        let membership = fences.0.iter().find_map(|&(system, id, generation)| (id == ship && system == hull.location_id).then_some((system, generation)));
        outbox.operation(OnlineChanged::Normal(OnlineMutation { bank_stops, ship, clear_heat, membership, module_online: changed.then_some((item, online)) }).into());
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

/// Offline a fitted module whose item is about to be deleted with its hull.
/// The shared inventory lifecycle already proved the complete locked closure and
/// caller authorization; this entry only rechecks the two locked rows and the
/// same-owner fitted child relationship, then delegates to the shared mutation.
pub(in crate::services) async fn offline_for_destruction<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, fences: &OnlineFences,
    hull: &crate::dao::items::loading::Item, module: &crate::dao::items::loading::Item,
    locks: &crate::dao::items::parent_locks::LockedItems<'_>, notify: Option<&NotifyTx>,
) -> Result<(), CallError> {
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    for (item, expected) in [(hull.item_id, hull), (module.item_id, module)] {
        let row = locks.row(item).ok_or_else(refuse)?;
        if !locks.is_write_locked(item) || row.jump_clone_id.is_some()
            || row.type_id != expected.type_id || row.owner_id != expected.owner_id
            || row.location_id != expected.location_id || row.flag_id != expected.flag_id
            || row.quantity != expected.quantity || row.singleton != expected.singleton {
            return Err(refuse());
        }
    }
    if module.owner_id != hull.owner_id || module.location_id != hull.item_id
        || !module.singleton || module.category_id != i64::from(infinity_core::consts::CATEGORY_MODULE)
        || !crate::services::inventory::is_fitting_flag(module.flag_id) {
        return Err(refuse());
    }
    let was_online = crate::dao::items::dogma::online(ex, hull.item_id).await.map_err(db_err)?
        .iter().any(|entry| entry.item_id == module.item_id);
    mutate_online(ex, outbox, fences, hull, module, false, was_online, notify).await?;
    Ok(())
}

fn queue_notification<E>(outbox: &mut Outbox<E>, sender: Option<&NotifyTx>, owner: i64, event: &str, args: Vec<Value>) {
    if let Some(sender) = sender { outbox.notify_sender(sender.clone(), event, args); }
    else { outbox.notify_pilot(owner, event, args); }
}

pub(in crate::services) fn queue_cleanup<E: From<OnlineChanged>>(outbox: &mut Outbox<E>, fences: &OnlineFences, ship: i64, modules: Vec<i64>) {
    outbox.operation(OnlineChanged::Normal(OnlineMutation { bank_stops: Vec::new(), ship, clear_heat: modules, membership: fences.membership(ship), module_online: None }).into());
}
/// Crate-visible so the Lua host move adapter can deliver its notifications.
pub(crate) fn dispatch_notification(notification: crate::db::outbox::Notification, world: &crate::space::World, sender: &NotifyTx, current_character: Option<i64>) -> Result<(), CallError> {
    match notification {
        crate::db::outbox::Notification::Pilot { char_id, event, args } if current_character == Some(char_id) => {
            let _ = sender.send((event, args));
            Ok(())
        }
        notification => notification.dispatch_sender(world, Some(sender)),
    }
}

pub(in crate::services) fn dispatch_mixed_entries<T, E>(
    value: T, outbox: Outbox<E>, world: &crate::space::World,
    mut split_online: impl FnMut(E) -> Result<OnlineChanged, E>,
    mut apply_other: impl FnMut(E) -> Result<(), CallError>,
    mut notify: impl FnMut(crate::db::outbox::Notification) -> Result<(), CallError>,
) -> Result<T, CallError> {
    use std::collections::{HashMap, HashSet};
    let entries = outbox.into_entries();
    // A notification-only outbox consumes its entries once in the original
    // order and allocates none of the mixed-dispatch collections.
    if !entries.iter().any(|entry| matches!(entry, OutboxEffect::Operation(_))) {
        for entry in entries {
            match entry {
                OutboxEffect::Notification(notification) => notify(notification)?,
                OutboxEffect::Operation(_) => unreachable!("notification-only entries checked"),
            }
        }
        return Ok(value);
    }
    let entries: Vec<_> = entries.into_iter().map(|entry| match entry {
        OutboxEffect::Operation(effect) => match split_online(effect) {
            Ok(change) => Ok(change),
            Err(other) => Err(OutboxEffect::Operation(other)),
        },
        OutboxEffect::Notification(notification) => Err(OutboxEffect::Notification(notification)),
    }).collect();
    let terminals: HashSet<_> = entries.iter().filter_map(|entry| match entry {
        Ok(OnlineChanged::Fitting { system, ship, generation, .. }) => Some((*system, *ship, *generation)),
        _ => None,
    }).collect();
    let mut cleanups: HashMap<_, Vec<super::heat::FittingHeatCleanup>> = HashMap::new();
    for entry in entries {
        match entry {
            Ok(OnlineChanged::Normal(OnlineMutation { bank_stops, ship, clear_heat, membership, module_online })) => {
                for stop in bank_stops { crate::services::combat::banks::apply_stop(stop); }
                let key = membership.map(|(system, generation)| (system, ship, generation));
                if let Some((system, _, generation)) = key.filter(|key| terminals.contains(key)) {
                    if !clear_heat.is_empty() {
                        cleanups.entry((system, ship, generation)).or_default().push(super::heat::detach_for_fitting(ship, system, &clear_heat));
                    }
                } else {
                    if !clear_heat.is_empty() { super::heat::clear_modules(ship, &clear_heat); }
                    if let (Some((module, online)), Some((system, generation))) = (module_online, membership) {
                        world.post(system, move |s| {
                            if s.combat_membership_generation(ship) == Some(generation) { s.fitted_module_online(ship, module, online); }
                        });
                    }
                }
            }
            Ok(OnlineChanged::Fitting { system, ship, owner, generation, patch }) => {
                let cleanup = cleanups.remove(&(system, ship, generation)).unwrap_or_default();
                world.post(system, move |s| { s.publish_fitting(ship, owner, generation, patch, cleanup); });
            }
            Ok(OnlineChanged::StoredFitting { owner, changes, brain }) => {
                if !changes.is_empty() {
                    let notification = crate::db::outbox::Notification::Pilot {
                        char_id: owner, event: "OnModuleAttributeChanges".into(), args: vec![Value::list(changes)],
                    };
                    notify(notification)?;
                }
                if let Some((character, ship, structure)) = brain {
                    let notification = crate::db::outbox::Notification::Pilot {
                        char_id: owner, event: crate::services::dogma::brain::UPDATED_EVENT.into(),
                        args: crate::services::dogma::brain::update_args(owner, character, ship, structure),
                    };
                    notify(notification)?;
                }
            }
            Ok(OnlineChanged::Character { owner, fences, inputs }) => {
                // One package shared by every recipient in a system. A piloted
                // hull publish already fans the same inputs out to that
                // system's owned drones, so the per-item fallback only reaches
                // captured members the hull publish did not cover.
                let inputs = Arc::new(inputs);
                let mut systems: HashMap<i64, Vec<(i64, u64)>> = HashMap::new();
                for &(system, item, membership) in fences.0.iter() {
                    systems.entry(system).or_default().push((item, membership));
                }
                for (system, items) in systems {
                    let inputs = inputs.clone();
                    world.post(system, move |s| {
                        if let Some(ball) = s.pilot_ball(owner) {
                            if let Some(&(_, membership)) = items.iter().find(|&&(item, _)| item == ball.id) {
                                if s.publish_character_inputs(ball.id, owner, membership, inputs.as_ref()) { return; }
                            }
                        }
                        for &(item, membership) in &items {
                            s.publish_character_inputs(item, owner, membership, inputs.as_ref());
                        }
                    });
                }
            }
            Err(OutboxEffect::Operation(other)) => apply_other(other)?,
            Err(OutboxEffect::Notification(notification)) => notify(notification)?,
        }
    }
    Ok(value)
}

pub(in crate::services) fn dispatch_entries<T>(value: T, outbox: Outbox<OnlineChanged>, world: &crate::space::World, mut notify: impl FnMut(crate::db::outbox::Notification) -> Result<(), CallError>) -> Result<T, CallError> {
    dispatch_mixed_entries(value, outbox, world, Ok, |_| unreachable!(), |notification| {
        if let Err(error) = notify(notification) { tracing::warn!("postcommit notification failed: {error:?}"); }
        Ok(())
    })
}

pub(in crate::services) fn dispatch_changed<T>(value: T, outbox: Outbox<OnlineChanged>, c: &Client) -> Result<T, CallError> {
    dispatch_entries(value, outbox, &c.world, |notification| dispatch_notification(notification, &c.world, &c.notify_tx, c.session.int("charid")))
}

pub(crate) fn dispatch_changed_world<T>(value: T, outbox: Outbox<OnlineChanged>, world: &crate::space::World) -> Result<T, CallError> {
    dispatch_entries(value, outbox, world, |notification| notification.dispatch(world, None))
}

/// Reconcile all owned online hulls while the caller holds the market lock.
///
/// The shared character domain is extracted once from the first reconciled
/// final fit before `finish` consumes it, without another character read. With
/// no online hull candidates a single character-only cold load supplies it;
/// the recipient set comes from the pre-transaction fences, not the candidates.
pub(in crate::services) async fn reconcile_owner<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<E>, fences: &OnlineFences, owner: i64, at: i64,
) -> Result<crate::services::fit::character::CharacterInputs, CallError> {
    let candidates = crate::dao::items::fit::owner_online_ships(ex, owner).await.map_err(db_err)?;
    let mut ships: Vec<_> = crate::services::items::load_items(ex, sde, crate::dao::items::loading::ItemQuery::Ids(&candidates)).await.map_err(db_err)?
        .into_iter().filter(|item| item.owner_id == owner && item.singleton && item.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP))
        .map(|item| item.item_id).collect();
    ships.sort_unstable();
    for &ship in &ships {
        if !crate::dao::items::ship::lock_reload_ship(ex, ship).await.map_err(db_err)? {
            return Err(CallError::Internal(anyhow::anyhow!("ship disappeared")));
        }
    }
    let mut inputs = None;
    for ship in ships {
        let before = fitting::capture(ex, sde, fences, owner, ship).await?;
        let final_fit = reconcile_core(ex, sde, outbox, fences, owner, ship).await?;
        if inputs.is_none() { inputs = Some(final_fit.character_inputs()); }
        fitting::finish(ex, before, final_fit, outbox).await?;
    }
    match inputs {
        Some(inputs) => Ok(inputs),
        None => Ok(crate::services::fit::Fit::load_character_at(ex, sde.clone(), owner, at).await?.character_inputs()),
    }
}


pub(in crate::services) async fn reconcile_core<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<E>, fences: &OnlineFences, owner: i64, ship: i64,
) -> Result<crate::services::fit::Fit, CallError> {
    use crate::dao::items::loading::ItemQuery;
    use crate::services::items::load_items;
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    if !crate::dao::items::ship::lock_reload_ship(ex, ship).await.map_err(db_err)? { return Err(refuse()); }
    let hull = load_items(ex, sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
    if hull.owner_id != owner || hull.category_id != i64::from(infinity_core::consts::CATEGORY_SHIP) || !hull.singleton { return Err(refuse()); }
    let fit = crate::services::fit::Fit::load(ex, sde.clone(), owner, ship).await?;
    let mut burnt = crate::dao::items::dogma::online(ex, ship).await.map_err(db_err)?;
    burnt.sort_unstable_by_key(|module| module.item_id);
    for entry in burnt {
        if !super::heat::damage::burnt(&fit, entry.item_id) { continue; }
        // The energizer's activation burn keeps its own module online for the
        // accepted duration; ordinary burnt modules still reconcile offline.
        if super::active_cycle::running(entry.item_id) { continue; }
        let module = load_items(ex, sde, ItemQuery::Id(entry.item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
        if module.owner_id != owner || module.location_id != ship || !module.singleton || !crate::services::inventory::is_fitting_flag(module.flag_id) { return Err(refuse()); }
        if !mutate_online(ex, outbox, fences, &hull, &module, false, true, None).await? { return Err(refuse()); }
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
        if !mutate_online(ex, outbox, fences, &hull, &module, false, true, None).await? { return Err(refuse()); }
    }
    loop {
        let (fit, candidate) = super::fitting::overload_candidate(ex, sde.clone(), owner, ship).await?;
        let Some(item) = candidate else { return Ok(fit) };
        if remaining == 0 { return Err(refuse()); }
        let module = load_items(ex, sde, ItemQuery::Id(item)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
        if module.owner_id != owner || module.location_id != ship || !module.singleton || !crate::services::inventory::is_fitting_flag(module.flag_id) { return Err(refuse()); }
        if !mutate_online(ex, outbox, fences, &hull, &module, false, true, None).await? { return Err(refuse()); }
        remaining -= 1;
    }
}

pub(in crate::services) async fn reconcile_ship(
    db: &crate::db::Db, world: &crate::space::World, sde: &Arc<Sde>, owner: i64, ship: i64,
    notify: impl FnMut(crate::db::outbox::Notification) -> Result<(), CallError> + Send,
) -> Result<(), CallError> {
    let fences = capture_online_fences(world, &[owner]).await;
    let armed = crate::services::combat::loaded::LoadedHolds::arm(world, &fences.membership(ship).map(|(system, _)| (system, ship)).into_iter().collect::<Vec<_>>(), notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let _lock = crate::services::market::lock().await;
    let mut holds = crate::services::combat::loaded::LoadedHolds::lock(world, armed, notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let settle = holds.settle_rows();
    let sde = sde.clone();
    holds.may_execute();
    let result = db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            let settle = settle.clone();
            Box::pin(async move {
                if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
                let hull = crate::services::items::load_items(tx, &sde, crate::dao::items::loading::ItemQuery::Id(ship)).await.map_err(db_err)?
                    .into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                if hull.owner_id != owner || hull.category_id != i64::from(infinity_core::consts::CATEGORY_SHIP) || !hull.singleton {
                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                crate::services::combat::loaded::settle_in(tx, &settle).await?;
                let before = fitting::capture(tx, &sde, &fences, owner, ship).await?;
                let final_fit = reconcile_core(tx, &sde, outbox, &fences, owner, ship).await?;
                fitting::finish(tx, before, final_fit, outbox).await
            })
        },
        |value, outbox| async move { dispatch_entries(value, outbox, world, notify) },
    ).await;
    holds.finish(db, world, result, &[]).await
}

/// Change online state for an authorized fitted module; login_user comes from the owning account and is read only by the module-online/offline info log.
pub(crate) async fn set_online(
    identity: &Identity, db: &crate::db::Db, world: &crate::space::World, sde: &Arc<Sde>, sender: NotifyTx, login_user: i64,
    ship: i64, item_id: i64, online: bool,
    notify: impl FnMut(crate::db::outbox::Notification) -> Result<(), CallError> + Send,
) -> CallResult {
    let policy = OnlinePolicy::from_identity(identity, sender)?;
    let fences = capture_online_fences(world, &[identity.char_id]).await;
    let armed = crate::services::combat::loaded::LoadedHolds::arm(world, &fences.membership(ship).map(|(system, _)| (system, ship)).into_iter().collect::<Vec<_>>(), notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let _lock = crate::services::market::lock().await;
    let mut holds = crate::services::combat::loaded::LoadedHolds::lock(world, armed, notify_error(messages::fitting::CHARGES_SETTLING)).await?;
    let settle = holds.settle_rows();
    let sde = sde.clone();
    holds.may_execute();
    let result = db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let policy = policy.clone();
            let sde = sde.clone();
            let fences = fences.clone();
            let settle = settle.clone();
            Box::pin(async move {
                if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
                subject_core(tx, &sde, &policy, ship, item_id, EFFECT_ONLINE as i32, false).await?;
                let before = fitting::capture(tx, &sde, &fences, policy.owner, ship).await?;
                crate::services::combat::loaded::settle_in(tx, &settle).await?;
                let (changed, final_fit) = set_online_core(tx, &sde, &policy, outbox, &fences, ship, item_id, online).await?;
                fitting::finish(tx, before, final_fit, outbox).await?;
                Ok(changed)
            })
        },
        |changed, outbox| async move {
            dispatch_entries((), outbox, world, notify)?;
            if changed { info!(user = login_user, "module {item_id} {}", if online { "online" } else { "offline" }); }
            Ok(Value::None)
        },
    ).await;
    holds.finish(db, world, result, &[]).await
}
