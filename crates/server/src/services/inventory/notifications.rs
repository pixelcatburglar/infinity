//! `OnItemChange` delivery for `eve/client/script/environment/invCache.py`.
//! Corporate recipients use custody and role records; shared-bay audiences combine
//! sampled nearby pilots with fresh owner-bound configuration and affiliations.
//! Subscribers and durable notifications are not tracked here.
//! Market item changes use the same current custody-aware corporate audience; pushes
//! follow commit and are not a durable notification log. Change quantities are
//! projected to the client display limit; stored quantities stay exact.

use super::access::{corp_hangar_role, corporate_custody, custody_station};
use infinity_core::consts::{IX_LOCATION_ID, IX_QUANTITY, IX_STACK_SIZE, LOCATION_JUNKYARD};
use crate::dao::items::loading::{Item, ItemQuery};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::sde::Sde;
use crate::services::{db_err, items::load_items, CallError};
use marshal::Value;
use std::collections::{HashMap, HashSet};
use super::reachability::Endpoint;
use crate::dao::items::deliveries::Ancestor;
use crate::db::Db;
use crate::services::identity::Identity;
use crate::space::World;

/// Pretransaction lineage and compact visible, in-range pilot samples.
#[derive(Clone)]
pub(in crate::services) struct Audience {
    endpoints: Vec<(Endpoint, Vec<Ancestor>)>,
    candidates: HashMap<i64, Vec<(i64, i64, f64)>>,
    system: Option<i64>,
}

/// Per-endpoint recipients resolved against fresh owner-bound bay policy.
pub(in crate::services) struct ResolvedAudience {
    viewers: HashMap<(i64, i32), HashSet<i64>>,
}

