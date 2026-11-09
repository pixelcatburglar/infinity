//! Physical inventory reachability for eve/client/script/environment/invControllers.py station, ship-bay and
//! celestial-container inventories. Ancestry comes from `game.items`, type data
//! from the SDE, and sampled distances from the system actor. Shared ship bays
//! use the current owner-bound policy; POS tower-control inventories are not implemented.
//! Relocations of shared containers are serialized by the economy lock; system-driven deletion or ownership changes are caught by the item's guarded write.

use infinity_core::consts::{CARGO_CONTAINER_GROUPS, CATEGORY_SHIP, GROUP_WRECK, FLAG_SHIP_HANGAR, FLAG_FLEET_HANGAR, MAX_CARGO_CONTAINER_TRANSFER_DISTANCE};
use infinity_core::consts::dogma::{ATTR_SHIP_MAINTENANCE_BAY_CAPACITY, ATTR_FLEET_HANGAR_CAPACITY};
use crate::db::executor::Executor;
use infinity_core::messages;
use crate::sde::Sde;
use crate::services::{db_err, notify_error, CallError};
use crate::db::Db;
use crate::services::identity::Identity;
use crate::space::World;
use std::collections::HashMap;
use std::sync::Arc;

/// dogma/const.py attributeMaxOperationalDistance.
const ATTR_MAX_OPERATIONAL_DISTANCE: i32 = 715;

#[derive(Clone, Copy)]
pub(in crate::services) struct Endpoint { pub location: i64, pub flag: i32 }

#[derive(Clone)]
pub(in crate::services) struct Reachability {
    character: Option<i64>, station: Option<i64>, ship: Option<i64>,
    system: Option<i64>, corporation: Option<i64>, capture: Arc<Capture>,
}

#[derive(Clone, Copy)]
struct Sample { gap: f64, visible: bool }

struct Capture {
    gaps: HashMap<i64, Sample>,
    ancestry: HashMap<i64, Vec<crate::dao::items::deliveries::LineageRow>>,
}

/// Fresh lineages and admission decisions for a single batch; a lineage alone is not an access grant.
pub(in crate::services) struct CheckedEndpoints {
    pub admitted: Vec<bool>,
    lineages: HashMap<i64, (bool, HashMap<i64, crate::dao::items::deliveries::Ancestor>)>,
    full_lineages: HashMap<i64, Vec<crate::dao::items::deliveries::LineageRow>>,
}

impl CheckedEndpoints {
    pub(in crate::services) fn lineage(&self, location: i64) -> Option<&HashMap<i64, crate::dao::items::deliveries::Ancestor>> {
        self.lineages.get(&location).and_then(|(valid, rows)| valid.then_some(rows))
    }
    pub(in crate::services) fn full_lineage(&self, location: i64) -> Option<&[crate::dao::items::deliveries::LineageRow]> {
        self.lineages.get(&location).filter(|(valid, _)| *valid)?;
        self.full_lineages.get(&location).map(Vec::as_slice)
    }
}

fn in_range(gap: Option<f64>, operational_distance: Option<f64>) -> bool {
    let limit = operational_distance.unwrap_or(MAX_CARGO_CONTAINER_TRANSFER_DISTANCE);
    limit.is_finite() && gap.is_some_and(|gap| gap.is_finite() && gap <= limit)
}

/// Evaluate an already-sampled physical root; visibility and ancestry remain caller responsibilities.
pub(in crate::services) fn root_in_range(sde: &Sde, type_id: i32, flag: i32, active_ship: bool, gap: Option<f64>) -> bool {
    let Some(ty) = sde.types.get(type_id) else { return false };
    if ty.category_id == Some(CATEGORY_SHIP) {
        if active_ship { return gap.is_some(); }
        let attribute = match flag {
            FLAG_SHIP_HANGAR => ATTR_SHIP_MAINTENANCE_BAY_CAPACITY,
            FLAG_FLEET_HANGAR => ATTR_FLEET_HANGAR_CAPACITY,
            _ => return false,
        };
        if !ty.attribute(attribute).is_some_and(|capacity| capacity.is_finite() && capacity > 0.0) { return false; }
        in_range(gap, Some(MAX_CARGO_CONTAINER_TRANSFER_DISTANCE))
    } else if ty.group_id.is_some_and(|g| g == GROUP_WRECK || CARGO_CONTAINER_GROUPS.contains(&g)) {
        in_range(gap, ty.attribute(ATTR_MAX_OPERATIONAL_DISTANCE))
    } else { false }
}

