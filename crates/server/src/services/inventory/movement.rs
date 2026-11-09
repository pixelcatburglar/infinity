//! Item movement and authorization for invInventory (eve/client/script/environment/invCache.py).
//! Sources and destinations use fresh ancestry, owner-bound bay permissions and
//! sampled physical reachability from game.items and the system actor. Shared-bay
//! deposits preserve item ownership; whole takes acquire the subtree after pending
//! reload settlement. Loot rights, suspect flags and POS tower control are not modelled.

use super::access::{corporate_custody, custody_allowed, Container};
use super::destination::{Custody, Destination};
use super::is_fitting_flag;
use super::notifications::{notify_corp_item_change, notify_item_change};
use super::reachability::{Endpoint, Reachability};
use super::stacks::split_stack;
use infinity_core::consts::{CATEGORY_SHIP, CORP_HANGAR_DIVISIONS, FLAG_CARGO, FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, FLAG_HANGAR, IX_FLAG, IX_LOCATION_ID, IX_OWNER_ID, IX_SINGLETON, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::dao::items::{inventory as inventory_dao, loading::{Item, ItemQuery}};
use crate::db::{executor::Executor, outbox::Outbox};
use infinity_core::messages;
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
}
crate::progress::leaf_effect!(MoveEffect);

impl From<super::super::effects::online::OnlineChanged> for MoveEffect {
    fn from(change: super::super::effects::online::OnlineChanged) -> Self { Self::Online(change) }
}

pub(super) fn dispatch_move<T>(value: T, outbox: Outbox<MoveEffect>, world: &World, mut notify: impl FnMut(Notification) -> Result<(), CallError>) -> Result<T, CallError> {
    super::super::effects::online::dispatch_mixed_entries(value, outbox, world,
        |effect| match effect { MoveEffect::Online(change) => Ok(change), other => Err(other) },
        |effect| { match effect { MoveEffect::Bank(stop) => super::super::combat::banks::apply_stop(stop), MoveEffect::Online(_) => unreachable!() }; Ok(()) },
        |notification| {
            if let Err(error) = notify(notification) { tracing::warn!("postcommit notification failed: {error:?}"); }
            Ok(())
        })
}


#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceProof { Corporate(i64, i64, i32), Personal, ForeignLoot, Shared }

pub(super) fn shared_lineage<'a>(sde: &Sde, endpoint: Endpoint, len: usize, get: impl Fn(i64) -> Option<&'a crate::dao::items::deliveries::Ancestor>) -> bool {
    shared_bay(sde, endpoint, len, get).is_some()
}

fn shared_bay<'a>(sde: &Sde, endpoint: Endpoint, len: usize, get: impl Fn(i64) -> Option<&'a crate::dao::items::deliveries::Ancestor>) -> Option<(i64, i32)> {
    let mut location = endpoint.location;
    let mut flag = endpoint.flag;
    for _ in 0..len {
        let Some(row) = get(location) else { break };
        if sde.types.get(row.type_id).is_some_and(|ty| ty.category_id == Some(CATEGORY_SHIP)) {
            return matches!(flag, infinity_core::consts::FLAG_FLEET_HANGAR | infinity_core::consts::FLAG_SHIP_HANGAR)
                .then_some((row.item_id, flag));
        }
        location = row.location_id;
        flag = row.flag_id;
    }
    None
}

async fn authorized_source(ex: &(impl Executor + ?Sized), sde: &Sde, item: &Item, source: Option<i64>, char_id: i64, station: Option<i64>, system: Option<i64>, shared: bool) -> Result<Option<SourceProof>, CallError> {
    if source.is_some_and(|id| id != item.location_id) { return Ok(None); }
    if shared && station.is_none() && system.is_some() { return Ok(Some(SourceProof::Shared)); }
    if let Some((corp, location, flag)) = corporate_custody(ex, Some(sde), item.item_id).await? {
        if custody_allowed(ex, sde, item.item_id, char_id, station, true).await? != Some(true) {
            return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE));
        }
        return Ok(Some(SourceProof::Corporate(corp, location, flag)));
    }
    if shared {
        return Ok(Some(SourceProof::Shared));
    }
    if item.owner_id == char_id { return Ok(Some(SourceProof::Personal)); }
    if station.is_some() || system.is_none() { return Ok(None); }
    let Some(parent) = load_items(ex, sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
    Ok((parent.group_id == i64::from(infinity_core::consts::GROUP_WRECK) || i32::try_from(parent.group_id).ok().is_some_and(|id| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&id))).then_some(SourceProof::ForeignLoot))
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
            let default = if item.category_id == CATEGORY_SHIP as i64 { FLAG_CARGO } else { FLAG_HANGAR };
            let flag = flag.unwrap_or(default);
            // Foreign parents are provisional here; the combined reachability
            // and shared-lineage proof below decides whether they are usable.
            Ok(Some((*id, char_id, flag, false)))
        }
    }
}

