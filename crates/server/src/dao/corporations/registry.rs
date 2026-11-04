//! Corporation registry persistence in game corporations, characters,
//! employment history, applications and items.
//! Client behavior is owned by corpRegistry (eve/client/script/ui/services/corporation);
//! this module does not implement policy, notifications or transactions.

use crate::dao::characters::roles::EFFECTIVE_ROLES_SQL;
use crate::db::{self, executor::Executor};

pub(crate) struct Membership {
    pub(crate) corporation_id: i64,
    pub(crate) effective_roles: i64,
}

impl Membership {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { corporation_id: row.get("corporation_id"), effective_roles: row.get("effective_roles") }
    }
}

pub(crate) async fn membership(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Membership, db::Error> {
    let row = ex.query_one(&format!("SELECT ch.corporation_id, {EFFECTIVE_ROLES_SQL} AS effective_roles FROM game.characters ch WHERE ch.character_id = $1"), &[&char_id]).await?;
    Ok(Membership::from_row(&row))
}
/// Lock the union of explicit and corporation-derived alliances in ascending ID order,
/// before corporations, characters and applications in their respective ascending orders.
/// Return false if a corporation's alliance association changed while acquiring locks.
/// Missing explicit alliances are tolerated; callers decide whether they must exist.
pub(crate) async fn lock_admission(ex: &(impl Executor + ?Sized), alliances: &[i64], corps: &[i64], characters: &[i64]) -> Result<bool, db::Error> {
    let before = ex.query("SELECT corporation_id, alliance_id FROM game.corporations WHERE corporation_id = ANY($1) ORDER BY corporation_id", &[&corps]).await?;
    let mut alliances: Vec<i64> = alliances.iter().copied().chain(before.iter().filter_map(|r| r.get::<_, Option<i64>>("alliance_id"))).collect();
    alliances.sort_unstable();
    alliances.dedup();
    ex.query("SELECT alliance_id FROM game.alliances WHERE alliance_id = ANY($1) ORDER BY alliance_id FOR UPDATE", &[&alliances]).await?;
    let after = ex.query("SELECT corporation_id, alliance_id FROM game.corporations WHERE corporation_id = ANY($1) ORDER BY corporation_id FOR UPDATE", &[&corps]).await?;
    if before.len() != after.len() || before.iter().zip(&after).any(|(a, b)| a.get::<_, i64>("corporation_id") != b.get::<_, i64>("corporation_id") || a.get::<_, Option<i64>>("alliance_id") != b.get::<_, Option<i64>>("alliance_id")) {
        return Ok(false);
    }
    ex.query("SELECT character_id FROM game.characters WHERE character_id = ANY($1) ORDER BY character_id FOR UPDATE", &[&characters]).await?;
    ex.query("SELECT application_id FROM game.corp_applications WHERE character_id = ANY($1) ORDER BY application_id FOR UPDATE", &[&characters]).await?;
    Ok(true)
}

pub(crate) async fn active_character(ex: &(impl Executor + ?Sized), id: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT character_id FROM game.characters WHERE character_id = $1 AND NOT deleted", &[&id]).await?.is_some())
}
pub(crate) async fn character_corp(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<i64>, db::Error> {
    Ok(ex.query_opt("SELECT corporation_id FROM game.characters WHERE character_id = $1 AND NOT deleted", &[&id]).await?.map(|r| r.get("corporation_id")))
}


struct CeoRow { ceo_id: Option<i64> }
impl CeoRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { ceo_id: row.get("ceo_id") }
    }
}

pub(crate) struct HeadquartersRow {
    pub(crate) station_id: Option<i64>,
    pub(crate) alliance_id: Option<i64>,
}
impl HeadquartersRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { station_id: row.get("station_id"), alliance_id: row.get("alliance_id") }
    }
}

pub(crate) struct SchoolRow { pub(crate) school_id: Option<i32>, pub(crate) race_id: i32 }
impl SchoolRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { school_id: row.get("school_id"), race_id: row.get("race_id") }
    }
}

struct CountRow { count: i64 }
impl CountRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { count: row.get("count") }
    }
}

pub(crate) struct TakenRow { pub(crate) taken: bool, pub(crate) lower_name: String }
impl TakenRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { taken: row.get("taken"), lower_name: row.get("lower_name") }
    }
}

struct CreatedRow { corporation_id: i64 }
impl CreatedRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { corporation_id: row.get("corporation_id") }
    }
}

