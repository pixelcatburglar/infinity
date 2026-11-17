//! Office lease persistence for officeManager
//! (eve/client/script/ui/shared/neocom/corporation). Uses game.offices and game.items.
//! A lease row is the office item's identity: it survives inactive states so the same
//! corporation/station container is reused. `Active` additionally requires
//! `rented_until > request time`; an expired Active row is logically inactive until a
//! billing pass materializes it. Notifications and wallet effects are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

/// Server lease lifecycle values in game.offices.lease_state; the client never reads them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LeaseState {
    Active = 0,
    Impounded = 1,
    Released = 2,
}

impl LeaseState {
    pub(crate) fn decode(value: i16) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Active),
            1 => Ok(Self::Impounded),
            2 => Ok(Self::Released),
            other => Err(Error::Progress(anyhow::anyhow!("game.offices lease_state {other} is out of range"))),
        }
    }
    pub(super) fn encode(self) -> i16 {
        self as i16
    }
}

/// One office lease row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Lease {
    pub office_id: i64,
    pub corporation_id: i64,
    pub station_id: i64,
    pub rented_until: i64,
    pub state: LeaseState,
    pub state_changed_at: i64,
}

impl Lease {
    /// The lease grants access only while Active and before its exact deadline.
    pub(crate) fn is_active(&self, now: i64) -> bool {
        self.state == LeaseState::Active && self.rented_until > now
    }
    fn from_row(r: &Row) -> Result<Self, Error> {
        Ok(Self {
            office_id: r.get("office_id"),
            corporation_id: r.get("corporation_id"),
            station_id: r.get("station_id"),
            rented_until: r.get("rented_until"),
            state: LeaseState::decode(r.get("lease_state"))?,
            state_changed_at: r.get("state_changed_at"),
        })
    }
}

struct ItemId {
    item_id: i64,
}
impl ItemId {
    fn from_row(r: &Row) -> Self {
        Self { item_id: r.get("item_id") }
    }
}
struct CorporationId {
    corporation_id: i64,
}
impl CorporationId {
    fn from_row(r: &Row) -> Self {
        Self { corporation_id: r.get("corporation_id") }
    }
}
struct StationId {
    station_id: i64,
}
impl StationId {
    fn from_row(r: &Row) -> Self {
        Self { station_id: r.get("station_id") }
    }
}
struct Count {
    count: i64,
}
impl Count {
    fn from_row(r: &Row) -> Self {
        Self { count: r.get("count") }
    }
}

const LEASE_SELECT: &str = "SELECT office_id, corporation_id, station_id, rented_until, lease_state, state_changed_at FROM game.offices";

/// The lease for `corp` at `station`, whatever its state.
pub(crate) async fn identity(ex: &(impl Executor + ?Sized), corp: i64, station: i64) -> Result<Option<Lease>, Error> {
    ex.query_opt(&format!("{LEASE_SELECT} WHERE corporation_id = $1 AND station_id = $2"), &[&corp, &station]).await?
        .map(|r| Lease::from_row(&r)).transpose()
}

/// One lease by its office item id, whatever its state.
pub(crate) async fn get(ex: &(impl Executor + ?Sized), office: i64) -> Result<Option<Lease>, Error> {
    ex.query_opt(&format!("{LEASE_SELECT} WHERE office_id = $1"), &[&office]).await?
        .map(|r| Lease::from_row(&r)).transpose()
}

/// The lease only when it is Active and unexpired at `now`.
pub(crate) async fn active_at(ex: &(impl Executor + ?Sized), corp: i64, station: i64, now: i64) -> Result<Option<Lease>, Error> {
    ex.query_opt(&format!("{LEASE_SELECT} WHERE corporation_id = $1 AND station_id = $2 AND lease_state = $3 AND rented_until > $4"),
        &[&corp, &station, &LeaseState::Active.encode(), &now]).await?
        .map(|r| Lease::from_row(&r)).transpose()
}

