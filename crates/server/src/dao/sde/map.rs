//! Startup projections of SDE map, station, and native station restriction tables.
//! Client consumers include `eve/client/script/remote/michelle.py:491-532`,
//! `eve/client/script/parklife/autopilot.py:260-261,329-332`, and
//! `evestations/standingsrestriction.py`. Unused map fields are not loaded.

use crate::db::{self, executor::Executor};
use crate::sde::map::{SolarSystem, Region, Constellation, Stargate, Station, Celestial};
use tokio_postgres::Row;

impl SolarSystem {
    fn from_row(row: &Row) -> Self {
        Self {
            solar_system_id: row.get("solar_system_id"),
            name: row.get("name"),
            constellation_id: row.get("constellation_id"),
            region_id: row.get("region_id"),
            faction_id: row.get("faction_id"),
            security: row.get("security"),
            x: row.get("x"),
            y: row.get("y"),
            z: row.get("z"),
        }
    }
}
impl Region {
    fn from_row(row: &Row) -> Self {
        Self {
            region_id: row.get("region_id"),
            wormhole_class_id: row.get("wormhole_class_id"),
            nebula_id: row.get("nebula_id"),
        }
    }
}
impl Constellation {
    fn from_row(row: &Row) -> Self {
        Self {
            constellation_id: row.get("constellation_id"),
            wormhole_class_id: row.get("wormhole_class_id"),
        }
    }
}
impl Stargate {
    fn from_row(row: &Row) -> Self {
        Self {
            stargate_id: row.get("stargate_id"),
            solar_system_id: row.get("solar_system_id"),
            dest_stargate_id: row.get("dest_stargate_id"),
            dest_solar_system_id: row.get("dest_solar_system_id"),
        }
    }
}
impl Station {
    fn from_row(row: &Row) -> Self {
        Self {
            station_id: row.get("station_id"),
            type_id: row.get("type_id"),
            owner_id: row.get("owner_id"),
            solar_system_id: row.get("solar_system_id"),
            constellation_id: row.get("constellation_id"),
            region_id: row.get("region_id"),
            operation_id: row.get("operation_id"),
            x: row.get("x"),
            y: row.get("y"),
            z: row.get("z"),
            security: row.get("security"),
        }
    }
}
impl Celestial {
    fn from_row(row: &Row) -> Self {
        Self {
            item_id: row.get("item_id"),
            type_id: row.get("type_id"),
            solar_system_id: row.get("solar_system_id"),
            group_id: row.get("group_id"),
            x: row.get("x"),
            y: row.get("y"),
            z: row.get("z"),
            radius: row.get("radius"),
            celestial_index: row.get("celestial_index"),
        }
    }
}

pub(crate) struct Jump {
    pub from_solar_system_id: i64,
    pub to_solar_system_id: i64,
}
impl Jump {
    fn from_row(row: &Row) -> Self {
        Self {
            from_solar_system_id: row.get("from_solar_system_id"),
            to_solar_system_id: row.get("to_solar_system_id"),
        }
    }
}
/// Raw owner-keyed service thresholds from `sde_native.stationstandingsrestrictions`.
pub(crate) struct StationStandingRestriction {
    pub owner: Option<String>,
    pub services: Option<String>,
}

impl StationStandingRestriction {
    fn from_row(row: &Row) -> Self {
        Self { owner: row.get("owner"), services: row.get("services") }
    }
}

pub(crate) async fn station_standing_restrictions(ex: &(impl Executor + ?Sized)) -> Result<Vec<StationStandingRestriction>, db::Error> {
    Ok(ex.query("SELECT key AS owner, (data->'services')::text AS services FROM sde_native.stationstandingsrestrictions ORDER BY row_no", &[]).await?.iter().map(StationStandingRestriction::from_row).collect())
}

pub(crate) async fn systems(ex: &(impl Executor + ?Sized)) -> Result<Vec<SolarSystem>, db::Error> {
    Ok(ex.query("SELECT solar_system_id::bigint, name, constellation_id::bigint, region_id::bigint, faction_id::bigint, security::float8, x::float8, y::float8, z::float8 FROM sde.map_solar_systems ORDER BY solar_system_id", &[]).await?.iter().map(SolarSystem::from_row).collect())
}
pub(crate) async fn regions(ex: &(impl Executor + ?Sized)) -> Result<Vec<Region>, db::Error> {
    Ok(ex.query("SELECT region_id::bigint, wormhole_class_id::bigint, nebula_id::bigint FROM sde.map_regions ORDER BY region_id", &[]).await?.iter().map(Region::from_row).collect())
}
pub(crate) async fn constellations(ex: &(impl Executor + ?Sized)) -> Result<Vec<Constellation>, db::Error> {
    Ok(ex.query("SELECT constellation_id::bigint, wormhole_class_id::bigint FROM sde.map_constellations ORDER BY constellation_id", &[]).await?.iter().map(Constellation::from_row).collect())
}
pub(crate) async fn stargates(ex: &(impl Executor + ?Sized)) -> Result<Vec<Stargate>, db::Error> {
    Ok(ex.query("SELECT stargate_id::bigint, solar_system_id::bigint, dest_stargate_id::bigint, dest_solar_system_id::bigint FROM sde.map_stargates ORDER BY stargate_id", &[]).await?.iter().map(Stargate::from_row).collect())
}
pub(crate) async fn stations(ex: &(impl Executor + ?Sized)) -> Result<Vec<Station>, db::Error> {
    Ok(ex.query("SELECT station_id::bigint, type_id::bigint, owner_id::bigint, solar_system_id::bigint, constellation_id::bigint, region_id::bigint, operation_id::bigint, x::float8, y::float8, z::float8, security::float8 FROM sde.sta_stations ORDER BY station_id", &[]).await?.iter().map(Station::from_row).collect())
}
pub(crate) async fn celestials(ex: &(impl Executor + ?Sized)) -> Result<Vec<Celestial>, db::Error> {
    Ok(ex.query("SELECT item_id::bigint, type_id::bigint, solar_system_id::bigint, group_id::bigint, x::float8, y::float8, z::float8, radius::float8, celestial_index::bigint FROM sde.map_celestials ORDER BY item_id", &[]).await?.iter().map(Celestial::from_row).collect())
}
pub(crate) async fn jumps(ex: &(impl Executor + ?Sized)) -> Result<Vec<Jump>, db::Error> {
    Ok(ex.query("SELECT from_solar_system_id::bigint, to_solar_system_id::bigint FROM sde.map_jumps ORDER BY from_solar_system_id, to_solar_system_id", &[]).await?.iter().map(Jump::from_row).collect())
}

#[cfg(test)]
mod tests;
