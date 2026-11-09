//! Immutable map and station records and native station standing restrictions loaded at startup.
//! Consumers include `eve/client/script/remote/michelle.py:491-532`,
//! `eve/client/script/parklife/autopilot.py:260-261,329-332`, and
//! `evestations/standingsrestriction.py`. Standing skill bonuses and map presentation
//! are outside this dataset.

use std::collections::HashMap;
use crate::{dao::sde::map as dao, db::Db};

/// `appConst.factionsWhoseStandingsAreNotAffectedBySkillBonuses`; see
/// `standingUtil.GetStandingBonusSkillTypeID` for the skill exemption.
const STANDING_SKILL_EXEMPT_FACTIONS: &[i64] = &[500024, 500025, 500026, 500027];

pub(crate) struct SolarSystem {
    pub solar_system_id: i64,
    pub name: Option<String>,
    pub constellation_id: Option<i64>,
    pub region_id: Option<i64>,
    pub faction_id: Option<i64>,
    pub security: Option<f64>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
}
pub(crate) struct Region {
    pub region_id: i64,
    pub wormhole_class_id: Option<i64>,
    pub nebula_id: Option<i64>,
}
pub(crate) struct Constellation {
    pub constellation_id: i64,
    pub wormhole_class_id: Option<i64>,
}
pub(crate) struct Stargate {
    pub stargate_id: i64,
    pub solar_system_id: Option<i64>,
    pub dest_stargate_id: Option<i64>,
    pub dest_solar_system_id: Option<i64>,
}
pub(crate) struct Station {
    pub station_id: i64,
    pub type_id: Option<i64>,
    pub owner_id: Option<i64>,
    pub solar_system_id: Option<i64>,
    pub constellation_id: Option<i64>,
    pub region_id: Option<i64>,
    pub operation_id: Option<i64>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
    pub security: Option<f64>,
}
pub(crate) struct Celestial {
    pub item_id: i64,
    pub type_id: Option<i64>,
    pub solar_system_id: Option<i64>,
    pub group_id: Option<i64>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
    pub radius: Option<f64>,
    pub celestial_index: Option<i64>,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct Map {
    systems: Vec<SolarSystem>,
    regions: Vec<Region>,
    constellations: Vec<Constellation>,
    stargates: Vec<Stargate>,
    stations: Vec<Station>,
    celestials: Vec<Celestial>,
    system_index: HashMap<i64, usize>,
    region_index: HashMap<i64, usize>,
    constellation_index: HashMap<i64, usize>,
    stargate_index: HashMap<i64, usize>,
    station_index: HashMap<i64, usize>,
    celestial_index: HashMap<i64, usize>,
    system_celestials: HashMap<i64, Vec<usize>>,
    owner_stations: HashMap<i64, Vec<usize>>,
    jumps: HashMap<i64, Vec<i64>>,
    station_standing_restrictions: HashMap<i64, HashMap<i32, f64>>,
}

impl Map {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let (systems, regions, constellations, stargates, stations, celestials, jumps, restrictions) = tokio::try_join!(
            dao::systems(db), dao::regions(db), dao::constellations(db), dao::stargates(db),
            dao::stations(db), dao::celestials(db), dao::jumps(db), dao::station_standing_restrictions(db),
        )?;
        let system_index = systems.iter().enumerate().map(|(i, r)| (r.solar_system_id, i)).collect();
        let region_index = regions.iter().enumerate().map(|(i, r)| (r.region_id, i)).collect();
        let constellation_index = constellations.iter().enumerate().map(|(i, r)| (r.constellation_id, i)).collect();
        let stargate_index = stargates.iter().enumerate().map(|(i, r)| (r.stargate_id, i)).collect();
        let station_index = stations.iter().enumerate().map(|(i, r)| (r.station_id, i)).collect();
        let celestial_index = celestials.iter().enumerate().map(|(i, r)| (r.item_id, i)).collect();
        let mut system_celestials: HashMap<i64, Vec<usize>> = HashMap::new();
        for (i, row) in celestials.iter().enumerate() {
            if let Some(system) = row.solar_system_id { system_celestials.entry(system).or_default().push(i); }
        }
        let mut owner_stations: HashMap<i64, Vec<usize>> = HashMap::new();
        for (i, row) in stations.iter().enumerate() {
            if let Some(owner) = row.owner_id { owner_stations.entry(owner).or_default().push(i); }
        }
        let mut adjacency: HashMap<i64, Vec<i64>> = HashMap::new();
        for row in jumps { adjacency.entry(row.from_solar_system_id).or_default().push(row.to_solar_system_id); }
        for destinations in adjacency.values_mut() { destinations.sort_unstable(); destinations.dedup(); }
        let station_standing_restrictions = project_station_standing_restrictions(restrictions)?;
        Ok(Self { systems, regions, constellations, stargates, stations, celestials, system_index, region_index, constellation_index, stargate_index, station_index, celestial_index, system_celestials, owner_stations, jumps: adjacency, station_standing_restrictions })
    }
    pub(crate) fn system(&self, id: i64) -> Option<&SolarSystem> { self.system_index.get(&id).map(|&i| &self.systems[i]) }
    pub(crate) fn region(&self, id: i64) -> Option<&Region> { self.region_index.get(&id).map(|&i| &self.regions[i]) }
    pub(crate) fn constellation(&self, id: i64) -> Option<&Constellation> { self.constellation_index.get(&id).map(|&i| &self.constellations[i]) }
    pub(crate) fn stargate(&self, id: i64) -> Option<&Stargate> { self.stargate_index.get(&id).map(|&i| &self.stargates[i]) }
    pub(crate) fn station(&self, id: i64) -> Option<&Station> { self.station_index.get(&id).map(|&i| &self.stations[i]) }
    /// Required raw standing for an owner and service, or `None` if unrestricted.
    pub(crate) fn required_station_standing(&self, owner: i64, service: i32) -> Option<f64> {
        self.station_standing_restrictions.get(&owner)?.get(&service).copied()
    }
    pub(crate) fn celestial(&self, id: i64) -> Option<&Celestial> { self.celestial_index.get(&id).map(|&i| &self.celestials[i]) }
    pub(crate) fn systems(&self) -> impl Iterator<Item = &SolarSystem> { self.systems.iter() }
    pub(crate) fn stations(&self) -> impl Iterator<Item = &Station> { self.stations.iter() }
    pub(crate) fn all_celestials(&self) -> impl Iterator<Item = &Celestial> { self.celestials.iter() }
    pub(crate) fn celestials(&self, system: i64) -> impl Iterator<Item = &Celestial> { self.system_celestials.get(&system).into_iter().flatten().map(|&i| &self.celestials[i]) }
    pub(crate) fn stations_by_owner(&self, owner: i64) -> impl Iterator<Item = &Station> { self.owner_stations.get(&owner).into_iter().flatten().map(|&i| &self.stations[i]) }
    pub(crate) fn jumps(&self, system: i64) -> &[i64] { self.jumps.get(&system).map_or(&[], Vec::as_slice) }
}