impl Reachability {
    pub(in crate::services) async fn capture(identity: &Identity, db: &Db, world: &World, space_system: Option<i64>, endpoints: &[Endpoint]) -> Result<Self, CallError> {
        let character = (identity.char_id != 0).then_some(identity.char_id);
        let station = identity.station_id;
        let ship = identity.ship_id;
        let system = space_system;
        let corporation = (identity.corp_id != 0).then_some(identity.corp_id);
        let mut candidates = HashMap::new();
        let mut ancestry = HashMap::new();
        let mut ids: Vec<_> = endpoints.iter().map(|endpoint| endpoint.location).collect();
        ids.sort_unstable();
        ids.dedup();
        for &id in &ids {
            candidates.insert(id, false);
            ancestry.insert(id, Vec::new());
        }
        if !ids.is_empty() {
            for row in crate::dao::items::deliveries::ancestry_many(db, &ids).await.map_err(db_err)? {
                candidates.insert(row.ancestor.item_id, character != Some(row.ancestor.owner_id));
                ancestry.get_mut(&row.origin).expect("seeded lineage").push(row);
            }
        }
        let mut gaps = HashMap::new();
        if station.is_none() {
            if let (Some(system), Some(ship), Some(character)) = (system, ship, character) {
                gaps = world.with(system, move |s| {
                    let mut gaps = HashMap::new();
                    if s.pilot_ball(character).is_none_or(|pilot| pilot.id != ship) { return gaps; }
                    let (Some(pilot), Some(pilot_pos)) = (s.ball(ship), s.sample_position(ship)) else { return gaps };
                    if !pilot.radius.is_finite() || pilot.radius < 0.0 || !pilot_pos.iter().all(|v| v.is_finite()) { return gaps; }
                    for (id, foreign) in candidates {
                        let (Some(root), Some(root_pos)) = (s.ball(id), s.sample_position(id)) else { continue };
                        if !root.radius.is_finite() || root.radius < 0.0 || !root_pos.iter().all(|v| v.is_finite()) { continue; }
                        let gap = crate::space::dist(pilot_pos, root_pos) - f64::from(pilot.radius) - f64::from(root.radius);
                        if gap.is_finite() {
                            gaps.insert(id, Sample { gap, visible: !foreign || s.visible_to(character, id) });
                        }
                    }
                    gaps
                }).await.unwrap_or_default();
            }
        }
        Ok(Self { character, station, ship, system, corporation, capture: Arc::new(Capture { gaps, ancestry }) })
    }

