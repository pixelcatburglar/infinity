//! Atomic ordinary mutaplasmid creation for `eve/client/script/dogma/dynamicItemSvc.py`.
//! Uses immutable SDE mappings, game inventory and overrides, and provenance with
//! typed factor rows in `game.dynamic_item_modifiers`. Rolls are supplied by the caller
//! before transaction retries. Deployed entities require World removal and are
//! unsupported; only reachable local inventory custody is accepted.
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use infinity_core::{consts::{dogma::ATTR_CAPACITY, dogma::ATTR_HULL_DAMAGE, dogma::ATTR_MASS, dogma::ATTR_RADIUS, dogma::ATTR_VOLUME, CARGO_CONTAINER_GROUPS, CATEGORY_SHIP, CORP_HANGAR_DIVISIONS, FLAG_BOOSTER, FLAG_CARGO, FLAG_CORP_SAG1, FLAG_HANGAR, FLAG_IMPLANT, FLAG_PILOT, FLAG_SHIP_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE, ROLE_HANGAR_CAN_TAKE_1}, messages::dynamic_items as msg};
use crate::{client::filetime_now, dao::{self, items::{creation, dogma, loading::{Item, ItemQuery}, quantities::{self, QuantityChange}}}, db::{executor::Executor, outbox::Outbox}, sde::{Sde, dynamic_items::Mutation}, services::{db_err, fit::base_attribute, inventory, items::load_items, notify_error, CallError}};
use marshal::Value;

/// Server policy: bound ancestry traversal to prevent cycles and pathological nesting.
const MAX_CUSTODY_DEPTH: usize = 32;

#[derive(Clone)]
pub(super) struct Context { pub character: i64, pub station: Option<i64>, pub ship: Option<i64>, pub system: Option<i64> }

pub(super) fn draws(sde: &Sde, mutator_type: i32, mut sample: impl FnMut() -> f64) -> Result<BTreeMap<i32, f64>, CallError> {
    let mutation = sde.dynamic_items.get(mutator_type).ok_or_else(|| notify_error(msg::INCOMPATIBLE))?;
    Ok(mutation.attributes.keys().map(|&id| (id, sample())).collect())
}

pub(super) fn attributes(sde: &Sde, source_type: i32, mutation: &Mutation, draws: &BTreeMap<i32, f64>) -> Result<(BTreeMap<i32, f64>, BTreeMap<i32, f64>), CallError> {
    let data = sde.types.get(source_type).ok_or_else(|| notify_error(msg::INVALID_DATA))?;
    let mut bases: BTreeMap<_, _> = sde.types.attributes(source_type).collect();
    for id in [ATTR_MASS, ATTR_CAPACITY, ATTR_VOLUME, ATTR_RADIUS] {
        if let Some(value) = data.fit_attribute(id) { bases.entry(id).or_insert(value); }
    }
    // Damage is condition, not inherited base state.
    bases.remove(&ATTR_HULL_DAMAGE);
    let mut factors = BTreeMap::new();
    if draws.len() != mutation.attributes.len() { return Err(notify_error(msg::CHANGED)); }
    for (&id, range) in &mutation.attributes {
        let draw = *draws.get(&id).ok_or_else(|| notify_error(msg::CHANGED))?;
        if !draw.is_finite() || !(0.0..1.0).contains(&draw) { return Err(notify_error(msg::INVALID_DATA)); }
        let factor = range.min + draw * (range.max - range.min);
        let base = base_attribute(Some(data), sde.dogma.attribute(id), id);
        let value = base * factor;
        if !factor.is_finite() || !value.is_finite() { return Err(notify_error(msg::INVALID_DATA)); }
        bases.insert(id, value);
        factors.insert(id, factor);
    }
    if bases.values().any(|v| !v.is_finite()) { return Err(notify_error(msg::INVALID_DATA)); }
    Ok((bases, factors))
}