pub(crate) async fn ceo_of(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<i64>, db::Error> {
    Ok(ex.query_opt("SELECT ceo_id FROM game.corporations WHERE corporation_id = $1 AND NOT deleted", &[&corp]).await?.as_ref().and_then(|row| CeoRow::from_row(row).ceo_id))
}

pub(crate) async fn headquarters(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<HeadquartersRow>, db::Error> {
    Ok(ex.query_opt("SELECT station_id, alliance_id FROM game.corporations WHERE corporation_id = $1", &[&corp]).await?
        .as_ref().map(HeadquartersRow::from_row))
}

pub(crate) async fn set_corp(ex: &(impl Executor + ?Sized), char_id: i64, corp: i64, roles: i64, now: i64) -> Result<bool, db::Error> {
    let changed = ex.execute(
        "UPDATE game.characters SET corporation_id = $2, corp_roles = $3, corp_grantable_roles = 0, corp_title_mask = 0, corp_title = ''
             WHERE character_id = $1 AND corporation_id IS DISTINCT FROM $2",
        &[&char_id, &corp, &roles],
    ).await?;
    if changed == 0 { return Ok(false); }
    ex.execute("INSERT INTO game.employment_history (char_id, corporation_id, start_date) VALUES ($1, $2, $3)", &[&char_id, &corp, &now]).await?;
    Ok(true)
}

pub(crate) async fn school(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<SchoolRow, db::Error> {
    let row = ex.query_one("SELECT school_id, race_id FROM game.characters WHERE character_id = $1", &[&char_id]).await?;
    Ok(SchoolRow::from_row(&row))
}

pub(crate) async fn other_member_count(ex: &(impl Executor + ?Sized), corp: i64, char_id: i64) -> Result<i64, db::Error> {
    Ok(CountRow::from_row(&ex.query_one("SELECT count(*) AS count FROM game.characters WHERE corporation_id = $1 AND character_id <> $2 AND NOT deleted", &[&corp, &char_id]).await?).count)
}

pub(crate) async fn close_corp(ex: &(impl Executor + ?Sized), corp: i64) -> Result<(), db::Error> {
    ex.execute("UPDATE game.corporations SET deleted = true, ceo_id = NULL WHERE corporation_id = $1", &[&corp]).await?;
    Ok(())
}

pub(crate) async fn name_taken(ex: &(impl Executor + ?Sized), name: &str, ticker: &str) -> Result<TakenRow, db::Error> {
    Ok(TakenRow::from_row(&ex.query_one(
        "SELECT EXISTS (SELECT 1 FROM game.corporations WHERE NOT deleted AND (lower(name) = lower($1) OR upper(ticker) = $2)) AS taken, lower($1) AS lower_name",
        &[&name, &ticker],
    ).await?))
}

pub(crate) struct NewCorporation<'a> {
    pub(crate) name: &'a str,
    pub(crate) ticker: &'a str,
    pub(crate) description: &'a str,
    pub(crate) url: &'a str,
    pub(crate) tax: f64,
    pub(crate) lp_tax: f64,
    pub(crate) ceo: i64,
    pub(crate) station: i64,
    pub(crate) recruiting: bool,
    pub(crate) friendly: bool,
    pub(crate) shapes: [Option<i32>; 7],
    pub(crate) now: i64,
    pub(crate) member_limit: Option<i64>,
}

pub(crate) async fn create(ex: &(impl Executor + ?Sized), corp: &NewCorporation<'_>) -> Result<i64, db::Error> {
    Ok(CreatedRow::from_row(&ex.query_one(
        "INSERT INTO game.corporations (name, ticker, description, url, tax_rate, ceo_id, creator_id, station_id, is_recruiting, friendly_fire,
                                                    shape1, shape2, shape3, color1, color2, color3, typeface, created, member_limit, lp_tax_rate)
                     VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) RETURNING corporation_id",
        &[&corp.name, &corp.ticker, &corp.description, &corp.url, &corp.tax, &corp.ceo, &corp.station, &corp.recruiting, &corp.friendly,
          &corp.shapes[0], &corp.shapes[1], &corp.shapes[2], &corp.shapes[3], &corp.shapes[4], &corp.shapes[5], &corp.shapes[6], &corp.now, &corp.member_limit, &corp.lp_tax],
    ).await?).corporation_id)
}

pub(crate) struct Application {
    pub(crate) application_id: i64,
    pub(crate) corporation_id: i64,
    pub(crate) character_id: i64,
    pub(crate) application_text: String,
    pub(crate) status: i32,
    pub(crate) created: i64,
    pub(crate) custom_message: String,
}

impl Application {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { application_id: r.get("application_id"), corporation_id: r.get("corporation_id"), character_id: r.get("character_id"), application_text: r.get("application_text"), status: r.get("status"), created: r.get("created"), custom_message: r.get("custom_message") }
    }
}

const APPLICATION_SELECT: &str = "SELECT application_id, corporation_id, character_id, application_text, status, created, custom_message FROM game.corp_applications";

pub(crate) enum ApplicationFilter { Character, Corporation }