impl Audience {
    pub(in crate::services) async fn capture(
        _identity: &Identity, db: &Db, world: &World, system: Option<i64>, endpoints: &[Endpoint],
    ) -> Result<Self, CallError> {
        let mut rows = Vec::with_capacity(endpoints.len());
        let mut roots = HashMap::<i64, (i32, i64, Vec<i32>)>::new();
        let mut lineages = HashMap::<i64, Vec<Ancestor>>::new();
        let mut seen = HashSet::new();
        for &endpoint in endpoints {
            if !seen.insert((endpoint.location, endpoint.flag)) { continue; }
            let ancestry = if let Some(rows) = lineages.get(&endpoint.location) { rows.clone() } else {
                let rows = crate::dao::items::deliveries::ancestry(db, endpoint.location).await.map_err(db_err)?;
                lineages.insert(endpoint.location, rows.clone());
                rows
            };
            if !super::movement::shared_lineage(world.sde(), endpoint, ancestry.len(), |id| ancestry.iter().find(|row| row.item_id == id)) { continue; }
            if let Some(root) = ancestry.iter().find(|row| Some(row.location_id) == system) {
                let mut id = endpoint.location;
                let mut flag = endpoint.flag;
                for _ in 0..ancestry.len() {
                    let Some(row) = ancestry.iter().find(|row| row.item_id == id) else { break };
                    if id == root.item_id {
                        let flags = &mut roots.entry(id).or_insert_with(|| (row.type_id, row.owner_id, Vec::new())).2;
                        if !flags.contains(&flag) { flags.push(flag); }
                        break;
                    }
                    id = row.location_id;
                    flag = row.flag_id;
                }
            }
            rows.push((endpoint, ancestry));
        }
        let sde = world.sde().clone();
        let candidates = if let Some(system) = system.filter(|_| !roots.is_empty()) {
            world.with(system, move |space| {
                let mut candidates = HashMap::new();
                for (root, (type_id, owner, flags)) in roots {
                    let (Some(target), Some(target_pos)) = (space.ball(root), space.sample_position(root)) else { continue };
                    if !target.radius.is_finite() || target.radius < 0.0 || !target_pos.iter().all(|v| v.is_finite()) { continue; }
                    let mut viewers = Vec::new();
                    for pilot in space.pilots() {
                        let Some(character) = pilot.char_id else { continue };
                        if space.pilot_ball(character).is_none_or(|current| current.id != pilot.id) { continue; }
                        let Some(pos) = space.sample_position(pilot.id) else { continue };
                        let foreign_ship = character != owner && sde.types.get(type_id).is_some_and(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP));
                        if !pilot.radius.is_finite() || pilot.radius < 0.0 || !pos.iter().all(|v| v.is_finite())
                            || (foreign_ship && !space.visible_to(character, root)) { continue; }
                        let gap = crate::space::dist(pos, target_pos) - f64::from(pilot.radius) - f64::from(target.radius);
                        if gap.is_finite() && flags.iter().any(|flag|
                            super::reachability::root_in_range(&sde, type_id, *flag, pilot.id == root, Some(gap))) {
                            viewers.push((character, pilot.id, gap));
                        }
                    }
                    candidates.insert(root, viewers);
                }
                candidates
            }).await.unwrap_or_default()
        } else { HashMap::new() };
        Ok(Self { endpoints: rows, candidates, system })
    }

    pub(in crate::services) async fn resolve(
        &self, ex: &(impl Executor + ?Sized), sde: &Sde,
    ) -> Result<ResolvedAudience, CallError> {
        let mut hulls = HashMap::new();
        let mut people = HashSet::new();
        let mut checked = HashSet::new();
        for (endpoint, captured) in &self.endpoints {
            if checked.insert(endpoint.location) && crate::dao::items::deliveries::ancestry(ex, endpoint.location).await.map_err(db_err)? != *captured {
                return Err(crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
            }
            for row in captured {
                if sde.types.get(row.type_id).is_some_and(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)) {
                    hulls.entry(row.item_id).or_insert(row.owner_id);
                    people.insert(row.owner_id);
                }
            }
        }
        if self.endpoints.is_empty() { return Ok(ResolvedAudience { viewers: HashMap::new() }); }
        for viewers in self.candidates.values() { people.extend(viewers.iter().map(|&(character, _, _)| character)); }
        let people: Vec<_> = people.into_iter().collect();
        let orgs = crate::dao::characters::members::organizations(ex, &people).await.map_err(db_err)?;
        let mut settings = HashMap::new();
        for &ship in hulls.keys() {
            settings.insert(ship, crate::dao::items::ship_configuration::get(ex, ship).await.map_err(db_err)?);
        }
        let mut viewers = HashMap::new();
        let mut grants = HashMap::new();
        for (endpoint, rows) in &self.endpoints {
            let Some(root) = rows.iter().find(|row| Some(row.location_id) == self.system) else { continue };
            let Some(candidates) = self.candidates.get(&root.item_id) else { continue };
            let mut allowed = HashSet::new();
            for &(character, pilot_ship, gap) in candidates {
                let mut location = endpoint.location;
                let mut flag = endpoint.flag;
                let mut valid = false;
                let mut saw_ship = false;
                for _ in 0..rows.len() {
                    let Some(row) = rows.iter().find(|row| row.item_id == location) else { break };
                    let Some(ty) = sde.types.get(row.type_id) else { break };
                    let is_ship = ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP);
                    let is_container = ty.group_id.is_some_and(|group| group == infinity_core::consts::GROUP_WRECK || infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&group));
                    if !row.singleton || (is_ship && row.quantity != 1) || (!is_ship && !is_container) { break; }
                    if is_ship {
                        saw_ship = true;
                        let permission = *grants.entry((character, row.item_id, row.owner_id)).or_insert_with(|| {
                            let corp = orgs.get(&character).copied().filter(|id| *id > 0);
                            let affiliation = super::bay_access::Affiliation {
                                fleet: character != row.owner_id && crate::services::fleets::same_fleet(character, row.owner_id),
                                corporation: corp.is_some() && corp == orgs.get(&row.owner_id).copied(),
                            };
                            super::bay_access::permissions_from(character, row.owner_id,
                                settings.get(&row.item_id).and_then(Option::as_ref), affiliation)
                        });
                        if !permission.allows(flag) { break; }
                    }
                    if row.item_id == root.item_id {
                        valid = saw_ship && super::reachability::root_in_range(sde, row.type_id, flag, pilot_ship == row.item_id, Some(gap));
                        break;
                    }
                    location = row.location_id;
                    flag = row.flag_id;
                }
                if valid { allowed.insert(character); }
            }
            viewers.insert((endpoint.location, endpoint.flag), allowed);
        }
        Ok(ResolvedAudience { viewers })
    }
}

impl ResolvedAudience {
    /// The movement core already queues the actor's direct change. Other viewers
    /// receive the destination row or a source-only junkyard removal. A previous
    /// owner may receive the updated quantity of their unchanged source row.
    pub(in crate::services) fn item_change<X: Send>(
        &self, outbox: &mut Outbox<X>, old: &Item, new: &Item, change: Vec<(i64, Value)>, actor: i64,
    ) {
        let source = self.viewers.get(&(old.location_id, old.flag_id));
        let destination = self.viewers.get(&(new.location_id, new.flag_id));
        let mut recipients = HashSet::new();
        if let Some(source) = source { recipients.extend(source.iter().copied()); }
        if let Some(destination) = destination { recipients.extend(destination.iter().copied()); }
        if old.location_id != LOCATION_JUNKYARD && old.owner_id != actor { recipients.insert(old.owner_id); }
        let same_owned_row = old.item_id == new.item_id && old.owner_id == new.owner_id
            && old.location_id == new.location_id && old.flag_id == new.flag_id
            && old.type_id == new.type_id && old.singleton == new.singleton
            && old.custom_info == new.custom_info;
        for character in recipients {
            if character == actor { continue; }
            if destination.is_some_and(|viewers| viewers.contains(&character))
                || (character == old.owner_id && same_owned_row) {
                let change = item_change_dict(change.iter().cloned());
                outbox.notify_pilot(character, "OnItemChange", vec![new.row(), change, Value::None]);
            } else {
                let gone = Item { location_id: LOCATION_JUNKYARD, ..old.clone() };
                let change = Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old.location_id))]);
                outbox.notify_pilot(character, "OnItemChange", vec![gone.row(), change, Value::None]);
            }
        }
    }
}

