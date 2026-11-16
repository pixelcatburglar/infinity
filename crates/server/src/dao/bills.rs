//! Bill persistence for billMgr (eve/client/script/ui/shared/neocom/bill) and the
//! alliance bill arms. Uses game.bills, game.bill_autopay, game.corporations,
//! game.alliances and game.offices. Amounts are exact numeric(36,2) parsed through
//! IskAmount; services own authorization, wallet effects, transactions and notifications.
use crate::db::{executor::Executor, Error};
use crate::isk::IskAmount;
use infinity_core::consts::{BILL_STATUS_CANCELLED, BILL_STATUS_PAID, BILL_STATUS_UNPAID, BILL_TYPE_RENTAL};
use tokio_postgres::Row;

/// Owner of an autopay configuration or a debtor identity: the owner id itself
/// distinguishes corporation from alliance (game.bill_autopay is owner-keyed).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BillOwner {
    Corporation(i64),
    Alliance(i64),
}

impl BillOwner {
    pub(crate) fn id(self) -> i64 {
        match self {
            Self::Corporation(id) | Self::Alliance(id) => id,
        }
    }
}


#[derive(Clone)]
pub(crate) struct Bill {
    pub bill_id: i64,
    pub bill_type: i32,
    pub debtor_id: i64,
    pub creditor_id: i64,
    pub amount: IskAmount,
    pub due: i64,
    pub interest: f64,
    pub external_id: Option<i64>,
    pub external_id2: Option<i64>,
    pub status: i32,
    pub period_start: Option<i64>,
    pub period_end: Option<i64>,
}

impl Bill {
    fn from_row(r: &Row) -> Result<Self, Error> {
        let amount: String = r.get("amount");
        Ok(Self {
            bill_id: r.get("bill_id"),
            bill_type: r.get("bill_type"),
            debtor_id: r.get("debtor_id"),
            creditor_id: r.get("creditor_id"),
            amount: IskAmount::parse_decimal(&amount).map_err(|e| Error::Progress(anyhow::anyhow!("game.bills amount: {e}")))?,
            due: r.get("due"),
            interest: r.get("interest"),
            external_id: r.get("external_id"),
            external_id2: r.get("external_id2"),
            status: r.get("status"),
            period_start: r.get("period_start"),
            period_end: r.get("period_end"),
        })
    }
}

const BILL_SELECT: &str = "SELECT bill_id, bill_type, debtor_id, creditor_id, amount::text AS amount, due, interest, external_id, external_id2, status, period_start, period_end FROM game.bills";

/// Unpaid bills `owner` owes (`creditor` false) or is owed.
pub(crate) async fn list(ex: &(impl Executor + ?Sized), owner: i64, creditor: bool) -> Result<Vec<Bill>, Error> {
    let column = if creditor { "creditor_id" } else { "debtor_id" };
    ex.query(&format!("{BILL_SELECT} WHERE {column} = $1 AND status = $2 ORDER BY due"), &[&owner, &BILL_STATUS_UNPAID]).await?
        .iter().map(|r| Bill::from_row(r)).collect()
}

/// One bill in any state, for settlement reads.
pub(crate) async fn get(ex: &(impl Executor + ?Sized), bill: i64) -> Result<Option<Bill>, Error> {
    ex.query_opt(&format!("{BILL_SELECT} WHERE bill_id = $1"), &[&bill]).await?
        .map(|r| Bill::from_row(&r)).transpose()
}

/// Claim the bill as paid only when every expected fact still matches.
/// The caller supplies the bill it re-read inside the same transaction.
pub(crate) async fn claim_paid(ex: &(impl Executor + ?Sized), expected: &Bill, now: i64) -> Result<bool, Error> {
    Ok(ex.execute(
        "UPDATE game.bills SET status = $8, settled_at = $9 WHERE bill_id = $1 AND status = $2 AND bill_type = $3 AND debtor_id = $4 AND creditor_id = $5 AND amount = $6::text::numeric AND due = $7 AND external_id IS NOT DISTINCT FROM $10 AND external_id2 IS NOT DISTINCT FROM $11 AND period_start IS NOT DISTINCT FROM $12 AND period_end IS NOT DISTINCT FROM $13",
        &[&expected.bill_id, &BILL_STATUS_UNPAID, &expected.bill_type, &expected.debtor_id, &expected.creditor_id,
          &expected.amount.decimal(), &expected.due, &BILL_STATUS_PAID, &now,
          &expected.external_id, &expected.external_id2, &expected.period_start, &expected.period_end]).await? == 1)
}