/// Resolve and authorize a selected destination. `None` means denial or staleness.
pub(in crate::services) async fn authorize_destination(
    ex: &(impl Executor + ?Sized), sde: &Sde, identity: &Identity, reachability: &Reachability,
    container: &Container, flag: Option<i32>,
) -> Result<Option<Destination>, CallError> {
    let Some((location, owner, flag, corporate)) = authorized_destination(ex, sde, container, flag,
        identity.char_id, identity.station_id, (identity.corp_id != 0).then_some(identity.corp_id)).await? else { return Ok(None) };
    let endpoint = Endpoint { location, flag };
    let checked = reachability.inspect_many(ex, sde, &[endpoint]).await?;
    if !checked.admitted[0] { return Ok(None); }
    let lineage = checked.lineage(location);
    let shared = lineage.is_some_and(|rows| shared_lineage(sde, endpoint, rows.len(), |id| rows.get(&id)));
    let parent = lineage.and_then(|rows| rows.get(&location));
    if !corporate && parent.is_some_and(|row| row.owner_id != identity.char_id) && !shared { return Ok(None); }
    let capacity_owner = parent.map_or(owner, |row| row.owner_id);
    Ok(Some(Destination { item_owner: owner, capacity_owner, location, flag }))
}

fn same_item_snapshot(current: &Item, item: &Item) -> bool {
    current.owner_id == item.owner_id && current.location_id == item.location_id && current.flag_id == item.flag_id
        && current.type_id == item.type_id && current.singleton == item.singleton && current.quantity == item.quantity
}

fn checked_bay(sde: &Sde, checked: &super::reachability::CheckedEndpoints, endpoint: Endpoint) -> Option<(i64, i32)> {
    checked.lineage(endpoint.location).and_then(|rows| shared_bay(sde, endpoint, rows.len(), |id| rows.get(&id)))
}

fn checked_destination(
    sde: &Sde, identity: &Identity, checked: &super::reachability::CheckedEndpoints,
    item: &Item, proof: &SourceProof, source_bay: Option<(i64, i32)>, destination: (i64, i64, i32, bool),
) -> Option<(Destination, Custody)> {
    let (location, mut owner, flag, corporate) = destination;
    let endpoint = Endpoint { location, flag };
    let rows = checked.lineage(location);
    let destination_bay = checked_bay(sde, checked, endpoint);
    let parent = rows.and_then(|rows| rows.get(&location));
    if !corporate && parent.is_some_and(|row| row.owner_id != identity.char_id) && destination_bay.is_none() { return None; }
    let same = item.location_id == location && item.flag_id == flag;
    let same_bay = source_bay.is_some() && source_bay == destination_bay;
    if !corporate && (same && source_bay.is_some() || same_bay || source_bay.is_none() && destination_bay.is_some() && item.owner_id == identity.char_id) {
        owner = item.owner_id;
    }
    let capacity_owner = parent.map_or(owner, |row| row.owner_id);
    let custody = if !same && !same_bay && (*proof == SourceProof::Shared || matches!(proof, SourceProof::Corporate(..)) && item.owner_id != owner) {
        Custody::TakeSubtree
    } else { Custody::Preserve };
    Some((Destination { item_owner: owner, capacity_owner, location, flag }, custody))
}

