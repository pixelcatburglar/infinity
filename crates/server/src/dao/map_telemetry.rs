//! Durable FILETIME map events and cumulative destination visits for map history
//! and visit consumers in `eve/client/script/ui/shared/mapView/mapViewColorHandler.py`
//! and `eve/client/script/ui/shared/mapView/filters/mapFilterVisited.py`.
//! Uses `game.map_events`, `game.map_visits`, and `game.map_occurrence_id_seq`.
//! Callers own transaction boundaries and record visits only for accepted arrivals;
//! event receipts expire under retention while visits persist independently.
//! This DAO does not provide topology, beacon, or faction warfare data.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

// Database encodings are shared with the CHECK constraints in game_schema.sql.
#[derive(Copy, Clone)]
#[repr(i16)]
pub(crate) enum EventKind { Jump = 1, Ship = 2, Npc = 3, Pod = 4 }

#[derive(Copy, Clone)]
#[repr(i16)]
pub(crate) enum EventSource { JumpOccurrence = 1, Killmail = 2, NpcOccurrence = 3, UnpilotedShip = 4 }

#[derive(Copy, Clone)]
pub(crate) enum HistoryStat { Jumps, Kills }

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct History {
    pub solar_system_id: i64,
    pub value1: i64,
    pub value2: i64,
    pub value3: i64,
}

impl History {
    fn from_row(row: &Row) -> Self {
        Self { solar_system_id: row.get("solar_system_id"), value1: row.get("value1"), value2: row.get("value2"), value3: row.get("value3") }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Visit {
    pub last_date_time: i64,
    pub solar_system_id: i64,
    pub visits: i64,
}

impl Visit {
    fn from_row(row: &Row) -> Self {
        Self { last_date_time: row.get("last_date_time"), solar_system_id: row.get("solar_system_id"), visits: row.get("visits") }
    }
}

/// Allocate outside a transaction retry closure so all attempts share an identity.
pub(crate) async fn next_occurrence(ex: &(impl Executor + ?Sized)) -> Result<i64, db::Error> {
    Ok(ex.query_one("SELECT nextval('game.map_occurrence_id_seq') AS occurrence_id", &[]).await?.get("occurrence_id"))
}

pub(crate) async fn append(ex: &(impl Executor + ?Sized), source: EventSource, source_id: i64, system: i64, event_time: i64, kind: EventKind) -> Result<bool, db::Error> {
    Ok(ex.execute(
        "INSERT INTO game.map_events (source, source_id, solar_system_id, event_time, kind) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (source, source_id) DO NOTHING",
        &[&(source as i16), &source_id, &system, &event_time, &(kind as i16)],
    ).await? != 0)
}

pub(crate) async fn history(ex: &(impl Executor + ?Sized), stat: HistoryStat, cutoff: i64, now: i64) -> Result<Vec<History>, db::Error> {
    let (first, second, third) = match stat {
        HistoryStat::Jumps => (EventKind::Jump as i16, EventKind::Jump as i16, EventKind::Jump as i16),
        HistoryStat::Kills => (EventKind::Ship as i16, EventKind::Npc as i16, EventKind::Pod as i16),
    };
    let jumps = matches!(stat, HistoryStat::Jumps);
    Ok(ex.query(
        "SELECT solar_system_id, COUNT(*) FILTER (WHERE kind = $3 OR (NOT $6 AND kind = $4)) AS value1, COUNT(*) FILTER (WHERE NOT $6 AND kind = $4) AS value2, COUNT(*) FILTER (WHERE NOT $6 AND kind = $5) AS value3 FROM game.map_events WHERE event_time >= $1 AND event_time <= $2 AND (kind = $3 OR (NOT $6 AND kind IN ($4, $5))) GROUP BY solar_system_id ORDER BY solar_system_id",
        &[&cutoff, &now, &first, &second, &third, &jumps],
    ).await?.iter().map(History::from_row).collect())
}

/// Invoke only after an accepted jump append, within the same transaction.
pub(crate) async fn record_visit(ex: &(impl Executor + ?Sized), character: i64, system: i64, now: i64) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.map_visits (character_id, solar_system_id, last_date_time, visits) VALUES ($1, $2, $3, 1) ON CONFLICT (character_id, solar_system_id) DO UPDATE SET visits = game.map_visits.visits + 1, last_date_time = GREATEST(game.map_visits.last_date_time, EXCLUDED.last_date_time)", &[&character, &system, &now]).await?;
    Ok(())
}

pub(crate) async fn visits(ex: &(impl Executor + ?Sized), character: i64) -> Result<Vec<Visit>, db::Error> {
    Ok(ex.query("SELECT last_date_time, solar_system_id, visits FROM game.map_visits WHERE character_id = $1 ORDER BY solar_system_id", &[&character]).await?.iter().map(Visit::from_row).collect())
}

pub(crate) async fn delete_before(ex: &(impl Executor + ?Sized), cutoff: i64) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.map_events WHERE event_time < $1", &[&cutoff]).await
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