/// Terminally close a bill without payment; the guarded status keeps replays idempotent.
pub(crate) async fn close_unpaid(ex: &(impl Executor + ?Sized), bill: i64, status: i32, now: i64) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.bills SET status = $2, settled_at = $3 WHERE bill_id = $1 AND status = $4",
        &[&bill, &status, &now, &BILL_STATUS_UNPAID]).await? == 1)
}

/// Cancel every unpaid renewal bill of `office`'s debtor for terms starting at or
/// after `period_start`, e.g. when the lease ends before the covered boundary.
pub(crate) async fn cancel_unpaid_renewals(ex: &(impl Executor + ?Sized), office: i64, debtor: i64, period_start: i64, now: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.bills SET status = $5, settled_at = $6 WHERE bill_type = $1 AND external_id = $2 AND debtor_id = $3 AND period_start >= $4 AND status = $7",
        &[&BILL_TYPE_RENTAL, &office, &debtor, &period_start, &BILL_STATUS_CANCELLED, &now, &BILL_STATUS_UNPAID]).await
}

/// Issue one rental renewal bill for the term [period_start, period_end), due at
/// period_start. The partial unique index makes a second issue a no-op (0 rows).
pub(crate) async fn issue_renewal(ex: &(impl Executor + ?Sized), debtor: i64, creditor: i64, amount: &IskAmount, due: i64, office: i64, station: i64, period_start: i64, period_end: i64, now: i64) -> Result<bool, Error> {
    Ok(ex.execute(
        "INSERT INTO game.bills (bill_type, debtor_id, creditor_id, amount, due, external_id, external_id2, status, created, period_start, period_end) SELECT $1, $2, $3, $4::text::numeric, $5, $6, $7, $8, $9, $10, $11 WHERE NOT EXISTS (SELECT 1 FROM game.bills WHERE bill_type = $1 AND debtor_id = $2 AND external_id = $6 AND period_start = $10 AND status IN ($12, $13))",
        &[&BILL_TYPE_RENTAL, &debtor, &creditor, &amount.decimal(), &due, &office, &station,
          &BILL_STATUS_UNPAID, &now, &period_start, &period_end, &BILL_STATUS_UNPAID, &BILL_STATUS_PAID]).await? == 1)
}

/// Active leases whose rent comes due within `ahead` and have no live renewal bill
/// for exactly that office, debtor and boundary period.
#[derive(Clone)]
pub(crate) struct DueLease {
    pub office_id: i64,
    pub corporation_id: i64,
    pub station_id: i64,
    pub rented_until: i64,
}

impl DueLease {
    fn from_row(r: &Row) -> Self {
        Self {
            office_id: r.get("office_id"),
            corporation_id: r.get("corporation_id"),
            station_id: r.get("station_id"),
            rented_until: r.get("rented_until"),
        }
    }
}

pub(crate) async fn leases_due(ex: &(impl Executor + ?Sized), now: i64, ahead: i64) -> Result<Vec<DueLease>, Error> {
    Ok(ex.query(
        "SELECT o.office_id, o.corporation_id, o.station_id, o.rented_until FROM game.offices o
         WHERE o.lease_state = $6 AND o.rented_until > $1 AND o.rented_until - $1 <= $2
           AND NOT EXISTS (SELECT 1 FROM game.bills b WHERE b.bill_type = $3 AND b.external_id = o.office_id
                           AND b.debtor_id = o.corporation_id AND b.external_id2 = o.station_id
                           AND b.period_start = o.rented_until AND b.status IN ($4, $5))",
        &[&now, &ahead, &BILL_TYPE_RENTAL, &BILL_STATUS_UNPAID, &BILL_STATUS_PAID, &super::offices::LeaseState::Active.encode()]).await?
        .iter().map(DueLease::from_row).collect())
}

/// Leases whose Active deadline passed at or before `now`, for expiry materialization.
pub(crate) async fn leases_expired(ex: &(impl Executor + ?Sized), now: i64) -> Result<Vec<DueLease>, Error> {
    Ok(ex.query(
        "SELECT o.office_id, o.corporation_id, o.station_id, o.rented_until FROM game.offices o WHERE o.lease_state = $2 AND o.rented_until <= $1 ORDER BY o.office_id",
        &[&now, &super::offices::LeaseState::Active.encode()]).await?
        .iter().map(DueLease::from_row).collect())
}