/// Authorize both selected endpoints with one fresh physical and ACL proof.
/// `None` means denial or staleness; it is not a successful no-op.
pub(in crate::services) async fn authorize_move(
    ex: &(impl Executor + ?Sized), sde: &Sde, identity: &Identity, reachability: &Reachability,
    item: &Item, source: Option<i64>, container: &Container, flag: Option<i32>,
) -> Result<Option<(Destination, Custody)>, CallError> {
    let Some(current) = load_items(ex, sde, ItemQuery::Id(item.item_id)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
    if !same_item_snapshot(&current, item) { return Ok(None); }
    let Some(destination) = authorized_destination(ex, sde, container, flag,
        identity.char_id, identity.station_id, (identity.corp_id != 0).then_some(identity.corp_id)).await? else { return Ok(None) };
    let source_endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
    let checked = reachability.inspect_many(ex, sde, &[source_endpoint, Endpoint { location: destination.0, flag: destination.2 }]).await?;
    if checked.admitted.iter().any(|allowed| !allowed) { return Ok(None); }
    let source_bay = checked_bay(sde, &checked, source_endpoint);
    let Some(proof) = authorized_source(ex, sde, item, source, identity.char_id, identity.station_id, identity.system_id, source_bay.is_some()).await? else { return Ok(None) };
    Ok(checked_destination(sde, identity, &checked, item, &proof, source_bay, destination))
}

/// Authorize a batch atomically using one current-row read and one endpoint proof.
/// Results retain input order; `None` means that at least one item was denied or stale.
pub(in crate::services) async fn authorize_moves(
    ex: &(impl Executor + ?Sized), sde: &Sde, identity: &Identity, reachability: &Reachability,
    items: &[Item], source: Option<i64>, container: &Container, flag: Option<i32>,
) -> Result<Option<Vec<(Destination, Custody)>>, CallError> {
    let ids: Vec<_> = items.iter().map(|item| item.item_id).collect();
    let current = load_items(ex, sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
    let current: std::collections::HashMap<_, _> = current.iter().map(|item| (item.item_id, item)).collect();
    if items.iter().any(|item| current.get(&item.item_id).is_none_or(|row| !same_item_snapshot(row, item))) { return Ok(None); }
    let Some(destination) = authorized_destination(ex, sde, container, flag,
        identity.char_id, identity.station_id, (identity.corp_id != 0).then_some(identity.corp_id)).await? else { return Ok(None) };
    let mut endpoints = std::collections::BTreeSet::new();
    endpoints.insert((destination.0, destination.2));
    endpoints.extend(items.iter().map(|item| (item.location_id, item.flag_id)));
    let endpoints: Vec<_> = endpoints.into_iter().map(|(location, flag)| Endpoint { location, flag }).collect();
    let checked = reachability.inspect_many(ex, sde, &endpoints).await?;
    if checked.admitted.iter().any(|allowed| !allowed) { return Ok(None); }
    let mut proofs = std::collections::HashMap::new();
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
        let source_bay = checked_bay(sde, &checked, endpoint);
        let key = (item.location_id, item.flag_id, item.owner_id);
        if !proofs.contains_key(&key) {
            let Some(proof) = authorized_source(ex, sde, item, source, identity.char_id, identity.station_id, identity.system_id, source_bay.is_some()).await? else { return Ok(None) };
            proofs.insert(key, proof);
        }
        let Some(value) = checked_destination(sde, identity, &checked, item, &proofs[&key], source_bay, destination) else { return Ok(None) };
        result.push(value);
    }
    Ok(Some(result))
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
    let source_endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
    let source_ancestry = crate::dao::items::deliveries::ancestry(db, item.location_id).await.map_err(db_err)?;
    let source_bay = shared_bay(sde, source_endpoint, source_ancestry.len(), |id| source_ancestry.iter().find(|row| row.item_id == id));
    let source_shared = source_bay.is_some();
    let source_proof = authorized_source(db, sde, &item, source, char_id, station, system, source_shared).await?;
    let destination = if source_proof.is_some() { authorized_destination(db, sde, container, flag, char_id, station, corporation).await? } else { None };
    let Some((location, owner, destination_flag, corporate_destination)) = destination else {
        let destination = match container { Container::Hangar { station_id, .. } | Container::Deliveries { station_id, .. } => *station_id, Container::Item(id) => *id };
        warn!(char_id, item_id, destination, "inventory move authorization refused");
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    };
    if Some(item_id) == identity.ship_id && matches!(container, Container::Item(_)) {
        return Err(CallError::User { msg: messages::MSG_CAN_NOT_PLACE_ACTIVE_SHIP_IN_ANOTHER_HANGAR.into(), dict: vec![] });
    }
    let destination_endpoint = Endpoint { location, flag: destination_flag };
    let reachability = Reachability::capture(identity, db, world, space_system, &[source_endpoint, destination_endpoint]).await?;
    let mut audience_endpoints = vec![source_endpoint, destination_endpoint];
    let destination_ancestry = crate::dao::items::deliveries::ancestry(db, location).await.map_err(db_err)?;
    let destination_bay = shared_bay(sde, destination_endpoint, destination_ancestry.len(), |id| destination_ancestry.iter().find(|row| row.item_id == id));
    let destination_shared = destination_bay.is_some();
    let same = item.location_id == location && item.flag_id == destination_flag;
    let partial = !item.singleton && qty.is_some_and(|quantity| quantity > 0 && quantity < i64::from(item.quantity));
    let possible_take = !same && !partial && (source_bay.is_none() || source_bay != destination_bay)
        && (source_proof == Some(SourceProof::Shared)
            || matches!(&source_proof, Some(SourceProof::Corporate(..))) && item.owner_id != owner);
    let pending_snapshot = if possible_take {
        let preparation = super::super::combat::reload_recovery::TakePreparation::capture(db, sde, item_id).await?;
        audience_endpoints.extend_from_slice(preparation.endpoints());
        Some(preparation)
    } else { None };
    let destination_parent = if matches!(container, Container::Item(_)) && !corporate_destination {
        load_items(db, sde, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next()
    } else { None };
    let audience = if source_proof == Some(SourceProof::Shared) || destination_shared {
        Some(super::notifications::Audience::capture(identity, db, world, space_system, &audience_endpoints).await?)
    } else { None };
    let sde = sde.clone();
    let active_ship = identity.ship_id;
    let identity = Identity { system_id: space_system, ..*identity };
    let container = container.clone();
    let policy = super::super::effects::online::OnlinePolicy::from_identity(&identity, sender)?;
    let mut fence_owners = vec![item.owner_id, owner];
    if is_fitting_flag(item.flag_id) {
        if let Some(parent) = source_ancestry.iter().find(|row| row.item_id == item.location_id) { fence_owners.push(parent.owner_id); }
    }
    if is_fitting_flag(destination_flag) || destination_flag == FLAG_AUTOFIT {
        if let Some(parent) = destination_ancestry.iter().find(|row| row.item_id == location) { fence_owners.push(parent.owner_id); }
    }
    fence_owners.sort_unstable();
    fence_owners.dedup();
    let fences = super::super::effects::online::capture_online_fences(world, &fence_owners).await;
    db.transaction(
        move |tx, outbox: &mut Outbox<MoveEffect>| {
            let item = item.clone();
            let container = container.clone();
            let sde = sde.clone();
            let policy = policy.clone();
            let fences = fences.clone();
            let reachability = reachability.clone();
            let audience = audience.clone();
            let mut pending_snapshot = pending_snapshot.as_ref().map(|preparation| preparation.fresh_attempt());
            let destination_ancestry = destination_ancestry.clone();
            let source_ancestry = source_ancestry.clone();
            let destination_parent = destination_parent.clone();
            Box::pin(async move {
                let flag = destination_flag;
                let destination_fitting = is_fitting_flag(flag) || (flag == FLAG_AUTOFIT
                    && (item.category_id == i64::from(infinity_core::consts::CATEGORY_MODULE) || item.category_id == i64::from(infinity_core::consts::CATEGORY_SUBSYSTEM)));
                let source_fitting = is_fitting_flag(item.flag_id);
                use crate::dao::items::parent_locks::{LockRequest, LockStrength};
                let mut requests = vec![LockRequest { item_id: item.item_id, strength: LockStrength::Update }];
                requests.extend(super::destination::parent_requests(&sde,
                    source_ancestry.iter().chain(destination_ancestry.iter()).map(|row| (row.item_id, row.type_id)),
                    LockStrength::Update, LockStrength::Share)?);
                for row in source_ancestry.iter().filter(|row| row.item_id == item.location_id)
                    .chain(destination_ancestry.iter().filter(|row| row.item_id == location)) {
                    if super::hold_storage::is_ordinary_container(&sde, row.type_id) {
                        requests.push(LockRequest { item_id: row.item_id, strength: LockStrength::Update });
                    }
                }
                if let Some(preparation) = &pending_snapshot {
                    requests.extend(preparation.closure().iter().chain(preparation.ancestors()).map(|id|
                        LockRequest { item_id: *id, strength: LockStrength::Update }));
                }
                let mut optional = Vec::new();
                if let Some(preparation) = &pending_snapshot {
                    let mut strict: std::collections::HashSet<_> = requests.iter().map(|request| request.item_id).collect();
                    strict.extend(preparation.refund().mandatory_ids());
                    optional = preparation.refund().candidate_ids().into_iter().filter(|id| !strict.contains(id)).collect();
                    requests.extend_from_slice(preparation.refund().requests());
                }
                let locks = super::destination::lock_items(tx, &sde, &requests, &optional).await?;
                if crate::dao::items::deliveries::ancestry(tx, item.location_id).await.map_err(db_err)? != source_ancestry
                    || crate::dao::items::deliveries::ancestry(tx, location).await.map_err(db_err)? != destination_ancestry
                    || !locks.row(item.item_id).is_some_and(|row| row.jump_clone_id.is_none()
                        && row.type_id == item.type_id && row.owner_id == item.owner_id && row.location_id == item.location_id
                        && row.flag_id == item.flag_id && row.quantity == item.quantity && row.singleton == item.singleton) {
                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                if let Some(preparation) = &pending_snapshot {
                    preparation.revalidate(tx).await?;
                    preparation.refund().revalidate(tx, &sde, &locks).await?;
                }
                let _parent_guard = destination_parent.as_ref().filter(|parent| parent.category_id != i64::from(CATEGORY_SHIP))
                    .map(|parent| super::destination::ParentSnapshot { item: parent.clone(), ancestry: destination_ancestry.clone() })
                    .map(|snapshot| super::destination::ParentGuard::revalidate(&locks, &[snapshot])).transpose()?;
                let (mut destination, mut custody) = authorize_move(tx, &sde, &identity, &reachability, &item, source, &container, Some(flag)).await?
                    .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                if destination.location != location || destination.flag != destination_flag {
                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                let audience = match audience { Some(audience) => Some(audience.resolve(tx, &sde).await?), None => None };
                if custody == Custody::TakeSubtree {
                    if let Some(preparation) = &pending_snapshot { preparation.revalidate(tx).await?; }
                }
                let owner = destination.item_owner;
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
                if custody == Custody::TakeSubtree && item.location_id == location && item.flag_id == flag {
                    destination.item_owner = item.owner_id;
                    custody = Custody::Preserve;
                }
                let owner = destination.item_owner;
                let take = if is_fitting_flag(flag) { Some(1) } else { qty }
                    .filter(|q| *q > 0 && !item.singleton && *q < i64::from(item.quantity));
                if custody == Custody::TakeSubtree && take.is_none() && pending_snapshot.is_none() {
                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                let departure = source_fitting && take.is_none() && (item.location_id != location || item.flag_id != flag || item.owner_id != owner);
                let leaving_fit = departure && (item.location_id != location || !is_fitting_flag(flag) || item.owner_id != owner);
                let online = leaving_fit && item.category_id == i64::from(infinity_core::consts::CATEGORY_MODULE)
                    && crate::dao::items::dogma::online(tx, item.location_id).await.map_err(db_err)?.iter().any(|entry| entry.item_id == item.item_id);
                let mut affected = Vec::new();
                if departure { affected.push((item.location_id, item.owner_id)); }
                if is_fitting_flag(flag) { affected.push((location, owner)); }
                affected.sort_unstable();
                affected.dedup_by_key(|entry| entry.0);
                let mut boundaries = Vec::new();
                for (ship, owner) in affected {
                    boundaries.push((ship, owner, super::super::effects::online::fitting::capture(tx, &sde, &fences, owner, ship).await?));
                }
                let bank_departure = if departure {
                    let before = super::super::combat::banks::snapshot(tx, item.location_id, &[item.item_id]).await?;
                    let master = super::super::combat::targeting_charges::master_for_member(tx, item.location_id, item.item_id).await?;
                    if !online {
                        let mut modules = super::super::combat::targeting_charges::bank(tx, item.location_id, master).await?;
                        if !modules.contains(&item.item_id) { modules.push(item.item_id); }
                        super::super::effects::online::queue_cleanup(outbox, &fences, item.location_id, modules);
                    }
                    let changed = super::super::combat::banks::remove_member(tx, item.location_id, item.item_id, master, false).await?;
                    Some((before, changed))
                } else { None };
                if online {
                    super::super::effects::online::set_online_core(tx, &sde, &policy, outbox, &fences, item.location_id, item.item_id, false).await?;
                }
                destination.flag = flag;
                let actor = if audience.is_some() { char_id } else { destination.item_owner };
                let moved_id = move_core(tx, &sde, outbox, &item, &destination, take, custody, &locks, pending_snapshot.as_mut(), audience.as_ref(), actor).await?
                    .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                if let Some(audience) = &audience {
                    if let Some(moved) = load_items(tx, &sde, ItemQuery::Id(moved_id)).await.map_err(db_err)?.into_iter().next() {
                        if take.is_some() {
                            if let Some(left) = load_items(tx, &sde, ItemQuery::Id(item.item_id)).await.map_err(db_err)?.into_iter().next() {
                                audience.item_change(outbox, &item, &left, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))], char_id);
                            }
                            let created = Item { location_id: LOCATION_JUNKYARD, ..moved.clone() };
                            audience.item_change(outbox, &created, &moved, vec![(IX_LOCATION_ID, Value::Int(LOCATION_JUNKYARD))], char_id);
                        } else {
                            let mut change = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(i64::from(item.flag_id)))];
                            if item.owner_id != moved.owner_id { change.push((IX_OWNER_ID, Value::Int(item.owner_id))); }
                            audience.item_change(outbox, &item, &moved, change, char_id);
                        }
                    }
                }
                if is_fitting_flag(flag) {
                    super::super::effects::fitting::validate_fit(tx, sde.clone(), owner, location, moved_id).await?;
                }
                if let Some((before, true)) = bank_departure {
                    for stop in super::super::combat::banks::changes(tx, item.location_id, before).await? { outbox.operation(MoveEffect::Bank(stop)); }
                    outbox.notify_client("OnWeaponBanksChanged", vec![Value::Int(item.location_id), super::super::combat::banks(tx, item.location_id).await?]);
                }
                for (ship, owner, before) in boundaries {
                    let final_fit = super::super::effects::online::reconcile_core(tx, &sde, outbox, &fences, owner, ship).await?;
                    super::super::effects::online::fitting::finish(tx, before, final_fit, outbox).await?;
                }
                if take.is_some() || item.location_id != location || item.flag_id != flag || item.owner_id != owner || (is_fitting_flag(flag) && !item.singleton) {
                    outbox.event(crate::lua::events::GameEvent::ItemsMoved {
                        char_id, item_id: moved_id, from_location_id: item.location_id,
                        to_location_id: location, quantity: take.unwrap_or_else(|| crate::services::item_volume::stack_size(item.singleton, item.quantity)),
                    });
                }
                Ok(Some(moved_id))
            })
        },
        |value, outbox| async move { dispatch_move(value, outbox, world, notify) },
    ).await
}