pub(crate) async fn applications(ex: &(impl Executor + ?Sized), filter: ApplicationFilter, arg: i64, open: &[i32]) -> Result<Vec<Application>, db::Error> {
    let where_sql = match filter { ApplicationFilter::Character => "character_id = $1", ApplicationFilter::Corporation => "corporation_id = $1" };
    Ok(ex.query(&format!("{APPLICATION_SELECT} WHERE {where_sql} AND status = ANY($2) ORDER BY application_id"), &[&arg, &open]).await?.iter().map(Application::from_row).collect())
}

pub(crate) async fn old_applications(ex: &(impl Executor + ?Sized), character: i64, open: &[i32], limit: i64) -> Result<Vec<Application>, db::Error> {
    Ok(ex.query(&format!("{APPLICATION_SELECT} WHERE character_id = $1 AND NOT (status = ANY($2)) ORDER BY application_id DESC LIMIT $3"), &[&character, &open, &limit]).await?.iter().map(Application::from_row).collect())
}
pub(crate) async fn old_corporation_applications(ex: &(impl Executor + ?Sized), corp: i64, open: &[i32]) -> Result<Vec<Application>, db::Error> {
    Ok(ex.query(&format!("{APPLICATION_SELECT} WHERE corporation_id = $1 AND NOT (status = ANY($2)) ORDER BY application_id DESC"), &[&corp, &open]).await?.iter().map(Application::from_row).collect())
}

pub(crate) async fn application_managers(ex: &(impl Executor + ?Sized), corp: i64, roles: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query(&format!("SELECT ch.character_id FROM game.characters ch WHERE ch.corporation_id = $1 AND NOT ch.deleted AND ({EFFECTIVE_ROLES_SQL} & $2) <> 0 ORDER BY ch.character_id"), &[&corp, &roles]).await?.iter().map(|r| r.get("character_id")).collect())
}


pub(crate) async fn application(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<Application>, db::Error> {
    Ok(ex.query_opt(&format!("{APPLICATION_SELECT} WHERE application_id = $1"), &[&id]).await?.as_ref().map(Application::from_row))
}

pub(crate) async fn update_application(ex: &(impl Executor + ?Sized), id: i64, status: i32, message: &str, current: i32) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.corp_applications SET status = $2, custom_message = $3 WHERE application_id = $1 AND status = $4", &[&id, &status, &message, &current]).await
}

pub(crate) async fn withdraw_applications(ex: &(impl Executor + ?Sized), applicant: i64, id: i64, status: i32, open: &[i32]) -> Result<Vec<Application>, db::Error> {
    Ok(ex.query("UPDATE game.corp_applications SET status = $3 WHERE character_id = $1 AND application_id <> $2 AND status = ANY($4) RETURNING application_id, corporation_id, character_id, application_text, status, created, custom_message", &[&applicant, &id, &status, &open]).await?.iter().map(Application::from_row).collect())
}

pub(crate) enum MemberFilter { All, Include, Exclude }

pub(crate) struct Member {
    pub(crate) character_id: i64,
    pub(crate) roles: i64,
    pub(crate) start_date: i64,
    pub(crate) station_id: Option<i64>,
    pub(crate) solar_system_id: Option<i64>,
    pub(crate) logoff_date: i64,
    pub(crate) ship_type_id: Option<i32>,
    pub(crate) grantable_roles: i64,
    pub(crate) title_mask: i64,
    pub(crate) title: String,
}

impl Member {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { character_id: r.get("character_id"), roles: r.get("corp_roles"), start_date: r.get("start_date"), station_id: r.get("station_id"), solar_system_id: r.get("solar_system_id"), logoff_date: r.get("logoff_date"), ship_type_id: r.get("ship_type_id"), grantable_roles: r.get("corp_grantable_roles"), title_mask: r.get("corp_title_mask"), title: r.get("corp_title") }
    }
}

const MEMBER_SELECT: &str = "SELECT ch.character_id, ch.corp_roles,
        COALESCE((SELECT max(e.start_date) FROM game.employment_history e WHERE e.char_id = ch.character_id AND e.corporation_id = ch.corporation_id),
                 (extract(epoch FROM ch.created_at) * $3::bigint)::bigint + $4::bigint) AS start_date,
        ch.station_id, ch.solar_system_id, (extract(epoch FROM ch.logoff_at) * $3::bigint)::bigint + $4::bigint AS logoff_date,
        (SELECT type_id FROM game.items WHERE item_id = ch.ship_id) AS ship_type_id, ch.corp_grantable_roles, ch.corp_title_mask, ch.corp_title
    FROM game.characters ch";