/// An unpaid bill whose type the debtor pays automatically. The scan supplies
/// the bill, debtor and its corporation/alliance kind observed at scan time;
/// settlement re-reads every authority fact in the transaction.
pub(crate) struct Automatic {
    pub bill_id: i64,
    pub debtor_id: i64,
    /// True when `debtor_id` matched a corporation row, false for an alliance.
    pub corporation: bool,
}

impl Automatic {
    fn from_row(r: &Row) -> Self {
        Self { bill_id: r.get("bill_id"), debtor_id: r.get("debtor_id"), corporation: r.get("corporation") }
    }
}

pub(crate) async fn automatic(ex: &(impl Executor + ?Sized)) -> Result<Vec<Automatic>, Error> {
    Ok(ex.query(
        "SELECT b.bill_id, b.debtor_id, (co.corporation_id IS NOT NULL) AS corporation FROM game.bills b
         JOIN game.bill_autopay p ON p.owner_id = b.debtor_id AND p.bill_type = b.bill_type AND p.enabled
         LEFT JOIN game.corporations co ON co.corporation_id = b.debtor_id
         LEFT JOIN game.alliances a ON a.alliance_id = b.debtor_id AND a.deleted = false
         WHERE b.status = $1 AND (co.corporation_id IS NOT NULL OR a.alliance_id IS NOT NULL)",
        &[&BILL_STATUS_UNPAID]).await?
        .iter().map(Automatic::from_row).collect())
}

/// The unpaid renewal bill covering the lease boundary `until`, if one exists.
pub(crate) async fn renewal_for(ex: &(impl Executor + ?Sized), office: i64, debtor: i64, until: i64) -> Result<Option<Bill>, Error> {
    ex.query_opt(&format!("{BILL_SELECT} WHERE bill_type = $1 AND external_id = $2 AND debtor_id = $3 AND period_start = $4 AND status = $5"),
        &[&BILL_TYPE_RENTAL, &office, &debtor, &until, &BILL_STATUS_UNPAID]).await?
        .map(|r| Bill::from_row(&r)).transpose()
}

/// One autopay switch row.
pub(crate) struct Setting {
    pub bill_type: i32,
    pub enabled: bool,
}

impl Setting {
    fn from_row(r: &Row) -> Self {
        Self { bill_type: r.get("bill_type"), enabled: r.get("enabled") }
    }
}

/// One owner's autopay switches.
pub(crate) async fn settings(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<Setting>, Error> {
    Ok(ex.query("SELECT bill_type, enabled FROM game.bill_autopay WHERE owner_id = $1 ORDER BY bill_type", &[&owner]).await?
        .iter().map(Setting::from_row).collect())
}

struct Division {
    bill_division: i32,
}
impl Division {
    fn from_row(r: &Row) -> Self {
        Self { bill_division: r.get("bill_division") }
    }
}

/// One owner's configured bill payment division, from its own row
/// (a corporation's or an alliance's bill_division).
pub(crate) async fn division(ex: &(impl Executor + ?Sized), owner: BillOwner) -> Result<Option<i32>, Error> {
    let (table, column, id) = match owner {
        BillOwner::Corporation(id) => ("game.corporations", "corporation_id", id),
        BillOwner::Alliance(id) => ("game.alliances", "alliance_id", id),
    };
    Ok(ex.query_opt(&format!("SELECT bill_division FROM {table} WHERE {column} = $1"), &[&id]).await?
        .map(|r| Division::from_row(&r).bill_division))
}

/// Save one autopay switch for an owner.
pub(crate) async fn set_setting(ex: &(impl Executor + ?Sized), owner: i64, bill_type: i32, enabled: bool) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.bill_autopay (owner_id, bill_type, enabled) VALUES ($1, $2, $3) ON CONFLICT (owner_id, bill_type) DO UPDATE SET enabled = $3",
        &[&owner, &bill_type, &enabled]).await
}

/// Save an owner's configured bill payment division on its own row.
pub(crate) async fn set_division(ex: &(impl Executor + ?Sized), owner: BillOwner, key: i32) -> Result<u64, Error> {
    let (table, column, id) = match owner {
        BillOwner::Corporation(id) => ("game.corporations", "corporation_id", id),
        BillOwner::Alliance(id) => ("game.alliances", "alliance_id", id),
    };
    ex.execute(&format!("UPDATE {table} SET bill_division = $2 WHERE {column} = $1"), &[&id, &key]).await
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
