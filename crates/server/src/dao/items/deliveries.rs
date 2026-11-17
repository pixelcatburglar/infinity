//! Shared item ancestry and retained office metadata from `game.items` and `game.offices`.
//! Consumers apply their own access and custody rules.

use crate::db::{executor::Executor, Error};

/// Item ancestry, with UNION ensuring corrupt cycles terminate.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Ancestor {
    pub item_id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i32,
    pub quantity: i64,
    pub singleton: bool,
}

impl Ancestor {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { item_id: row.get("item_id"), type_id: row.get("type_id"), owner_id: row.get("owner_id"), location_id: row.get("location_id"), flag_id: row.get("flag_id"), quantity: row.get("quantity"), singleton: row.get("singleton") }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OfficeMetadata {
    pub corporation_id: i64,
    pub station_id: i64,
    /// Server lease lifecycle in game.offices.lease_state (0 active, 1 impounded,
    /// 2 released); consumers combine it with rented_until, never trust the raw state.
    pub lease_state: i16,
    pub rented_until: i64,
}

impl OfficeMetadata {
    /// Same predicate as `offices::Lease::is_active`: an out-of-range state
    /// encoding decodes to an error and can never be active.
    pub(crate) fn is_active(&self, now: i64) -> bool {
        crate::dao::offices::LeaseState::decode(self.lease_state)
            .is_ok_and(|state| state == crate::dao::offices::LeaseState::Active)
            && self.rented_until > now
    }
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LineageRow {
    pub origin: i64,
    pub ancestor: Ancestor,
    pub office: Option<OfficeMetadata>,
}

impl LineageRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self {
            origin: row.get("origin"),
            ancestor: Ancestor::from_row(row),
            office: row.get::<_, Option<i64>>("office_corporation_id").map(|corporation_id| OfficeMetadata {
                corporation_id,
                station_id: row.get("office_station_id"),
                lease_state: row.get("office_lease_state"),
                rented_until: row.get("office_rented_until"),
            }),
        }
    }
}

pub(crate) async fn ancestry_many(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<LineageRow>, Error> {
    Ok(ex.query("WITH RECURSIVE ancestors AS (SELECT seed.origin, i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton FROM unnest($1::bigint[]) AS seed(origin) JOIN game.items i ON i.item_id = seed.origin WHERE i.jump_clone_id IS NULL UNION SELECT a.origin, i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton FROM game.items i JOIN ancestors a ON i.item_id = a.location_id WHERE i.jump_clone_id IS NULL) SELECT a.origin, a.item_id, a.type_id, a.owner_id, a.location_id, a.flag_id, a.quantity, a.singleton, o.corporation_id AS office_corporation_id, o.station_id AS office_station_id, o.lease_state AS office_lease_state, o.rented_until AS office_rented_until FROM ancestors a LEFT JOIN game.offices o ON o.office_id = a.item_id ORDER BY a.origin, a.item_id", &[&ids]).await?.iter().map(LineageRow::from_row).collect())
}

pub(crate) async fn ancestry(ex: &(impl Executor + ?Sized), id: i64) -> Result<Vec<Ancestor>, Error> {
    Ok(ancestry_many(ex, &[id]).await?.into_iter().map(|row| row.ancestor).collect())
}

#[cfg(all(test, feature = "tests-storage"))] mod tests;
