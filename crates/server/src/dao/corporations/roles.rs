//! Corporation roles, titles and CEO persistence for corpRegistry.
//! Mirrors eve/client/script/ui/services/corporation; uses game characters,
//! corporations and corp_titles. Policy and notifications are unsupported.

use crate::db::{self, executor::Executor};

pub(crate) struct Title {
    pub title_id: i64,
    pub name: String,
    pub roles: i64,
    pub grantable_roles: i64,
    pub roles_hq: i64,
    pub grantable_hq: i64,
    pub roles_base: i64,
    pub grantable_base: i64,
    pub roles_other: i64,
    pub grantable_other: i64,
}
impl Title {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self {
            title_id: r.get("title_id"),
            name: r.get("name"),
            roles: r.get("roles"),
            grantable_roles: r.get("grantable_roles"),
            roles_hq: r.get("roles_hq"),
            grantable_hq: r.get("grantable_hq"),
            roles_base: r.get("roles_base"),
            grantable_base: r.get("grantable_base"),
            roles_other: r.get("roles_other"),
            grantable_other: r.get("grantable_other"),
        }
    }
}
struct CharacterId {
    character_id: i64,
}
impl CharacterId {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { character_id: r.get("character_id") }
    }
}

pub(crate) struct Info {
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
    pub corp_title: String,
    pub corp_title_mask: i64,
}
impl Info {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self {
            corporation_id: r.get("corporation_id"),
            alliance_id: r.get("alliance_id"),
            corp_title: r.get("corp_title"),
            corp_title_mask: r.get("corp_title_mask"),
        }
    }
}
pub(crate) struct TitleName {
    pub title_id: i64,
    pub name: String,
}
impl TitleName {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self {
            title_id: r.get("title_id"),
            name: r.get("name"),
        }
    }
}
pub(crate) enum Field {
    Roles,
    Grantable,
    TitleMask,
}
pub(crate) enum Action {
    Add,
    Remove,
    Set,
}

pub(crate) async fn update_member(ex: &(impl Executor + ?Sized), target: i64, roles: Option<i64>, grantable: Option<i64>, title: Option<String>, title_mask: Option<i64>) -> Result<(), db::Error> {
    ex.execute("UPDATE game.characters SET corp_roles = COALESCE($2, corp_roles), corp_grantable_roles = COALESCE($3, corp_grantable_roles),
                    corp_title = COALESCE($4, corp_title), corp_title_mask = COALESCE($5, corp_title_mask)
             WHERE character_id = $1", &[&target, &roles, &grantable, &title, &title_mask]).await?;
    Ok(())
}
pub(crate) async fn action(ex: &(impl Executor + ?Sized), target: i64, field: Field, action: Action, value: i64) -> Result<(), db::Error> {
    let column = match field {
        Field::Roles => "corp_roles",
        Field::Grantable => "corp_grantable_roles",
        Field::TitleMask => "corp_title_mask",
    };
    let expr = match action {
        Action::Add => format!("{column} | $2"),
        Action::Remove => format!("{column} & ~$2"),
        Action::Set => "$2".to_string(),
    };
    ex.execute(&format!("UPDATE game.characters SET {column} = {expr} WHERE character_id = $1"), &[&target, &value]).await?;
    Ok(())
}
pub(crate) async fn titles(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Vec<Title>, db::Error> {
    Ok(ex.query("SELECT title_id, name, roles, grantable_roles, roles_hq, grantable_hq, roles_base, grantable_base, roles_other, grantable_other
             FROM game.corp_titles WHERE corporation_id = $1", &[&corp]).await?.iter().map(Title::from_row).collect())
}
pub(crate) async fn holders(ex: &(impl Executor + ?Sized), corp: i64, mask: i64, active: bool) -> Result<Vec<i64>, db::Error> {
    let sql = if active {
        "SELECT character_id FROM game.characters WHERE corporation_id = $1 AND corp_title_mask & $2 <> 0 AND NOT deleted"
    } else {
        "SELECT character_id FROM game.characters WHERE corporation_id = $1 AND corp_title_mask & $2 <> 0"
    };
    Ok(ex.query(sql, &[&corp, &mask]).await?.iter().map(|r| CharacterId::from_row(r).character_id).collect())
}
pub(crate) async fn save_titles(ex: &(impl Executor + ?Sized), corp: i64, titles: Vec<Title>) -> Result<(), db::Error> {
    for t in &titles {
        ex.execute("INSERT INTO game.corp_titles (corporation_id, title_id, name, roles, grantable_roles, roles_hq, grantable_hq, roles_base, grantable_base, roles_other, grantable_other)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                 ON CONFLICT (corporation_id, title_id) DO UPDATE SET name = $3, roles = $4, grantable_roles = $5, roles_hq = $6, grantable_hq = $7,
                     roles_base = $8, grantable_base = $9, roles_other = $10, grantable_other = $11", &[&corp, &t.title_id, &t.name, &t.roles, &t.grantable_roles, &t.roles_hq, &t.grantable_hq, &t.roles_base, &t.grantable_base, &t.roles_other, &t.grantable_other]).await?;
    }
    Ok(())
}
pub(crate) async fn delete_title(ex: &(impl Executor + ?Sized), corp: i64, title: i64) -> Result<(), db::Error> {
    ex.execute("DELETE FROM game.corp_titles WHERE corporation_id = $1 AND title_id = $2", &[&corp, &title]).await?;
    ex.execute("UPDATE game.characters SET corp_title_mask = corp_title_mask & ~$2 WHERE corporation_id = $1", &[&corp, &title]).await?;
    Ok(())
}
pub(crate) async fn potential_ceos(ex: &(impl Executor + ?Sized), corp: i64, ceo: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("SELECT ch.character_id
             FROM game.characters ch
             WHERE ch.corporation_id = $1 AND ch.character_id <> $2 AND NOT ch.deleted
             ORDER BY ch.character_id", &[&corp, &ceo]).await?.iter().map(|r| r.get("character_id")).collect())
}
pub(crate) async fn transfer_ceo(ex: &(impl Executor + ?Sized), corp: i64, next: i64, mut writes: [(i64, i64); 2]) -> Result<(), db::Error> {
    ex.execute("UPDATE game.corporations SET ceo_id = $2 WHERE corporation_id = $1", &[&corp, &next]).await?;
    writes.sort_by_key(|(id, _)| *id);
    for (id, roles) in writes {
        ex.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&id, &roles]).await?;
    }
    Ok(())
}
pub(crate) async fn info(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Option<Info>, db::Error> {
    Ok(ex.query_opt("SELECT ch.corporation_id, co.alliance_id, ch.corp_title, ch.corp_title_mask FROM game.characters ch
             LEFT JOIN game.corporations co ON co.corporation_id = ch.corporation_id WHERE ch.character_id = $1", &[&char_id]).await?.as_ref().map(Info::from_row))
}
pub(crate) async fn title_names(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Vec<TitleName>, db::Error> {
    Ok(ex.query("SELECT title_id, name FROM game.corp_titles WHERE corporation_id = $1", &[&corp]).await?.iter().map(TitleName::from_row).collect())
}