pub(crate) async fn members(ex: &(impl Executor + ?Sized), corp: i64, filter: MemberFilter, ids: &[i64], second: i64, epoch: i64) -> Result<Vec<Member>, db::Error> {
    let extra = match filter { MemberFilter::All => "AND $2::bigint[] IS NOT NULL", MemberFilter::Include => "AND ch.character_id = ANY($2)", MemberFilter::Exclude => "AND ch.character_id <> ALL($2)" };
    let sql = format!("{MEMBER_SELECT} WHERE ch.corporation_id = $1 AND NOT ch.deleted {extra} ORDER BY ch.character_id");
    Ok(ex.query(&sql, &[&corp, &ids, &second, &epoch]).await?.iter().map(Member::from_row).collect())
}

pub(crate) struct Employment {
    pub(crate) corporation_id: i64,
    pub(crate) start_date: i64,
}

impl Employment {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { corporation_id: row.get("corporation_id"), start_date: row.get("start_date") }
    }
}

pub(crate) async fn employment(ex: &(impl Executor + ?Sized), char_id: i64, second: i64, epoch: i64) -> Result<Vec<Employment>, db::Error> {
    Ok(ex.query(
        "SELECT corporation_id, start_date FROM game.employment_history WHERE char_id = $1
             UNION ALL
             SELECT ch.corporation_id, (extract(epoch FROM ch.created_at) * $2::bigint)::bigint + $3::bigint
             FROM game.characters ch WHERE ch.character_id = $1 AND NOT EXISTS (SELECT 1 FROM game.employment_history WHERE char_id = $1)
             ORDER BY 2 DESC",
        &[&char_id, &second, &epoch],
    ).await?.iter().map(Employment::from_row).collect())
}

pub(crate) struct Corporation {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) ticker: String,
    pub(crate) url: String,
    pub(crate) tax_rate: f64,
    pub(crate) lp_tax_rate: f64,
    pub(crate) creator_id: i64,
    pub(crate) ceo_id: Option<i64>,
    pub(crate) station_id: i64,
    pub(crate) is_recruiting: bool,
    pub(crate) friendly_fire: bool,
    pub(crate) logo: [Option<i64>; 7],
    pub(crate) created: i64,
    pub(crate) deleted: bool,
    pub(crate) member_count: i64,
    pub(crate) member_limit: Option<i64>,
    pub(crate) alliance_id: Option<i64>,
    pub(crate) chosen_executor_id: Option<i64>,
    pub(crate) division_names: Option<Vec<Option<String>>>,
}

impl Corporation {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self {
            name: r.get("name"), description: r.get("description"), ticker: r.get("ticker"), url: r.get("url"),
            tax_rate: r.get("tax_rate"), lp_tax_rate: r.get("lp_tax_rate"), creator_id: r.get("creator_id"),
            ceo_id: r.get("ceo_id"), station_id: r.get("station_id"), is_recruiting: r.get("is_recruiting"),
            friendly_fire: r.get("friendly_fire"), logo: [r.get("shape1"), r.get("shape2"), r.get("shape3"), r.get("color1"), r.get("color2"), r.get("color3"), r.get("typeface")],
            created: r.get("created"), deleted: r.get("deleted"), member_count: r.get("member_count"), member_limit: r.get("member_limit"),
            alliance_id: r.get("alliance_id"), chosen_executor_id: r.get("chosen_executor_id"), division_names: r.get("division_names"),
        }
    }
}

pub(crate) async fn corporation(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<Corporation>, db::Error> {
    Ok(ex.query_opt(
        "SELECT name, description, ticker, url, tax_rate, lp_tax_rate, creator_id, ceo_id, station_id, is_recruiting, friendly_fire,
                    shape1::bigint AS shape1, shape2::bigint AS shape2, shape3::bigint AS shape3, color1::bigint AS color1, color2::bigint AS color2, color3::bigint AS color3, typeface::bigint AS typeface, created, deleted,
                    (SELECT count(*) FROM game.characters ch WHERE ch.corporation_id = $1 AND NOT ch.deleted) AS member_count, member_limit, alliance_id, chosen_executor_id, division_names
             FROM game.corporations WHERE corporation_id = $1",
        &[&corp],
    ).await?.as_ref().map(Corporation::from_row))
}

/// Callers lock the corporation row before reading or changing its ability snapshot.
pub(crate) async fn capacity(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<(Option<i64>, Option<i64>, i64)>, db::Error> {
    Ok(ex.query_opt("SELECT ceo_id, member_limit, (SELECT count(*) FROM game.characters WHERE corporation_id = $1 AND NOT deleted) AS member_count FROM game.corporations WHERE corporation_id = $1 AND NOT deleted", &[&corp])
        .await?.map(|r| (r.get("ceo_id"), r.get("member_limit"), r.get("member_count"))))
}

pub(crate) async fn set_member_limit(ex: &(impl Executor + ?Sized), corp: i64, ceo: i64, limit: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.corporations SET member_limit = $3 WHERE corporation_id = $1 AND ceo_id = $2 AND NOT deleted", &[&corp, &ceo, &limit]).await
}