fn project_station_standing_restrictions(
    restrictions: Vec<dao::StationStandingRestriction>,
) -> anyhow::Result<HashMap<i64, HashMap<i32, f64>>> {
    let mut projected = HashMap::new();
    for row in restrictions {
        let owner_text = row.owner.as_deref().ok_or_else(|| anyhow::anyhow!("station restriction has no owner"))?;
        let owner: i64 = owner_text.parse().map_err(|_| anyhow::anyhow!("invalid station restriction owner {owner_text}"))?;
        let services_text = row.services.as_deref().ok_or_else(|| anyhow::anyhow!("station restriction owner {owner} has no services"))?;
        let services: serde_json::Value = serde_json::from_str(services_text)
            .map_err(|e| anyhow::anyhow!("invalid station restriction services for owner {owner}: {e}"))?;
        let services = services.as_object().ok_or_else(|| anyhow::anyhow!("station restriction services for owner {owner} are not an object"))?;
        let mut thresholds = HashMap::new();
        for (service_text, value) in services {
            let service: i32 = service_text.parse().map_err(|_| anyhow::anyhow!("invalid station restriction service {service_text} for owner {owner}"))?;
            if !STANDING_SKILL_EXEMPT_FACTIONS.contains(&owner) {
                anyhow::bail!("station restriction owner {owner} service {service} is not exempt from standing skill bonuses");
            }
            let threshold = value.as_f64().filter(|v| v.is_finite())
                .ok_or_else(|| anyhow::anyhow!("invalid station restriction threshold for owner {owner} service {service}"))?;
            if thresholds.insert(service, threshold).is_some() {
                anyhow::bail!("duplicate station restriction for owner {owner} service {service}");
            }
        }
        if projected.insert(owner, thresholds).is_some() {
            anyhow::bail!("duplicate station restriction owner {owner}");
        }
    }
    Ok(projected)
}

#[cfg(test)]
mod tests;