    pub(in crate::services) async fn check(&self, ex: &(impl Executor + ?Sized), sde: &Sde, endpoint: Endpoint) -> Result<(), CallError> {
        if self.inspect_many(ex, sde, &[endpoint]).await?.admitted[0] { Ok(()) }
        else { Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)) }
    }

    pub(in crate::services) async fn check_many(&self, ex: &(impl Executor + ?Sized), sde: &Sde, endpoints: &[Endpoint]) -> Result<Vec<bool>, CallError> {
        Ok(self.inspect_many(ex, sde, endpoints).await?.admitted)
    }

    pub(in crate::services) async fn inspect_many(&self, ex: &(impl Executor + ?Sized), sde: &Sde, endpoints: &[Endpoint]) -> Result<CheckedEndpoints, CallError> {
        let mut lineages = HashMap::new();
        let mut full_lineages = HashMap::new();
        let mut hulls = std::collections::HashSet::new();
        let mut ids: Vec<_> = endpoints.iter().map(|endpoint| endpoint.location).collect();
        ids.sort_unstable();
        ids.dedup();
        let mut batches: HashMap<_, Vec<_>> = ids.iter().map(|&id| (id, Vec::new())).collect();
        if !ids.is_empty() {
            for row in crate::dao::items::deliveries::ancestry_many(ex, &ids).await.map_err(db_err)? {
                batches.get_mut(&row.origin).expect("seeded lineage").push(row);
            }
        }
        let mut offices = HashMap::new();
        for (id, rows) in batches {
            let valid = self.capture.ancestry.get(&id) == Some(&rows);
            if valid {
                for row in &rows {
                    if self.foreign_ship(sde, &row.ancestor) { hulls.insert((row.ancestor.item_id, row.ancestor.owner_id)); }
                    if let Some(office) = &row.office { offices.insert(row.ancestor.item_id, office.clone()); }
                }
            }
            if valid { full_lineages.insert(id, rows.clone()); }
            lineages.insert(id, (valid, rows.into_iter().map(|row| (row.ancestor.item_id, row.ancestor)).collect::<HashMap<_, _>>()));
        }
        let hulls: Vec<_> = hulls.into_iter().collect();
        let permissions = super::bay_access::permissions_many(ex, self.character.unwrap_or(0), &hulls).await?;
        let mut admitted = Vec::with_capacity(endpoints.len());
        for &endpoint in endpoints {
            let (valid, rows) = &lineages[&endpoint.location];
            admitted.push(if *valid { self.check_endpoint(sde, endpoint, rows, &offices, &permissions) } else { false });
        }
        Ok(CheckedEndpoints { admitted, lineages, full_lineages })
    }

    fn foreign_ship(&self, sde: &Sde, row: &crate::dao::items::deliveries::Ancestor) -> bool {
        self.station.is_none() && self.character.is_some_and(|character| character != row.owner_id)
            && row.singleton && row.quantity == 1 && sde.types.get(row.type_id).is_some_and(|ty| ty.category_id == Some(CATEGORY_SHIP))
    }

    fn check_endpoint(&self, sde: &Sde, endpoint: Endpoint,
        rows: &HashMap<i64, crate::dao::items::deliveries::Ancestor>,
        offices: &HashMap<i64, crate::dao::items::deliveries::OfficeMetadata>,
        permissions: &HashMap<(i64, i64), super::bay_access::Permissions>) -> bool {
        let mut id = endpoint.location;
        let mut flag = endpoint.flag;
        for _ in 0..=rows.len() {
            if self.station == Some(id) && sde.map.station(id).is_some() { return true; }
            if let Some(office) = offices.get(&id) {
                let (Some(station), Some(corp)) = (self.station, self.corporation) else { return false };
                return office.corporation_id == corp && office.station_id == station;
            }
            let Some(row) = rows.get(&id) else { return false };
            let Some(ty) = sde.types.get(row.type_id) else { return false };
            let is_ship = ty.category_id == Some(CATEGORY_SHIP);
            let is_cargo = ty.group_id.is_some_and(|g| g == GROUP_WRECK || CARGO_CONTAINER_GROUPS.contains(&g));
            if (!is_ship && !is_cargo) || !row.singleton || (is_ship && row.quantity != 1) { return false; }
            if is_ship {
                if self.station.is_some() && self.character != Some(row.owner_id) { return false; }
                let Some(character) = self.character else { return false };
                let key = (id, row.owner_id);
                let access = permissions.get(&key).copied().unwrap_or_else(||
                    super::bay_access::permissions_from(character, row.owner_id, None, Default::default()));
                if !access.allows(flag) { return false; }
            }
            if self.station.is_none() && self.system == Some(row.location_id) {
                let sample = self.capture.gaps.get(&id);
                if is_ship && self.character != Some(row.owner_id) && !sample.is_some_and(|sample| sample.visible) { return false; }
                return root_in_range(sde, row.type_id, flag, self.ship == Some(id), sample.map(|sample| sample.gap));
            }
            flag = row.flag_id;
            id = row.location_id;
        }
        false
    }
}

#[cfg(test)] mod tests;
