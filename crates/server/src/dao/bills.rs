//! Bill persistence for billMgr (eve/client/script/ui/shared/neocom/bill).
//! Uses game.bills, game.bill_autopay, game.corporations and game.offices.
//! Static station data, billing policy, wallet transfers and notifications remain in services.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
const BILL_SELECT: &str = "SELECT bill_id, bill_type, debtor_id, creditor_id, amount::float8, due, interest, external_id, external_id2 FROM game.bills";
pub(crate) struct Bill { pub bill_id: i64, pub bill_type: i32, pub debtor_id: i64, pub creditor_id: i64, pub amount: f64, pub due: i64, pub interest: f64, pub external_id: Option<i64>, pub external_id2: Option<i64> }
impl Bill { fn from_row(r: &Row) -> Self { Self { bill_id: r.get("bill_id"), bill_type: r.get("bill_type"), debtor_id: r.get("debtor_id"), creditor_id: r.get("creditor_id"), amount: r.get("amount"), due: r.get("due"), interest: r.get("interest"), external_id: r.get("external_id"), external_id2: r.get("external_id2") } } }
pub(crate) struct Due { pub office_id: i64, pub corporation_id: i64, pub station_id: i64, pub rented_until: i64 }
impl Due { fn from_row(r: &Row) -> Self { Self { office_id: r.get("office_id"), corporation_id: r.get("corporation_id"), station_id: r.get("station_id"), rented_until: r.get("rented_until") } } }
pub(crate) struct Auto { pub bill_id: i64, pub debtor_id: i64, pub bill_division: i32 }
impl Auto { fn from_row(r: &Row) -> Self { Self { bill_id: r.get("bill_id"), debtor_id: r.get("debtor_id"), bill_division: r.get("bill_division") } } }
pub(crate) struct Setting { pub bill_type: i32, pub enabled: bool }
impl Setting { fn from_row(r: &Row) -> Self { Self { bill_type: r.get("bill_type"), enabled: r.get("enabled") } } }
struct Division { bill_division: i32 }
impl Division { fn from_row(r: &Row) -> Self { Self { bill_division: r.get("bill_division") } } }
pub(crate) async fn list(ex: &(impl Executor + ?Sized), owner: i64, creditor: bool, unpaid: i32) -> Result<Vec<Bill>, Error> { let column = if creditor { "creditor_id" } else { "debtor_id" }; Ok(ex.query(&format!("{BILL_SELECT} WHERE {column} = $1 AND status = $2 ORDER BY due"), &[&owner, &unpaid]).await?.iter().map(Bill::from_row).collect()) }
pub(crate) async fn get(ex: &(impl Executor + ?Sized), bill: i64, unpaid: i32) -> Result<Option<Bill>, Error> { Ok(ex.query_opt(&format!("{BILL_SELECT} WHERE bill_id = $1 AND status = $2"), &[&bill, &unpaid]).await?.as_ref().map(Bill::from_row)) }
pub(crate) async fn set_paid(ex: &(impl Executor + ?Sized), bill: i64, paid: i32) -> Result<u64, Error> { ex.execute("UPDATE game.bills SET status = $2 WHERE bill_id = $1", &[&bill, &paid]).await }
pub(crate) async fn extend_office(ex: &(impl Executor + ?Sized), office: i64, period: i64) -> Result<u64, Error> { ex.execute("UPDATE game.offices SET rented_until = rented_until + $2 WHERE office_id = $1", &[&office, &period]).await }
pub(crate) async fn due(ex: &(impl Executor + ?Sized), now: i64, ahead: i64, kind: i32, unpaid: i32) -> Result<Vec<Due>, Error> { Ok(ex.query("SELECT o.office_id, o.corporation_id, o.station_id, o.rented_until FROM game.offices o
             WHERE o.rented_until - $1 <= $2
               AND NOT EXISTS (SELECT 1 FROM game.bills b WHERE b.bill_type = $3 AND b.external_id = o.office_id AND b.status = $4)", &[&now, &ahead, &kind, &unpaid]).await?.iter().map(Due::from_row).collect()) }
pub(crate) async fn issue(ex: &(impl Executor + ?Sized), kind: i32, corp: i64, landlord: i64, amount: f64, until: i64, office: i64, station: i64, now: i64) -> Result<u64, Error> { ex.execute("INSERT INTO game.bills (bill_type, debtor_id, creditor_id, amount, due, external_id, external_id2, created)
                 VALUES ($1, $2, $3, $4::float8::numeric, $5, $6, $7, $8)", &[&kind, &corp, &landlord, &amount, &until, &office, &station, &now]).await }
pub(crate) async fn automatic(ex: &(impl Executor + ?Sized), unpaid: i32) -> Result<Vec<Auto>, Error> { Ok(ex.query("SELECT b.bill_id, b.debtor_id, co.bill_division FROM game.bills b
             JOIN game.bill_autopay a ON a.owner_id = b.debtor_id AND a.bill_type = b.bill_type AND a.enabled
             JOIN game.corporations co ON co.corporation_id = b.debtor_id
             WHERE b.status = $1", &[&unpaid]).await?.iter().map(Auto::from_row).collect()) }
pub(crate) async fn settings(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Vec<Setting>, Error> { Ok(ex.query("SELECT bill_type, enabled FROM game.bill_autopay WHERE owner_id = $1", &[&corp]).await?.iter().map(Setting::from_row).collect()) }
pub(crate) async fn division(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<i32>, Error> { Ok(ex.query_opt("SELECT bill_division FROM game.corporations WHERE corporation_id = $1", &[&corp]).await?.as_ref().map(|r| Division::from_row(r).bill_division)) }
pub(crate) async fn set_setting(ex: &(impl Executor + ?Sized), corp: i64, kind: i32, enabled: bool) -> Result<u64, Error> { ex.execute("INSERT INTO game.bill_autopay (owner_id, bill_type, enabled) VALUES ($1, $2, $3)
                                     ON CONFLICT (owner_id, bill_type) DO UPDATE SET enabled = $3", &[&corp, &kind, &enabled]).await }
pub(crate) async fn set_division(ex: &(impl Executor + ?Sized), corp: i64, key: i32) -> Result<u64, Error> { ex.execute("UPDATE game.corporations SET bill_division = $2 WHERE corporation_id = $1", &[&corp, &key]).await }