async fn item(ex: &(impl Executor + ?Sized), sde: &Sde, id: i64) -> Result<Option<Item>, CallError> {
    Ok(load_items(ex, sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next())
}

async fn custody(ex: &(impl Executor + ?Sized), sde: &Sde, context: &Context, input: &Item) -> Result<(), CallError> {
    let mut current = input.clone();
    let mut seen = HashSet::new();
    for _ in 0..MAX_CUSTODY_DEPTH {
        if !seen.insert(current.item_id) { return Err(notify_error(msg::INACCESSIBLE)); }
        if current.owner_id != context.character || inventory::is_fitting_flag(current.flag_id) || matches!(current.flag_id, FLAG_SHIP_HANGAR | FLAG_PILOT | FLAG_IMPLANT | FLAG_BOOSTER) { return Err(notify_error(msg::INACCESSIBLE)); }
        let parent = current.location_id;
        if context.station == Some(parent) {
            return if current.flag_id == FLAG_HANGAR { Ok(()) } else { Err(notify_error(msg::INACCESSIBLE)) };
        }
        if context.system == Some(parent) { return Err(notify_error(msg::DEPLOYED)); }
        if context.ship == Some(parent) {
            let ship = item(ex, sde, parent).await?.ok_or_else(|| notify_error(msg::INACCESSIBLE))?;
            let root = if let Some(station) = context.station { ship.location_id == station && ship.flag_id == FLAG_HANGAR } else { context.system == Some(ship.location_id) };
            return if ship.owner_id == context.character && ship.category_id == i64::from(CATEGORY_SHIP) && ship.singleton && root { Ok(()) } else { Err(notify_error(msg::INACCESSIBLE)) };
        }
        if let Some(corp) = dao::offices::owner(ex, parent).await.map_err(db_err)? {
            let station = context.station.ok_or_else(|| notify_error(msg::OFFICE))?;
            let division = current.flag_id.checked_sub(FLAG_CORP_SAG1).filter(|division| (0..CORP_HANGAR_DIVISIONS).contains(division)).ok_or_else(|| notify_error(msg::OFFICE))?;
            if dao::offices::at(ex, corp, station).await.map_err(db_err)? != Some(parent) || !crate::services::corps::permissions::has_at(ex, context.character, corp, station, ROLE_HANGAR_CAN_TAKE_1 << division).await? { return Err(notify_error(msg::OFFICE)); }
            return Ok(());
        }
        let container = item(ex, sde, parent).await?.ok_or_else(|| notify_error(msg::INACCESSIBLE))?;
        if current.flag_id != FLAG_HANGAR && current.flag_id != FLAG_CARGO { return Err(notify_error(msg::INACCESSIBLE)); }
        if container.owner_id != context.character || !i32::try_from(container.group_id).ok().is_some_and(|g| CARGO_CONTAINER_GROUPS.contains(&g)) || !container.singleton { return Err(notify_error(msg::INACCESSIBLE)); }
        current = container;
    }
    Err(notify_error(msg::INACCESSIBLE))
}

pub(super) async fn create(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox, context: &Context, expected_mutator: &Item, expected_source: &Item, draws: &BTreeMap<i32, f64>) -> Result<i64, CallError> {
    let mutator_id = expected_mutator.item_id;
    let source_id = expected_source.item_id;
    if mutator_id <= 0 || source_id <= 0 || mutator_id == source_id || context.character <= 0 { return Err(notify_error(msg::INVALID_ARGUMENT)); }
    let mutator = item(ex, sde, mutator_id).await?.ok_or_else(|| notify_error(msg::INVALID_ITEMS))?;
    let source = item(ex, sde, source_id).await?.ok_or_else(|| notify_error(msg::INVALID_ITEMS))?;
    if source.owner_id != context.character || mutator.owner_id != context.character || !source.singleton || source.quantity != 1 || mutator.quantity < 1 || context.ship == Some(source_id) { return Err(notify_error(msg::INVALID_ITEMS)); }
    if [&mutator, &source].iter().zip([expected_mutator, expected_source]).any(|(current, expected)|
        (current.type_id, current.owner_id, current.location_id, current.flag_id, current.quantity, current.singleton)
            != (expected.type_id, expected.owner_id, expected.location_id, expected.flag_id, expected.quantity, expected.singleton)) {
        return Err(notify_error(msg::CHANGED));
    }
    use crate::dao::items::parent_locks::{LockRequest, LockStrength};
    let source_lineage = dao::items::deliveries::ancestry(ex, source.location_id).await.map_err(db_err)?;
    let mutator_lineage = dao::items::deliveries::ancestry(ex, mutator.location_id).await.map_err(db_err)?;
    let mut requests = vec![
        LockRequest { item_id: source_id, strength: LockStrength::Update },
        LockRequest { item_id: mutator_id, strength: LockStrength::Update },
    ];
    requests.extend(inventory::destination::parent_requests(sde,
        source_lineage.iter().chain(mutator_lineage.iter()).map(|row| (row.item_id, row.type_id)),
        LockStrength::Update, LockStrength::Share)?);
    let mut parents = BTreeMap::new();
    for (location, lineage) in [(source.location_id, &source_lineage), (mutator.location_id, &mutator_lineage)] {
        if let Some(row) = lineage.iter().find(|row| row.item_id == location)
            .filter(|row| inventory::hold_storage::is_ordinary_container(sde, row.type_id)) {
            parents.insert(location, row.owner_id);
            requests.push(LockRequest { item_id: location, strength: LockStrength::Update });
        }
    }
    let locks = inventory::destination::lock_items(ex, sde, &requests, &[]).await?;
    if dao::items::deliveries::ancestry(ex, source.location_id).await.map_err(db_err)? != source_lineage
        || dao::items::deliveries::ancestry(ex, mutator.location_id).await.map_err(db_err)? != mutator_lineage
        || [expected_source, expected_mutator].iter().any(|item| !locks.row(item.item_id).is_some_and(|row|
            row.jump_clone_id.is_none() && row.type_id == item.type_id && row.owner_id == item.owner_id
                && row.location_id == item.location_id && row.flag_id == item.flag_id
                && row.quantity == item.quantity && row.singleton == item.singleton)) {
        return Err(notify_error(msg::CHANGED));
    }
    let ids = [source_id, mutator_id];
    if !dao::trades::reserved(ex, &ids).await.map_err(db_err)?.is_empty() || dao::industry::has_active_blueprint_job(ex, &ids).await.map_err(db_err)? { return Err(notify_error(msg::RESERVED)); }
    custody(ex, sde, context, &source).await?;
    custody(ex, sde, context, &mutator).await?;
    if dogma::overrides(ex, source_id).await.map_err(db_err)?.iter().any(|a| a.attribute_id == ATTR_HULL_DAMAGE && a.value > 0.0) { return Err(notify_error(msg::DAMAGED)); }
    let mutation = sde.dynamic_items.get(mutator.type_id).ok_or_else(|| notify_error(msg::INCOMPATIBLE))?;
    let result_type = mutation.result_type(source.type_id).ok_or_else(|| notify_error(msg::INCOMPATIBLE))?;
    let (bases, factors) = attributes(sde, source.type_id, mutation, draws)?;
    for (location, owner) in parents {
        let mut replaced = Vec::new();
        let mut additions = Vec::new();
        if source.location_id == location { replaced.push(source_id); }
        if mutator.location_id == location {
            replaced.push(mutator_id);
            if mutator.quantity > 1 {
                additions.push(inventory::hold_storage::Addition { type_id: mutator.type_id, singleton: mutator.singleton, quantity: mutator.quantity - 1 });
            }
            additions.push(inventory::hold_storage::Addition { type_id: result_type, singleton: true, quantity: 1 });
        }
        inventory::hold_storage::check_destination(ex, sde, owner, location, FLAG_CARGO, &additions,
            inventory::hold_storage::Replaced { items: &replaced, reloads: &[] }, None).await?;
    }
    let changes = BTreeMap::from([(source_id, QuantityChange::new(&source, None)), (mutator_id, QuantityChange::new(&mutator, (mutator.quantity > 1).then_some(mutator.quantity - 1))) ]);
    if !quantities::apply(ex, &changes).await.map_err(db_err)? { return Err(notify_error(msg::CHANGED)); }
    let result = creation::rookie_ship(ex, result_type, context.character, mutator.location_id, mutator.flag_id).await.map_err(db_err)?;
    for (&id, &value) in &bases {
        if !dogma::set_override(ex, result, id, value).await.map_err(db_err)? { return Err(notify_error(msg::CHANGED)); }
    }
    let metadata = dao::dynamic_items::MutationMetadata { item_id: result, source_item_id: source_id, mutator_item_id: mutator_id, source_type_id: source.type_id, mutator_type_id: mutator.type_id, result_type_id: result_type, creator_id: context.character, created_at: filetime_now(), modifiers: factors };
    dao::dynamic_items::insert(ex, &metadata).await.map_err(db_err)?;
    inventory::notifications::notify_item_removed(outbox, source);
    if mutator.quantity == 1 { inventory::notifications::notify_item_removed(outbox, mutator); } else {
        inventory::notifications::notify_item_change(ex, sde, outbox, mutator_id, vec![(IX_STACK_SIZE, Value::Int(mutator.quantity))]).await?;
    }
    inventory::notifications::notify_item_change(ex, sde, outbox, result, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
    Ok(result)
}
