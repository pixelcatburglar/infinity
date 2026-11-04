//! corpRegistry dispatch persistence for eve/client/script/ui/services/corporation.
//! Reads and writes game.corporations, game.characters and game.corp_applications.
//! Validation, wire conversion and unmodelled corporation features remain in the service.

use crate::db::{self, executor::Executor};

pub(crate) struct Owner {
    pub(crate) character_id: i64,
    pub(crate) name: String,
    pub(crate) gender_id: i64,
}

impl Owner {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self {
            character_id: row.get("character_id"),
            name: row.get("name"),
            gender_id: row.get("gender_id"),
        }
    }
}

struct Recruiting { is_recruiting: bool }
impl Recruiting {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { is_recruiting: row.get("is_recruiting") }
    }
}

struct ApplicationId { application_id: i64 }
impl ApplicationId {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { application_id: row.get("application_id") }
    }
}

struct WelcomeMail { welcome_mail: String }
impl WelcomeMail {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { welcome_mail: row.get("welcome_mail") }
    }
}

pub(crate) async fn recruiting(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<bool>, db::Error> {
    Ok(ex.query_opt("SELECT is_recruiting FROM game.corporations WHERE corporation_id = $1 AND NOT deleted", &[&corp])
        .await?.map(|row| Recruiting::from_row(&row).is_recruiting))
}

pub(crate) async fn insert_application(ex: &(impl Executor + ?Sized), corp: i64, character: i64, text: &str, status: i32, created: i64, open: &[i32]) -> Result<Option<i64>, db::Error> {
    let row = ex.query_opt(
        "INSERT INTO game.corp_applications (corporation_id, character_id, application_text, status, created)
         SELECT $1, $2, $3, $4, $5
         WHERE EXISTS (SELECT 1 FROM game.corporations WHERE corporation_id = $1 AND NOT deleted)
           AND NOT EXISTS (SELECT 1 FROM game.corp_applications WHERE corporation_id = $1 AND character_id = $2 AND status = ANY($6))
         RETURNING application_id",
        &[&corp, &character, &text, &status, &created, &open],
    ).await?;
    Ok(row.map(|row| ApplicationId::from_row(&row).application_id))
}

pub(crate) async fn insert_invitation(ex: &(impl Executor + ?Sized), corp: i64, character: i64, status: i32, created: i64, open: &[i32]) -> Result<Option<i64>, db::Error> {
    let row = ex.query_opt(
        "INSERT INTO game.corp_applications (corporation_id, character_id, application_text, status, created)
         SELECT $1, $2, '', $3, $4
         WHERE EXISTS (SELECT 1 FROM game.characters WHERE character_id = $2 AND NOT deleted AND corporation_id <> $1)
           AND NOT EXISTS (SELECT 1 FROM game.corp_applications WHERE corporation_id = $1 AND character_id = $2 AND status = ANY($5))
         RETURNING application_id",
        &[&corp, &character, &status, &created, &open],
    ).await?;
    Ok(row.map(|row| ApplicationId::from_row(&row).application_id))
}

pub(crate) async fn owners(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Vec<Owner>, db::Error> {
    Ok(ex.query("SELECT character_id, name, gender_id::bigint AS gender_id FROM game.characters WHERE corporation_id = $1 AND NOT deleted", &[&corp])
        .await?.iter().map(Owner::from_row).collect())
}

pub(crate) async fn set_division_names(ex: &(impl Executor + ?Sized), corp: i64, names: &[Option<String>]) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.corporations SET division_names = $2 WHERE corporation_id = $1", &[&corp, &names]).await
}

pub(crate) async fn update_corporation(ex: &(impl Executor + ?Sized), corp: i64, description: &str, url: &str, tax: f64, recruiting: bool, lp_tax: f64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.corporations SET description = $2, url = $3, tax_rate = $4, is_recruiting = $5, lp_tax_rate = $6 WHERE corporation_id = $1", &[&corp, &description, &url, &tax, &recruiting, &lp_tax]).await
}

pub(crate) async fn update_logo(ex: &(impl Executor + ?Sized), corp: i64, shapes: [Option<i32>; 3], colors: [Option<i32>; 3], typeface: Option<i32>, ceo: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.corporations SET shape1 = $2, shape2 = $3, shape3 = $4, color1 = $5, color2 = $6, color3 = $7, typeface = $8
                     WHERE corporation_id = $1 AND ceo_id = $9", &[&corp, &shapes[0], &shapes[1], &shapes[2], &colors[0], &colors[1], &colors[2], &typeface, &ceo]).await
}

pub(crate) async fn welcome_mail(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<String>, db::Error> {
    Ok(ex.query_opt("SELECT welcome_mail FROM game.corporations WHERE corporation_id = $1", &[&corp])
        .await?.map(|row| WelcomeMail::from_row(&row).welcome_mail))
}

pub(crate) async fn set_welcome_mail(ex: &(impl Executor + ?Sized), corp: i64, text: &str) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.corporations SET welcome_mail = $2 WHERE corporation_id = $1 AND NOT deleted", &[&corp, &text]).await
}