/// Route a direct notification to the target when the caller is another pilot.
pub(in crate::services) fn route_target(notification: crate::db::outbox::Notification, target: i64, current: Option<i64>) -> crate::db::outbox::Notification {
    use crate::db::outbox::Notification;
    match notification {
        Notification::DirectClient { event, args } if current != Some(target) => Notification::Pilot { char_id: target, event, args },
        notification => notification,
    }
}

/// `OnItemChange` change dict: quantity columns project to the client display
/// limit (invCache and inventorysvc rebuild the old row from the changed keys);
/// every other key, value type and the order are preserved.
pub(in crate::services) fn item_change_dict(change: impl IntoIterator<Item = (i64, Value)>) -> Value {
    Value::Dict(change.into_iter().map(|(key, value)| {
        let value = match value {
            Value::Int(quantity) if key == IX_QUANTITY || key == IX_STACK_SIZE => {
                Value::Int(super::super::dogma::info::cap_client_quantity(quantity))
            }
            value => value,
        };
        (Value::Int(key), value)
    }).collect())
}

/// `OnItemChange(item, change, location)` for `item_id` as it is now in the database.
pub(in crate::services) async fn notify_item_change<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, item_id: i64, change: Vec<(i64, Value)>) -> Result<(), CallError> {
    if let Some(item) = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next() {
        let change = item_change_dict(change);
        outbox.notify_client("OnItemChange", vec![item.row(), change, Value::None]);
    }
    Ok(())
}

/// Current station-scoped viewers of the item's corporate custody.
pub(in crate::services) async fn corp_item_viewers(ex: &(impl Executor + ?Sized), item: &Item) -> Result<Vec<i64>, CallError> {
    let custody = if let Some(corp) = crate::services::offices::office_owner(ex, item.location_id).await? {
        Some((corp, item.location_id, item.flag_id))
    } else if item.flag_id == infinity_core::consts::FLAG_CORP_DELIVERIES
        && crate::services::corps::is_player_corp(item.owner_id) {
        Some((item.owner_id, item.location_id, item.flag_id))
    } else {
        corporate_custody(ex, None, item.location_id).await?
    };
    let Some((corp, location, flag)) = custody else { return Ok(Vec::new()) };
    let Some(station) = custody_station(ex, location, flag).await? else { return Ok(Vec::new()) };
    let Some(role) = corp_hangar_role(flag, false) else { return Ok(Vec::new()) };
    let ids = crate::dao::characters::members::active_ids(ex, corp).await.map_err(db_err)?;
    let authorities = crate::dao::characters::roles::authorities(ex, &ids).await.map_err(db_err)?;
    Ok(authorities.into_iter().filter(|authority|
        crate::services::corps::permissions::allows_at(authority, corp, station, role)
    ).map(|authority| authority.character).collect())
}

/// Queue a provided corporate row to members authorized to view its division.
pub(in crate::services) async fn notify_corp_item_change<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item: &Item, change: Vec<(i64, Value)>) -> Result<(), CallError> {
    let mut source = item.clone();
    if item.location_id == LOCATION_JUNKYARD {
        source.location_id = change.iter().find_map(|(key, value)| (*key == IX_LOCATION_ID).then(|| value.as_int()).flatten()).unwrap_or(item.location_id);
    }
    let viewers = corp_item_viewers(ex, &source).await?;
    let change = item_change_dict(change);
    for character in viewers {
        outbox.notify_pilot(character, "OnItemChange", vec![item.row(), change.clone(), Value::None]);
    }
    Ok(())
}

/// Queue the standard junkyard notification for a removed item.
pub(in crate::services) fn notify_item_removed<X: Send>(outbox: &mut Outbox<X>, item: Item) {
    let old_location = item.location_id;
    let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
    let change = Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old_location))]);
    outbox.notify_client("OnItemChange", vec![gone.row(), change, Value::None]);
}

/// Queue deletion to corporate viewers using the item's captured pre-delete location.
pub(in crate::services) async fn notify_corp_item_removed<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item: &Item,
) -> Result<(), CallError> {
    let old = item.location_id;
    let gone = Item { location_id: LOCATION_JUNKYARD, ..item.clone() };
    notify_corp_item_change(ex, outbox, &gone, vec![(IX_LOCATION_ID, Value::Int(old))]).await
}

#[cfg(test)]
mod tests;
