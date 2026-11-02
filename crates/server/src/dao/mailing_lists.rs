//! Mailing-list persistence for `mailingListsMgr`, using `game.mailing_lists`,
//! `game.mailing_list_members` and `game.mailing_list_access`. Authorization,
//! transactions and delivery are service responsibilities.
//! Name normalization follows `carbon/common/script/util/format.py`'s
//! `GetKeyAndNormalize` and `CaseFold`. PostgreSQL's Unicode case mappings
//! depend on its locale/version and differ from Python 2, notably for U+0130
//! and sharp s; exact Python Unicode parity is not supported.

use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

#[derive(Clone)]
pub(crate) struct List {
    pub list_id: i64,
    pub name_key: String,
    pub display_name: String,
    pub owner_id: i64,
    pub default_access: i16,
    pub default_member_access: i16,
    pub welcome_title: Option<String>,
    pub welcome_body: Option<String>,
    pub deleted_at: Option<i64>,
}
impl List {
    fn from_row(row: &Row) -> Self {
        Self {
            list_id: row.get("list_id"), name_key: row.get("name_key"),
            display_name: row.get("display_name"), owner_id: row.get("owner_id"),
            default_access: row.get("default_access"), default_member_access: row.get("default_member_access"),
            welcome_title: row.get("welcome_title"), welcome_body: row.get("welcome_body"),
            deleted_at: row.get("deleted_at"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Member { pub character_id: i64, pub member_access: i16 }
impl Member {
    fn from_row(row: &Row) -> Self { Self { character_id: row.get("character_id"), member_access: row.get("member_access") } }
}
pub(crate) struct Access { pub entity_id: i64, pub entity_kind: i16, pub access: i16 }
impl Access {
    fn from_row(row: &Row) -> Self { Self { entity_id: row.get("entity_id"), entity_kind: row.get("entity_kind"), access: row.get("access") } }
}
struct Normalized { key: String, display: String }
impl Normalized {
    fn from_row(row: &Row) -> Self { Self { key: row.get("key"), display: row.get("display") } }
}
struct Count { count: i64 }
impl Count { fn from_row(row: &Row) -> Self { Self { count: row.get("count") } } }
struct CharacterId { character_id: i64 }
impl CharacterId { fn from_row(row: &Row) -> Self { Self { character_id: row.get("character_id") } } }
struct MemberAccess { member_access: i16 }
impl MemberAccess { fn from_row(row: &Row) -> Self { Self { member_access: row.get("member_access") } } }

// Python 2 unicode.isspace() characters used by the client's formatter.
const WHITESPACE: &str = "\u{0009}\u{000a}\u{000b}\u{000c}\u{000d}\u{001c}\u{001d}\u{001e}\u{001f}\u{0020}\u{0085}\u{00a0}\u{1680}\u{180e}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}";

pub(crate) async fn normalize_name(ex: &(impl Executor + ?Sized), name: &str) -> Result<(String, String), Error> {
    let row = ex.query_one(
        "WITH RECURSIVE folded(value) AS (
             SELECT reverse(split_part(reverse(translate($1::text, $2, '')), chr(92), 1)) UNION ALL
             SELECT lower(upper(value)) FROM folded WHERE value COLLATE \"C\" <> lower(upper(value)) COLLATE \"C\"
         ), chars AS (
             SELECT pos, substr($1::text, pos, 1) AS ch,
                    lag(substr($1::text, pos, 1)) OVER (ORDER BY pos) AS previous
             FROM generate_series(1, char_length($1::text)) AS pos
         )
         SELECT (SELECT value FROM folded WHERE value COLLATE \"C\" = lower(upper(value)) COLLATE \"C\") AS key,
                COALESCE((SELECT string_agg(ch, '' ORDER BY pos) FROM chars
                          WHERE position(ch in $2) = 0 OR ch IS DISTINCT FROM previous), '') AS display",
        &[&name, &WHITESPACE],
    ).await?;
    let row = Normalized::from_row(&row);
    Ok((row.key, row.display))
}

pub(crate) async fn get_info(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<List>, Error> {
    Ok(ex.query_opt("SELECT * FROM game.mailing_lists WHERE list_id = $1", &[&id]).await?.as_ref().map(List::from_row))
}
pub(crate) async fn find_active_by_key(ex: &(impl Executor + ?Sized), key: &str) -> Result<Option<List>, Error> {
    Ok(ex.query_opt("SELECT * FROM game.mailing_lists WHERE name_key = $1 AND deleted_at IS NULL", &[&key]).await?.as_ref().map(List::from_row))
}
pub(crate) async fn get_for_update(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<List>, Error> {
    Ok(ex.query_opt("SELECT * FROM game.mailing_lists WHERE list_id = $1 FOR UPDATE", &[&id]).await?.as_ref().map(List::from_row))
}
pub(crate) async fn create(ex: &(impl Executor + ?Sized), key: &str, display: &str, owner: i64, access: i16, role: i16, created: i64) -> Result<List, Error> {
    let row = ex.query_one("INSERT INTO game.mailing_lists (name_key, display_name, owner_id, default_access, default_member_access, created_at) VALUES ($1, $2, $3, $4, $5, $6) RETURNING *", &[&key, &display, &owner, &access, &role, &created]).await?;
    Ok(List::from_row(&row))
}
pub(crate) async fn joined_lists(ex: &(impl Executor + ?Sized), character: i64) -> Result<Vec<(List, i16)>, Error> {
    let rows = ex.query("SELECT l.*, m.member_access FROM game.mailing_list_members m JOIN game.mailing_lists l ON l.list_id = m.list_id WHERE m.character_id = $1 AND l.deleted_at IS NULL ORDER BY l.list_id", &[&character]).await?;
    Ok(rows.iter().map(|row| (List::from_row(row), MemberAccess::from_row(row).member_access)).collect())
}
pub(crate) async fn mark_deleted(ex: &(impl Executor + ?Sized), id: i64, deleted: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.mailing_lists SET deleted_at = $2, welcome_title = NULL, welcome_body = NULL WHERE list_id = $1 AND deleted_at IS NULL", &[&id, &deleted]).await
}
pub(crate) async fn members(ex: &(impl Executor + ?Sized), id: i64) -> Result<Vec<Member>, Error> {
    Ok(ex.query("SELECT character_id, member_access FROM game.mailing_list_members WHERE list_id = $1 ORDER BY character_id", &[&id]).await?.iter().map(Member::from_row).collect())
}
pub(crate) async fn membership(ex: &(impl Executor + ?Sized), id: i64, character: i64) -> Result<Option<i16>, Error> {
    Ok(ex.query_opt("SELECT member_access FROM game.mailing_list_members WHERE list_id = $1 AND character_id = $2", &[&id, &character]).await?.as_ref().map(|r| MemberAccess::from_row(r).member_access))
}
pub(crate) async fn insert_member_if_absent(ex: &(impl Executor + ?Sized), id: i64, character: i64, role: i16, joined: i64) -> Result<bool, Error> {
    Ok(ex.execute("INSERT INTO game.mailing_list_members (list_id, character_id, member_access, joined_at) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING", &[&id, &character, &role, &joined]).await? != 0)
}
pub(crate) async fn remove_members(ex: &(impl Executor + ?Sized), id: i64, ids: &[i64]) -> Result<u64, Error> {
    ex.execute("WITH targets AS (SELECT character_id FROM game.mailing_list_members WHERE list_id = $1 AND character_id = ANY($2) ORDER BY character_id FOR UPDATE) DELETE FROM game.mailing_list_members m USING targets t WHERE m.list_id = $1 AND m.character_id = t.character_id", &[&id, &ids]).await
}
pub(crate) async fn set_member_access(ex: &(impl Executor + ?Sized), id: i64, ids: &[i64], role: i16) -> Result<Vec<i64>, Error> {
    let rows = ex.query("WITH targets AS (SELECT character_id FROM game.mailing_list_members WHERE list_id = $1 AND character_id = ANY($2) AND member_access <> $3 ORDER BY character_id FOR UPDATE), changed AS (UPDATE game.mailing_list_members m SET member_access = $3 FROM targets t WHERE m.list_id = $1 AND m.character_id = t.character_id RETURNING m.character_id) SELECT character_id FROM changed ORDER BY character_id", &[&id, &ids, &role]).await?;
    Ok(rows.iter().map(|r| CharacterId::from_row(r).character_id).collect())
}
pub(crate) async fn recipient_ids(ex: &(impl Executor + ?Sized), id: i64) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT character_id FROM game.mailing_list_members WHERE list_id = $1 ORDER BY character_id", &[&id]).await?.iter().map(|r| CharacterId::from_row(r).character_id).collect())
}
pub(crate) async fn member_count(ex: &(impl Executor + ?Sized), id: i64) -> Result<i64, Error> {
    Ok(Count::from_row(&ex.query_one("SELECT count(*) AS count FROM game.mailing_list_members WHERE list_id = $1", &[&id]).await?).count)
}
pub(crate) async fn clear_members(ex: &(impl Executor + ?Sized), id: i64) -> Result<u64, Error> {
    ex.execute("WITH targets AS (SELECT character_id FROM game.mailing_list_members WHERE list_id = $1 ORDER BY character_id FOR UPDATE) DELETE FROM game.mailing_list_members m USING targets t WHERE m.list_id = $1 AND m.character_id = t.character_id", &[&id]).await
}
pub(crate) async fn access_entries(ex: &(impl Executor + ?Sized), id: i64) -> Result<Vec<Access>, Error> {
    Ok(ex.query("SELECT entity_id, entity_kind, access FROM game.mailing_list_access WHERE list_id = $1 ORDER BY entity_id", &[&id]).await?.iter().map(Access::from_row).collect())
}
pub(crate) async fn upsert_access(ex: &(impl Executor + ?Sized), id: i64, entity: i64, kind: i16, access: i16) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.mailing_list_access (list_id, entity_id, entity_kind, access) VALUES ($1, $2, $3, $4) ON CONFLICT (list_id, entity_id) DO UPDATE SET entity_kind = EXCLUDED.entity_kind, access = EXCLUDED.access", &[&id, &entity, &kind, &access]).await
}
pub(crate) async fn clear_access(ex: &(impl Executor + ?Sized), id: i64, entity: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.mailing_list_access WHERE list_id = $1 AND entity_id = $2", &[&id, &entity]).await
}
pub(crate) async fn clear_all_access(ex: &(impl Executor + ?Sized), id: i64) -> Result<u64, Error> {
    ex.execute("WITH targets AS (SELECT entity_id FROM game.mailing_list_access WHERE list_id = $1 ORDER BY entity_id FOR UPDATE) DELETE FROM game.mailing_list_access a USING targets t WHERE a.list_id = $1 AND a.entity_id = t.entity_id", &[&id]).await
}
pub(crate) async fn update_defaults(ex: &(impl Executor + ?Sized), id: i64, access: i16, role: i16) -> Result<u64, Error> {
    ex.execute("UPDATE game.mailing_lists SET default_access = $2, default_member_access = $3 WHERE list_id = $1", &[&id, &access, &role]).await
}
pub(crate) async fn save_welcome(ex: &(impl Executor + ?Sized), id: i64, title: &str, body: &str) -> Result<u64, Error> {
    ex.execute("UPDATE game.mailing_lists SET welcome_title = $2, welcome_body = $3 WHERE list_id = $1", &[&id, &title, &body]).await
}
pub(crate) async fn clear_welcome(ex: &(impl Executor + ?Sized), id: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.mailing_lists SET welcome_title = NULL, welcome_body = NULL WHERE list_id = $1", &[&id]).await
}

#[cfg(test)]
pub(crate) mod tests;