/// Every lease of `corp` that is Active and unexpired at `now`.
pub(crate) async fn active_list(ex: &(impl Executor + ?Sized), corp: i64, now: i64) -> Result<Vec<Lease>, Error> {
    ex.query(&format!("{LEASE_SELECT} WHERE corporation_id = $1 AND lease_state = $2 AND rented_until > $3 ORDER BY office_id"),
        &[&corp, &LeaseState::Active.encode(), &now]).await?
        .iter().map(|r| Lease::from_row(r)).collect()
}

/// Corporations holding an Active unexpired lease at `station`, for the public office list.
pub(crate) async fn active_corporations(ex: &(impl Executor + ?Sized), station: i64, now: i64) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT corporation_id FROM game.offices WHERE station_id = $1 AND lease_state = $2 AND rented_until > $3",
        &[&station, &LeaseState::Active.encode(), &now]).await?
        .iter().map(|r| CorporationId::from_row(r).corporation_id).collect())
}

/// How many Active unexpired leases `station` holds at `now`; capacity consumes it.
pub(crate) async fn active_count(ex: &(impl Executor + ?Sized), station: i64, now: i64) -> Result<i64, Error> {
    Ok(Count::from_row(&ex.query_one("SELECT count(*) AS count FROM game.offices WHERE station_id = $1 AND lease_state = $2 AND rented_until > $3",
        &[&station, &LeaseState::Active.encode(), &now]).await?).count)
}

/// State-independent owner of an office item; inactive leases still answer.
pub(crate) async fn owner(ex: &(impl Executor + ?Sized), item: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT corporation_id FROM game.offices WHERE office_id = $1", &[&item]).await?
        .map(|r| CorporationId::from_row(&r).corporation_id))
}

/// State-independent station of an office item; inactive leases still answer.
pub(crate) async fn station(ex: &(impl Executor + ?Sized), office: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT station_id FROM game.offices WHERE office_id = $1", &[&office]).await?
        .map(|r| StationId::from_row(&r).station_id))
}

/// Insert the lease row for an already-created office item; the term starts Active.
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), office: i64, corp: i64, station: i64, until: i64, now: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.offices (office_id, corporation_id, station_id, rented_until, lease_state, state_changed_at) VALUES ($1, $2, $3, $4, $5, $6)",
        &[&office, &corp, &station, &until, &LeaseState::Active.encode(), &now]).await
}

/// Guarded state change: only from `expected_state` with `expected_until`.
/// Used for unrent, expiry materialization and re-rent release-free restoration;
/// `new_until` replaces the deadline (pass `expected_until` to keep it).
pub(crate) async fn transition(ex: &(impl Executor + ?Sized), office: i64, expected_state: LeaseState, expected_until: i64, next_state: LeaseState, new_until: i64, now: i64) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.offices SET lease_state = $4, rented_until = $5, state_changed_at = $6 WHERE office_id = $1 AND lease_state = $2 AND rented_until = $3",
        &[&office, &expected_state.encode(), &expected_until, &next_state.encode(), &new_until, &now]).await? == 1)
}

/// Guarded term renewal: only an Active lease whose deadline is exactly
/// `expected_until` and still beyond `now` may extend to `new_until`.
pub(crate) async fn renew(ex: &(impl Executor + ?Sized), office: i64, expected_until: i64, new_until: i64, now: i64) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.offices SET rented_until = $3 WHERE office_id = $1 AND lease_state = $4 AND rented_until = $2 AND rented_until > $5",
        &[&office, &expected_until, &new_until, &LeaseState::Active.encode(), &now]).await? == 1)
}

/// Whether the office item holds any contents (direct children).
pub(crate) async fn has_items(ex: &(impl Executor + ?Sized), office: i64) -> Result<bool, Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.items WHERE location_id = $1 LIMIT 1", &[&office]).await?.is_some())
}

/// Direct children of the office item, for impound release.
pub(crate) async fn child_ids(ex: &(impl Executor + ?Sized), office: i64) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT item_id FROM game.items WHERE location_id = $1 AND jump_clone_id IS NULL ORDER BY item_id", &[&office]).await?
        .iter().map(|r| ItemId::from_row(r).item_id).collect())
}