/// Guarded movement primitive; `None` means no destination item was produced.
/// Selected endpoints require a fresh combined proof before this call. A whole
/// subtree take must first lock and revalidate its prepared closure and ancestors,
/// and settle with the resolved audience if shared refund viewers are relevant.
pub(in crate::services) async fn move_core<X: Send>(
    tx: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>,
    item: &Item, destination: &Destination, take: Option<i64>, custody: Custody,
    locks: &crate::dao::items::parent_locks::LockedItems<'_>,
    preparation: Option<&mut super::super::combat::reload_recovery::TakePreparation>,
    audience: Option<&super::notifications::ResolvedAudience>, actor: i64,
) -> Result<Option<i64>, CallError> {
    let Destination { item_owner: owner, capacity_owner, location, flag } = *destination;
    let mut guarded = if take.is_none() && (item.singleton || custody == Custody::TakeSubtree) {
        inventory_dao::descendants(tx, &[item.item_id]).await.map_err(db_err)?
    } else { vec![item.item_id] };
    if !guarded.contains(&item.item_id) { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
    guarded.push(location);
    if !crate::dao::trades::reserved(tx, &guarded).await.map_err(db_err)?.is_empty() {
        return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
    }
    guarded.pop();
    if crate::dao::industry::has_active_blueprint_job(tx, &guarded).await.map_err(db_err)? {
        return Err(notify_error(messages::industry::BLUEPRINT_ACTIVE));
    }
    if item.item_id == location || crate::dao::items::deliveries::ancestry(tx, location).await.map_err(db_err)?.iter().any(|row| row.item_id == item.item_id) {
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    }
    let mut transfer_closure = Vec::new();
    let mut created = Vec::new();
    let previous_descendants = if custody == Custody::TakeSubtree && take.is_none() {
        let preparation = preparation.ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
        if inventory_dao::descendants(tx, &[item.item_id]).await.map_err(db_err)? != preparation.closure()
            || preparation.closure().iter().any(|id| !locks.is_write_locked(*id)) {
            return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
        let receipts = super::super::combat::reload_recovery::settle_subtree(tx, sde, outbox, item.item_id, location, actor, audience, locks, preparation).await?;
        created.extend(receipts.iter().filter_map(|receipt| receipt.created.as_ref().map(|row| row.item.item_id)));
        transfer_closure = inventory_dao::descendants(tx, &[item.item_id]).await.map_err(db_err)?;
        if preparation.closure().iter().any(|id| !transfer_closure.contains(id))
            || transfer_closure.iter().any(|id| !preparation.closure().contains(id) && !created.contains(id)) {
            return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
        created.retain(|id| transfer_closure.contains(id));
        load_items(tx, sde, ItemQuery::Ids(&transfer_closure)).await.map_err(db_err)?
    } else { Vec::new() };
    let same_flag = item.location_id == location && item.flag_id == flag;
    let same_box = item.location_id == location && locks.row(location)
        .is_some_and(|parent| super::hold_storage::is_ordinary_container(sde, parent.type_id));
    let fitting = is_fitting_flag(flag);
    let quantity = if same_flag && !same_box { i64::from(item.quantity) } else { i64::from(take.unwrap_or(item.quantity)) };
    let additions = [
        super::hold_storage::Addition { type_id: item.type_id, singleton: item.singleton || fitting, quantity },
        super::hold_storage::Addition { type_id: item.type_id, singleton: item.singleton, quantity: i64::from(item.quantity) - i64::from(take.unwrap_or(0)) },
    ];
    let count = if same_box && take.is_some() { 2 } else { 1 };
    let excluded = if same_flag || same_box { std::slice::from_ref(&item.item_id) } else { &[] };
    super::hold_storage::check_destination(tx, sde, capacity_owner, location, flag, &additions[..count],
        super::hold_storage::Replaced { items: excluded, reloads: &[] }, None).await?;
    let item_id = item.item_id;
    let corporate = corporate_custody(tx, Some(sde), item.item_id).await?.is_some();
    if let Some(q) = take {
        let id = split_stack(tx, item, q, owner, location, flag, fitting).await?;
        if item.owner_id != owner && corporate {
            let left = Item { quantity: item.quantity - q, ..item.clone() };
            notify_corp_item_change(tx, outbox, &left, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
        } else {
            notify_item_change(tx, sde, outbox, item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
        }
        let mut change = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))];
        if item.owner_id != owner { change.push((IX_OWNER_ID, Value::Int(item.owner_id))); }
        notify_item_change(tx, sde, outbox, id, change).await?;
        Ok(Some(id))
    } else {
        if item.owner_id != owner && corporate {
            let gone = Item { location_id: LOCATION_JUNKYARD, ..item.clone() };
            notify_corp_item_change(tx, outbox, &gone, vec![(IX_LOCATION_ID, Value::Int(item.location_id))]).await?;
        }
        let singleton = item.singleton || fitting;
        if !inventory_dao::move_item(tx, item, location, flag, owner, singleton).await.map_err(db_err)? {
            return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
        }
        if custody == Custody::TakeSubtree {
            crate::dao::items::transfers::hand_over(tx, &[crate::dao::items::transfers::Transfer { item_id, owner, location, flag }], &transfer_closure, locks, &created).await.map_err(db_err)?;
            for previous in previous_descendants.iter().filter(|row| row.item_id != item_id && row.owner_id != owner) {
                let gone = Item { location_id: LOCATION_JUNKYARD, ..previous.clone() };
                outbox.notify_pilot(previous.owner_id, "OnItemChange", vec![gone.row(),
                    Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(previous.location_id))]), Value::None]);
            }
        }
        let mut change = vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(item.flag_id as i64))];
        if item.owner_id != owner { change.push((IX_OWNER_ID, Value::Int(item.owner_id))); }
        if singleton != item.singleton {
            change.push((IX_SINGLETON, Value::Bool(item.singleton)));
        }
        notify_item_change(tx, sde, outbox, item_id, change).await?;
        Ok(Some(item_id))
    }
}
