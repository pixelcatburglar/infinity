//! Physical inventory reachability for eve/client/script/environment/invControllers.py station, ship-bay and
//! celestial-container inventories. Ancestry comes from `game.items`, type data
//! from the SDE, and sampled distances from the system actor. Foreign fleet-bay
//! permissions and POS tower-control inventories are not implemented.
//! Relocations of shared containers are serialized by the economy lock; system-driven deletion or ownership changes are caught by the item's guarded write.

use super::*;
use crate::consts::{CARGO_CONTAINER_GROUPS, GROUP_WRECK};
use crate::db::Db;
use crate::services::identity::Identity;
use crate::space::World;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// appConst.py maxCargoContainerTransferDistance, metres.
const MAX_CARGO_CONTAINER_TRANSFER_DISTANCE: f64 = 2500.0;
/// dogma/const.py attributeMaxOperationalDistance.
const ATTR_MAX_OPERATIONAL_DISTANCE: i32 = 715;
/// inventorycommon/const.py flagShipHangar.
const FLAG_SHIP_HANGAR: i32 = 90;
/// inventorycommon/const.py flagFleetHangar.
const FLAG_FLEET_HANGAR: i32 = 155;
/// dogma/const.py attributeShipMaintenanceBayCapacity.
const ATTR_SHIP_MAINTENANCE_BAY_CAPACITY: i32 = 908;
/// dogma/const.py attributeFleetHangarCapacity.
const ATTR_FLEET_HANGAR_CAPACITY: i32 = 912;

#[derive(Clone, Copy)]
pub(super) struct Endpoint { pub location: i64, pub flag: i32 }

#[derive(Clone)]
pub(super) struct Reachability {
    character: Option<i64>, station: Option<i64>, ship: Option<i64>,
    system: Option<i64>, corporation: Option<i64>, capture: Arc<Capture>,
}

struct Capture {
    gaps: HashMap<i64, f64>,
    ancestry: HashMap<i64, Vec<crate::dao::items::deliveries::Ancestor>>,
}

fn in_range(gap: Option<f64>, operational_distance: Option<f64>) -> bool {
    let limit = operational_distance.unwrap_or(MAX_CARGO_CONTAINER_TRANSFER_DISTANCE);
    limit.is_finite() && gap.is_some_and(|gap| gap.is_finite() && gap <= limit)
}

impl Reachability {
    pub(super) async fn capture(identity: &Identity, db: &Db, world: &World, space_system: Option<i64>, endpoints: &[Endpoint]) -> Result<Self, CallError> {
        let character = (identity.char_id != 0).then_some(identity.char_id);
        let station = identity.station_id;
        let ship = identity.ship_id;
        let system = space_system;
        let corporation = (identity.corp_id != 0).then_some(identity.corp_id);
        let mut candidates = HashSet::new();
        let mut ancestry = HashMap::new();
        for endpoint in endpoints {
            if ancestry.contains_key(&endpoint.location) { continue; }
            let rows = crate::dao::items::deliveries::ancestry(db, endpoint.location).await.map_err(db_err)?;
            candidates.insert(endpoint.location);
            candidates.extend(rows.iter().map(|row| row.item_id));
            ancestry.insert(endpoint.location, rows);
        }
        let mut gaps = HashMap::new();
        if station.is_none() {
            if let (Some(system), Some(ship), Some(character)) = (system, ship, character) {
                gaps = world.with(system, move |s| {
                    let mut gaps = HashMap::new();
                    if s.pilot_ball(character).is_none_or(|pilot| pilot.id != ship) { return gaps; }
                    let (Some(pilot), Some(pilot_pos)) = (s.ball(ship), s.sample_position(ship)) else { return gaps };
                    if !pilot.radius.is_finite() || pilot.radius < 0.0 || !pilot_pos.iter().all(|v| v.is_finite()) { return gaps; }
                    for id in candidates {
                        let (Some(root), Some(root_pos)) = (s.ball(id), s.sample_position(id)) else { continue };
                        if !root.radius.is_finite() || root.radius < 0.0 || !root_pos.iter().all(|v| v.is_finite()) { continue; }
                        let gap = crate::space::dist(pilot_pos, root_pos) - f64::from(pilot.radius) - f64::from(root.radius);
                        if gap.is_finite() { gaps.insert(id, gap); }
                    }
                    gaps
                }).await.unwrap_or_default();
            }
        }
        Ok(Self { character, station, ship, system, corporation, capture: Arc::new(Capture { gaps, ancestry }) })
    }

    pub(super) async fn check(&self, ex: &(impl Executor + ?Sized), sde: &Sde, endpoint: Endpoint) -> Result<(), CallError> {
        let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
        let rows = crate::dao::items::deliveries::ancestry(ex, endpoint.location).await.map_err(db_err)?;
        if self.capture.ancestry.get(&endpoint.location) != Some(&rows) { return Err(refuse()); }
        let mut rows: HashMap<_, _> = rows.into_iter().map(|row| (row.item_id, row)).collect();
        let mut id = endpoint.location;
        let mut flag = endpoint.flag;
        loop {
            if self.station == Some(id) && sde.map.station(id).is_some() { return Ok(()); }
            if let Some(owner) = super::super::offices::office_owner(ex, id).await? {
                let (Some(station), Some(corp)) = (self.station, self.corporation) else { return Err(refuse()) };
                return if owner == corp && super::super::offices::office_at(ex, corp, station).await? == Some(id) { Ok(()) } else { Err(refuse()) };
            }
            let Some(row) = rows.remove(&id) else { return Err(refuse()) };
            let Some(ty) = sde.types.get(row.type_id) else { return Err(refuse()) };
            let is_ship = ty.category_id == Some(CATEGORY_SHIP);
            let is_cargo = ty.group_id.is_some_and(|g| g == GROUP_WRECK || CARGO_CONTAINER_GROUPS.contains(&g));
            if (!is_ship && !is_cargo) || !row.singleton { return Err(refuse()); }
            if is_ship && self.character != Some(row.owner_id) { return Err(refuse()); }
            if self.station.is_none() && self.system == Some(row.location_id) {
                let gap = self.capture.gaps.get(&id).copied();
                if is_ship && self.ship == Some(id) {
                    return if gap.is_some() { Ok(()) } else { Err(refuse()) };
                }
                let limit = if is_ship {
                    let attribute = match flag {
                        FLAG_SHIP_HANGAR => ATTR_SHIP_MAINTENANCE_BAY_CAPACITY,
                        FLAG_FLEET_HANGAR => ATTR_FLEET_HANGAR_CAPACITY,
                        _ => return Err(refuse()),
                    };
                    if !ty.attribute(attribute).is_some_and(|capacity| capacity.is_finite() && capacity > 0.0) { return Err(refuse()); }
                    Some(MAX_CARGO_CONTAINER_TRANSFER_DISTANCE)
                } else {
                    ty.attribute(ATTR_MAX_OPERATIONAL_DISTANCE)
                };
                return if in_range(gap, limit) { Ok(()) } else { Err(refuse()) };
            }
            flag = row.flag_id;
            id = row.location_id;
        }
    }
}

#[cfg(test)] mod tests;
