//! Scoped committed sovereignty reads for `sovMgr`, alliance, station and map.
//! The trusted adapter supplies authenticated selectors and authorizes alliance
//! scope. Campaigns and development-index queries are not supported here.
use marshal::Value;
use crate::{dao::sovereignty::{history, policy, structures, systems}, db::Db, sde::Sde, services::{db_err, CallError}};
use super::wire;

/// `appConst.locationUniverse`, the root of the client map hierarchy.
const LOCATION_UNIVERSE: i64 = 9;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Query {
    SystemSovereigntyInfo { system_id: i64 },
    InfrastructureHubInfo { system_id: i64 },
    Structures { system_id: i64 },
    AllianceStructures { alliance_id: i64 },
    CapitalSystemInfo { alliance_id: i64 },
    AllianceSystems,
    SystemsForAlliance { alliance_id: i64 },
    CurrentSovData { location_id: i64 },
    RecentSovActivity,
}

pub(super) fn positive(id: i64) -> Result<i64, CallError> {
    if id > 0 { Ok(id) } else { Err(CallError::Internal(anyhow::anyhow!("invalid sovereignty selector"))) }
}

fn map_systems(sde: &Sde, location: i64) -> Result<Vec<i64>, CallError> {
    positive(location)?;
    if location != LOCATION_UNIVERSE && sde.map.system(location).is_none() && sde.map.constellation(location).is_none() && sde.map.region(location).is_none() {
        return Err(CallError::Internal(anyhow::anyhow!("unknown map location")));
    }
    Ok(sde.map.systems().filter(|s| location == LOCATION_UNIVERSE || s.solar_system_id == location || s.constellation_id == Some(location) || s.region_id == Some(location)).map(|s| s.solar_system_id).collect())
}

pub(crate) async fn read(db: &Db, sde: &Sde, query: Query) -> Result<Value, CallError> {
    match query {
        Query::SystemSovereigntyInfo { system_id } => {
            let row = systems::read(db, positive(system_id)?).await.map_err(db_err)?;
            Ok(row.as_ref().map_or(Value::None, wire::system_claim))
        }
        Query::InfrastructureHubInfo { system_id } => {
            let row = systems::read(db, positive(system_id)?).await.map_err(db_err)?;
            Ok(wire::hub(row.as_ref()))
        }
        Query::Structures { system_id } => {
            let hubs = structures::snapshots(db, Some(positive(system_id)?), None).await.map_err(db_err)?;
            let policy = match hubs.first().and_then(|h| h.system.alliance_id) {
                Some(alliance) => policy::read(db, alliance).await.map_err(db_err)?, None => None,
            };
            Ok(Value::List(wire::structures(&hubs, policy.as_slice())))
        }
        Query::AllianceStructures { alliance_id } => {
            let hubs = structures::snapshots(db, None, Some(positive(alliance_id)?)).await.map_err(db_err)?;
            Ok(wire::alliance_structures(&hubs))
        }
        Query::CapitalSystemInfo { alliance_id } => {
            let row = policy::read(db, positive(alliance_id)?).await.map_err(db_err)?;
            Ok(wire::capital(row.as_ref()))
        }
        Query::AllianceSystems => Ok(wire::alliance_systems(&systems::list(db).await.map_err(db_err)?)),
        Query::SystemsForAlliance { alliance_id } => Ok(wire::alliance_systems(&systems::for_alliance(db, positive(alliance_id)?).await.map_err(db_err)?)),
        Query::CurrentSovData { location_id } => {
            let ids = map_systems(sde, location_id)?;
            let rows = systems::for_systems(db, &ids).await.map_err(db_err)?;
            Ok(wire::current_map(&ids, &rows))
        }
        Query::RecentSovActivity => Ok(wire::history(&history::recent(db).await.map_err(db_err)?)),
    }
}
