//! Immutable dungeon projections and map-derived exploration system classes.
//! The client consumes generated sites via `eve/client/script/parklife/scanSvc.py:253-260`.
//! Sources are `sde_native.dungeons` and the loaded SDE map; live dungeon layouts are excluded.

use std::collections::HashMap;
use infinity_core::{consts::{GROUP_PLANET, WORMHOLE_SYSTEM_START}};
use crate::{dao, db::Db, sde::map::Map};
/// Existing exploration SQL's exclusive destination-system upper bound.
const DESTINATION_SYSTEM_END: i64 = 31_002_605;
/// Existing exploration SQL excludes Jove regions from destinations.
const EXCLUDED_REGIONS: [i64; 3] = [10_000_004, 10_000_017, 10_000_019];
/// Existing exploration SQL's high-security threshold.
const HIGH_SECURITY: f64 = 0.45;
/// SDE wormhole class IDs for high-, low-, and null-security space.
const HIGH_CLASS: i64 = 7;
const LOW_CLASS: i64 = 8;
const NULL_CLASS: i64 = 9;

pub(crate) struct Dungeon {
    pub id: i64,
    pub name_id: Option<i64>,
    pub has_name: bool,
    pub difficulty: Option<i64>,
    pub faction_id: Option<i64>,
    pub archetype: Option<String>,
    pub gated: bool,
}
pub(crate) struct SystemInfo {
    pub security: f64,
    pub faction: Option<i64>,
    pub region: i64,
    pub class: i64,
}

/// Derive the exploration projection using the map's inner-joined records.
pub(crate) fn system_info(map: &Map, system: i64) -> Option<SystemInfo> {
    let system = map.system(system)?;
    let constellation = map.constellation(system.constellation_id?)?;
    let region = map.region(system.region_id?)?;
    let security_class = match system.security {
        Some(security) if security >= HIGH_SECURITY => HIGH_CLASS,
        Some(security) if security > 0.0 => LOW_CLASS,
        _ => NULL_CLASS,
    };
    let map_class = constellation.wormhole_class_id.or(region.wormhole_class_id);
    let class = if system.solar_system_id < WORMHOLE_SYSTEM_START && (HIGH_CLASS..=NULL_CLASS).contains(&map_class.unwrap_or(HIGH_CLASS)) {
        security_class
    } else {
        map_class.unwrap_or(security_class)
    };
    Some(SystemInfo { security: system.security.unwrap_or(0.0), faction: system.faction_id, region: region.region_id, class })
}
#[cfg_attr(test, derive(Default))]
pub(crate) struct Dungeons {
    rows: Vec<Dungeon>,
    classes: HashMap<i64, Vec<i64>>,
}
impl Dungeons {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        Ok(Self { rows: dao::sde::dungeons::load(db).await?, classes: HashMap::new() })
    }
    pub(crate) fn index_map(&mut self, map: &Map) {
        self.classes.clear();
        for system in map.systems() {
            let Some(info) = system_info(map, system.solar_system_id) else { continue };
            if system.solar_system_id < DESTINATION_SYSTEM_END && !EXCLUDED_REGIONS.contains(&info.region)
                && map.celestials(system.solar_system_id).any(|celestial| celestial.group_id == Some(i64::from(GROUP_PLANET))) {
                self.classes.entry(info.class).or_default().push(system.solar_system_id);
            }
        }
        for systems in self.classes.values_mut() { systems.sort_unstable(); }
    }
    pub(crate) fn get(&self, id: i64) -> Option<&Dungeon> { self.rows.binary_search_by_key(&id, |row| row.id).ok().map(|i| &self.rows[i]) }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Dungeon> { self.rows.iter() }
    pub(crate) fn systems_of_class(&self, class: i64) -> &[i64] { self.classes.get(&class).map_or(&[], Vec::as_slice) }
}
