//! Alliance registry, membership, application and history persistence.
//! Owning services support `eve/client/script/ui/services/alliances` and
//! `eve/client/script/ui/services/corporation`; this DAO has no wire contract.
//! Uses `game.alliances`, `game.corporations`, `game.characters`,
//! `game.alliance_applications` and `game.alliance_history`.
//! Authorization, transactions and notifications remain with callers.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) mod lookup;
pub(crate) mod wars;

struct ExecutorCorp {
    executor_corp_id: Option<i64>,
}
impl ExecutorCorp {
    fn from_row(row: &Row) -> Self {
        Self { executor_corp_id: row.get("executor_corp_id") }
    }
}

pub(crate) async fn executor_corp(ex: &(impl Executor + ?Sized), alliance: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT executor_corp_id FROM game.alliances WHERE alliance_id = $1 AND NOT deleted", &[&alliance])
        .await?.and_then(|row| ExecutorCorp::from_row(&row).executor_corp_id))
}

pub(crate) struct Alliance {
    pub id: i64, pub name: String, pub short_name: String, pub executor: Option<i64>,
    pub creator_corp: i64, pub creator_char: i64, pub description: String, pub url: String,
    pub created: i64, pub dictatorial: bool, pub deleted: bool, pub pilots: i64, pub corporations: i64,
}
impl Alliance {
    fn from_row(r: &Row) -> Self {
        Self { id: r.get("alliance_id"), name: r.get("name"), short_name: r.get("short_name"),
            executor: r.get("executor_corp_id"), creator_corp: r.get("creator_corp_id"),
            creator_char: r.get("creator_char_id"), description: r.get("description"),
            url: r.get("url"), created: r.get("created"), dictatorial: r.get("dictatorial"),
            deleted: r.get("deleted"), pilots: r.get("pilot_count"), corporations: r.get("corp_count") }
    }
}
const ALLIANCE_SELECT: &str = "SELECT a.alliance_id, a.name, a.short_name, a.executor_corp_id, a.creator_corp_id, a.creator_char_id,
        a.description, a.url, a.created, a.dictatorial, a.deleted,
        (SELECT count(*) FROM game.characters ch JOIN game.corporations co ON co.corporation_id = ch.corporation_id
          WHERE co.alliance_id = a.alliance_id AND NOT ch.deleted) AS pilot_count,
        (SELECT count(*) FROM game.corporations co WHERE co.alliance_id = a.alliance_id AND NOT co.deleted) AS corp_count
    FROM game.alliances a";

pub(crate) async fn alliance(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<Alliance>, Error> {
    Ok(ex.query_opt(&format!("{ALLIANCE_SELECT} WHERE a.alliance_id = $1"), &[&id]).await?.map(|r| Alliance::from_row(&r)))
}
pub(crate) async fn ranked(ex: &(impl Executor + ?Sized), limit: i64) -> Result<Vec<Alliance>, Error> {
    Ok(ex.query(&format!("{ALLIANCE_SELECT} WHERE NOT a.deleted ORDER BY 12 DESC, a.alliance_id LIMIT $1"), &[&limit]).await?.iter().map(Alliance::from_row).collect())
}

pub(crate) struct Member { pub corporation: i64, pub chosen_executor: Option<i64>, pub joined: i64 }
impl Member {
    fn from_row(r: &Row) -> Self {
        Self { corporation: r.get("corporation_id"), chosen_executor: r.get("chosen_executor_id"), joined: r.get("alliance_joined") }
    }
}
pub(crate) async fn members(ex: &(impl Executor + ?Sized), alliance: i64) -> Result<Vec<Member>, Error> {
    Ok(ex.query("SELECT corporation_id, chosen_executor_id, COALESCE(alliance_joined, created) AS alliance_joined FROM game.corporations
             WHERE alliance_id = $1 AND NOT deleted ORDER BY corporation_id", &[&alliance]).await?.iter().map(Member::from_row).collect())
}

pub(crate) struct Application { pub alliance: i64, pub corporation: i64, pub text: String, pub created: i64, pub state: i32 }
impl Application {
    fn from_row(r: &Row) -> Self {
        Self { alliance: r.get("alliance_id"), corporation: r.get("corporation_id"), text: r.get("application_text"), created: r.get("created"), state: r.get("state") }
    }
}
const APPLICATION_SELECT: &str = "SELECT alliance_id, corporation_id, application_text, created, state FROM game.alliance_applications";
pub(crate) enum ApplicationFilter { Corporation(i64), Alliance(i64) }
pub(crate) async fn applications(ex: &(impl Executor + ?Sized), filter: ApplicationFilter) -> Result<Vec<Application>, Error> {
    let rows = match filter {
        ApplicationFilter::Corporation(corp) => ex.query(&format!("{APPLICATION_SELECT} WHERE corporation_id = $1"), &[&corp]).await?,
        ApplicationFilter::Alliance(alliance) => ex.query(&format!("{APPLICATION_SELECT} WHERE alliance_id = $1"), &[&alliance]).await?,
    };
    Ok(rows.iter().map(Application::from_row).collect())
}
pub(crate) async fn application(ex: &(impl Executor + ?Sized), alliance: i64, corp: i64) -> Result<Option<Application>, Error> {
    Ok(ex.query_opt(&format!("{APPLICATION_SELECT} WHERE alliance_id = $1 AND corporation_id = $2"), &[&alliance, &corp]).await?.map(|r| Application::from_row(&r)))
}
/// Lock a corporation's application rows after the common admission locks.
pub(crate) async fn lock_applications(ex: &(impl Executor + ?Sized), corp: i64) -> Result<(), Error> {
    ex.query("SELECT alliance_id FROM game.alliance_applications WHERE corporation_id = $1 ORDER BY alliance_id FOR UPDATE", &[&corp]).await?;
    Ok(())
}


struct CharacterId { id: i64 }
impl CharacterId { fn from_row(r: &Row) -> Self { Self { id: r.get("character_id") } } }
pub(crate) async fn pilots(ex: &(impl Executor + ?Sized), alliance: Option<i64>, corp: Option<i64>) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT ch.character_id FROM game.characters ch JOIN game.corporations co ON co.corporation_id = ch.corporation_id
             WHERE NOT ch.deleted AND (co.alliance_id = $1 OR co.corporation_id = $2)", &[&alliance, &corp]).await?.iter().map(|r| CharacterId::from_row(r).id).collect())
}
pub(crate) async fn set_corporation_alliance(ex: &(impl Executor + ?Sized), corp: i64, alliance: Option<i64>, now: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.corporations SET alliance_id = $2, alliance_joined = $3, chosen_executor_id = NULL WHERE corporation_id = $1 AND ($2::bigint IS NULL OR (alliance_id IS NULL AND NOT deleted))", &[&corp, &alliance, &alliance.map(|_| now)]).await
}
pub(crate) async fn insert_history(ex: &(impl Executor + ?Sized), corp: i64, alliance: Option<i64>, now: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.alliance_history (corporation_id, alliance_id, start_date) VALUES ($1, $2, $3)", &[&corp, &alliance, &now]).await
}
struct AllianceId { id: Option<i64> }
impl AllianceId { fn from_row(r: &Row) -> Self { Self { id: r.get("alliance_id") } } }
pub(crate) async fn corporation_alliance(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<i64>, Error> {
    Ok(AllianceId::from_row(&ex.query_one("SELECT alliance_id FROM game.corporations WHERE corporation_id = $1", &[&corp]).await?).id)
}
pub(crate) async fn live_corporation_alliance(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<Option<i64>>, Error> {
    Ok(ex.query_opt("SELECT alliance_id FROM game.corporations WHERE corporation_id = $1 AND NOT deleted", &[&corp]).await?.map(|r| AllianceId::from_row(&r).id))
}

pub(crate) struct LiveCorporationAlliance {
    pub(crate) corporation_id: i64,
    pub(crate) alliance_id: Option<i64>,
}
impl LiveCorporationAlliance {
    fn from_row(r: &Row) -> Self {
        Self { corporation_id: r.get("corporation_id"), alliance_id: r.get("alliance_id") }
    }
}
pub(crate) async fn live_corporation_alliances(ex: &(impl Executor + ?Sized), corps: &[i64]) -> Result<Vec<LiveCorporationAlliance>, Error> {
    Ok(ex.query("SELECT corporation_id, alliance_id FROM game.corporations WHERE corporation_id = ANY($1) AND NOT deleted ORDER BY corporation_id", &[&corps])
        .await?.iter().map(LiveCorporationAlliance::from_row).collect())
}
struct Taken { value: bool }
impl Taken { fn from_row(r: &Row) -> Self { Self { value: r.get("taken") } } }
pub(crate) async fn name_taken(ex: &(impl Executor + ?Sized), name: &str, short_name: &str) -> Result<bool, Error> {
    Ok(Taken::from_row(&ex.query_one("SELECT EXISTS (SELECT 1 FROM game.alliances WHERE NOT deleted AND (lower(name) = lower($1) OR upper(short_name) = $2)) AS taken", &[&name, &short_name]).await?).value)
}
struct CreatedAlliance { id: i64 }
impl CreatedAlliance { fn from_row(r: &Row) -> Self { Self { id: r.get("alliance_id") } } }
pub(crate) async fn create(ex: &(impl Executor + ?Sized), name: &str, short_name: &str, description: &str, url: &str, corp: i64, me: i64, now: i64) -> Result<i64, Error> {
    Ok(CreatedAlliance::from_row(&ex.query_one("INSERT INTO game.alliances (name, short_name, description, url, executor_corp_id, creator_corp_id, creator_char_id, created)
                     VALUES ($1, $2, $3, $4, $5, $5, $6, $7) RETURNING alliance_id", &[&name, &short_name, &description, &url, &corp, &me, &now]).await?).id)
}
pub(crate) async fn delete_other_applications(ex: &(impl Executor + ?Sized), corp: i64, alliance: i64, open_state: i32) -> Result<Vec<Application>, Error> {
    Ok(ex.query("DELETE FROM game.alliance_applications WHERE corporation_id = $1 AND alliance_id <> $2 AND state = $3 RETURNING alliance_id, corporation_id, application_text, created, state", &[&corp, &alliance, &open_state]).await?.iter().map(Application::from_row).collect())
}

pub(crate) struct CorporationMembership { pub alliance: i64, pub executor: Option<i64> }
impl CorporationMembership {
    fn from_row(r: &Row) -> Self { Self { alliance: r.get("alliance_id"), executor: r.get("executor_corp_id") } }
}
pub(crate) async fn corporation_membership(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<CorporationMembership>, Error> {
    Ok(ex.query_opt("SELECT co.alliance_id, a.executor_corp_id FROM game.corporations co JOIN game.alliances a ON a.alliance_id = co.alliance_id
             WHERE co.corporation_id = $1", &[&corp]).await?.map(|r| CorporationMembership::from_row(&r)))
}
pub(crate) async fn clear_support(ex: &(impl Executor + ?Sized), alliance: i64, corp: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.corporations SET chosen_executor_id = NULL WHERE alliance_id = $1 AND chosen_executor_id = $2", &[&alliance, &corp]).await
}
struct CorporationId { id: i64 }
impl CorporationId { fn from_row(r: &Row) -> Self { Self { id: r.get("corporation_id") } } }
pub(crate) async fn next_executor(ex: &(impl Executor + ?Sized), alliance: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT co.corporation_id FROM game.corporations co WHERE co.alliance_id = $1 AND NOT co.deleted
                 ORDER BY (SELECT count(*) FROM game.characters ch WHERE ch.corporation_id = co.corporation_id AND NOT ch.deleted) DESC, co.corporation_id
                 LIMIT 1", &[&alliance]).await?.map(|r| CorporationId::from_row(&r).id))
}
pub(crate) async fn set_executor(ex: &(impl Executor + ?Sized), alliance: i64, corp: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.alliances SET executor_corp_id = $2 WHERE alliance_id = $1", &[&alliance, &corp]).await
}
pub(crate) async fn close_alliance(ex: &(impl Executor + ?Sized), alliance: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.alliances SET deleted = true, executor_corp_id = NULL WHERE alliance_id = $1", &[&alliance]).await
}
pub(crate) async fn delete_alliance_applications(ex: &(impl Executor + ?Sized), alliance: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.alliance_applications WHERE alliance_id = $1", &[&alliance]).await
}
pub(crate) async fn support(ex: &(impl Executor + ?Sized), corp: i64, candidate: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.corporations SET chosen_executor_id = $2 WHERE corporation_id = $1", &[&corp, &candidate]).await
}
pub(crate) struct Votes { pub backing: i64, pub total: i64 }
impl Votes { fn from_row(r: &Row) -> Self { Self { backing: r.get("backing"), total: r.get("total") } } }
pub(crate) async fn votes(ex: &(impl Executor + ?Sized), alliance: i64, candidate: i64) -> Result<Votes, Error> {
    Ok(Votes::from_row(&ex.query_one("SELECT COALESCE(sum(n) FILTER (WHERE co.chosen_executor_id = $2), 0)::bigint AS backing, COALESCE(sum(n), 0)::bigint AS total
             FROM game.corporations co
             CROSS JOIN LATERAL (SELECT count(*) AS n FROM game.characters ch WHERE ch.corporation_id = co.corporation_id AND NOT ch.deleted) m
             WHERE co.alliance_id = $1 AND NOT co.deleted", &[&alliance, &candidate]).await?))
}
pub(crate) async fn elect_executor(ex: &(impl Executor + ?Sized), alliance: i64, candidate: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.alliances SET executor_corp_id = $2 WHERE alliance_id = $1 AND executor_corp_id IS DISTINCT FROM $2", &[&alliance, &candidate]).await
}

pub(crate) async fn exists(ex: &(impl Executor + ?Sized), alliance: i64) -> Result<bool, Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.alliances WHERE alliance_id = $1 AND NOT deleted", &[&alliance]).await?.is_some())
}
pub(crate) async fn store_application(ex: &(impl Executor + ?Sized), alliance: i64, corp: i64, text: &str, state: i32, now: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.alliance_applications (alliance_id, corporation_id, application_text, state, created) VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (alliance_id, corporation_id) DO NOTHING", &[&alliance, &corp, &text, &state, &now]).await
}
pub(crate) async fn delete_application(ex: &(impl Executor + ?Sized), alliance: i64, corp: i64, state: i32) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.alliance_applications WHERE alliance_id = $1 AND corporation_id = $2 AND state = $3", &[&alliance, &corp, &state]).await
}
pub(crate) async fn update_application(ex: &(impl Executor + ?Sized), alliance: i64, corp: i64, text: &str, state: i32, expected: i32) -> Result<u64, Error> {
    ex.execute("UPDATE game.alliance_applications SET application_text = $3, state = $4 WHERE alliance_id = $1 AND corporation_id = $2 AND state = $5", &[&alliance, &corp, &text, &state, &expected]).await
}
pub(crate) async fn update_alliance(ex: &(impl Executor + ?Sized), alliance: i64, description: &str, url: &str) -> Result<u64, Error> {
    ex.execute("UPDATE game.alliances SET description = $2, url = $3 WHERE alliance_id = $1", &[&alliance, &description, &url]).await
}
pub(crate) struct Employment { pub alliance: Option<i64>, pub start: i64, pub deleted: bool }
impl Employment {
    fn from_row(r: &Row) -> Self { Self { alliance: r.get("alliance_id"), start: r.get("start_date"), deleted: r.get("deleted") } }
}
pub(crate) async fn employment(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Vec<Employment>, Error> {
    Ok(ex.query("SELECT h.alliance_id, h.start_date, COALESCE(a.deleted, false) AS deleted FROM game.alliance_history h
                     LEFT JOIN game.alliances a ON a.alliance_id = h.alliance_id WHERE h.corporation_id = $1
                     UNION ALL SELECT NULL, created, false FROM game.corporations WHERE corporation_id = $1
                     ORDER BY 2 DESC", &[&corp]).await?.iter().map(Employment::from_row).collect())
}
pub(crate) struct Relationship { pub to: i64, pub value: i32 }
impl Relationship { fn from_row(r: &Row) -> Self { Self { to: r.get("to_id"), value: r.get("relationship") } } }
struct RelationshipValue { value: i32 }
impl RelationshipValue { fn from_row(r: &Row) -> Self { Self { value: r.get("relationship") } } }
pub(crate) async fn relationships(ex: &(impl Executor + ?Sized), alliance: i64) -> Result<Vec<Relationship>, Error> {
    Ok(ex.query("SELECT to_id, relationship FROM game.alliance_relationships WHERE alliance_id = $1", &[&alliance]).await?.iter().map(Relationship::from_row).collect())
}
pub(crate) async fn relationship(ex: &(impl Executor + ?Sized), alliance: i64, to: i64) -> Result<Option<i32>, Error> {
    Ok(ex.query_opt("SELECT relationship FROM game.alliance_relationships WHERE alliance_id = $1 AND to_id = $2", &[&alliance, &to]).await?.map(|r| RelationshipValue::from_row(&r).value))
}
pub(crate) async fn set_relationship(ex: &(impl Executor + ?Sized), alliance: i64, to: i64, value: i32) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.alliance_relationships (alliance_id, to_id, relationship) VALUES ($1, $2, $3)
                             ON CONFLICT (alliance_id, to_id) DO UPDATE SET relationship = $3", &[&alliance, &to, &value]).await
}
pub(crate) async fn delete_relationship(ex: &(impl Executor + ?Sized), alliance: i64, to: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.alliance_relationships WHERE alliance_id = $1 AND to_id = $2", &[&alliance, &to]).await
}
struct BillAmount { amount: f64 }
impl BillAmount { fn from_row(r: &Row) -> Self { Self { amount: r.get("amount") } } }
pub(crate) async fn bill_amount(ex: &(impl Executor + ?Sized), bill: i64, alliance: i64) -> Result<Option<f64>, Error> {
    Ok(ex.query_opt("SELECT amount::float8 AS amount FROM game.bills WHERE bill_id = $1 AND debtor_id = $2", &[&bill, &alliance]).await?.map(|r| BillAmount::from_row(&r).amount))
}
