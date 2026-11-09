//! Bookmark persistence for `eve/client/script/ui/shared/bookmarks/bookmarkSvc.py`.
//! Uses `game.bookmarks`, `game.bookmark_folders`, and `game.bookmark_subfolders`.
//! Folder access policy, transactions and effects remain with callers.
pub(crate) mod known;
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

#[derive(Clone)]
pub(crate) struct Folder {
    pub folder_id: i64,
    pub owner_id: i64,
    pub name: String,
    pub description: String,
    pub is_personal: bool,
    pub admin_group_id: Option<i64>,
    pub manage_group_id: Option<i64>,
    pub use_group_id: Option<i64>,
    pub view_group_id: Option<i64>,
}
impl Folder {
    fn from_row(r: &Row) -> Self {
        Self { folder_id: r.get("folder_id"), owner_id: r.get("owner_id"), name: r.get("name"), description: r.get("description"), is_personal: r.get("is_personal"), admin_group_id: r.get("admin_group_id"), manage_group_id: r.get("manage_group_id"), use_group_id: r.get("use_group_id"), view_group_id: r.get("view_group_id") }
    }
}
#[derive(Clone)]
pub(crate) struct Subfolder {
    pub subfolder_id: i64,
    pub folder_id: i64,
    pub creator_id: i64,
    pub name: String,
}
impl Subfolder {
    fn from_row(r: &Row) -> Self {
        Self { subfolder_id: r.get("subfolder_id"), folder_id: r.get("folder_id"), creator_id: r.get("creator_id"), name: r.get("name") }
    }
}
#[derive(Clone)]
pub(crate) struct Bookmark {
    pub bookmark_id: i64,
    pub folder_id: i64,
    pub subfolder_id: Option<i64>,
    pub item_id: Option<i64>,
    pub type_id: Option<i32>,
    pub location_id: i64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub memo: String,
    pub note: String,
    pub created: i64,
    pub expiry: Option<i64>,
    pub creator_id: i64,
}
impl Bookmark {
    fn from_row(r: &Row) -> Self {
        Self {
            bookmark_id: r.get("bookmark_id"),
            folder_id: r.get("folder_id"),
            subfolder_id: r.get("subfolder_id"),
            item_id: r.get("item_id"),
            type_id: r.get("type_id"),
            location_id: r.get("location_id"),
            x: r.get("x"),
            y: r.get("y"),
            z: r.get("z"),
            memo: r.get("memo"),
            note: r.get("note"),
            created: r.get("created"),
            expiry: r.get("expiry"),
            creator_id: r.get("creator_id"),
        }
    }
}
struct BookmarkId { bookmark_id: i64 }
impl BookmarkId {
    fn from_row(r: &Row) -> Self { Self { bookmark_id: r.get("bookmark_id") } }
}
#[derive(Clone)]
pub(crate) struct Moved {
    pub bookmark_id: i64,
    pub folder_id: i64,
    pub subfolder_id: Option<i64>,
}
impl Moved {
    fn from_row(r: &Row) -> Self {
        Self { bookmark_id: r.get("bookmark_id"), folder_id: r.get("folder_id"), subfolder_id: r.get("subfolder_id") }
    }
}
const BOOKMARK_SELECT: &str = "SELECT bookmark_id, folder_id, subfolder_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry, creator_id
    FROM game.bookmarks";

pub(crate) struct NewBookmark<'a> {
    pub folder: i64,
    pub subfolder: Option<i64>,
    pub creator: i64,
    pub item: Option<i64>,
    pub type_id: Option<i32>,
    pub system: i64,
    pub pos: [f64; 3],
    pub memo: &'a str,
    pub note: &'a str,
    pub created: i64,
    pub expiry: Option<i64>,
}
pub(crate) async fn add(ex: &(impl Executor + ?Sized), b: &NewBookmark<'_>) -> Result<Bookmark, Error> {
    let row = ex.query_one(
        "INSERT INTO game.bookmarks (folder_id, subfolder_id, creator_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING bookmark_id, folder_id, subfolder_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry, creator_id",
        &[&b.folder, &b.subfolder, &b.creator, &b.item, &b.type_id, &b.system, &b.pos[0], &b.pos[1], &b.pos[2], &b.memo, &b.note, &b.created, &b.expiry],
    ).await?;
    Ok(Bookmark::from_row(&row))
}
const FOLDER_COLUMNS: &str = "folder_id, owner_id, name, description, is_personal, admin_group_id, manage_group_id, use_group_id, view_group_id";
#[derive(Clone)]
pub(crate) struct FolderGroups {
    pub admin_group_id: Option<i64>, pub manage_group_id: Option<i64>,
    pub use_group_id: Option<i64>, pub view_group_id: Option<i64>,
}
pub(crate) struct NewFolder<'a> {
    pub owner_id: i64, pub name: &'a str, pub description: &'a str,
    pub is_personal: bool, pub groups: FolderGroups,
}
pub(crate) async fn all_folders(ex: &(impl Executor + ?Sized)) -> Result<Vec<Folder>, Error> {
    Ok(ex.query(&format!("SELECT {FOLDER_COLUMNS} FROM game.bookmark_folders ORDER BY folder_id"), &[]).await?.iter().map(Folder::from_row).collect())
}
pub(crate) async fn bookmark(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<Bookmark>, Error> {
    Ok(ex.query_opt(&format!("{BOOKMARK_SELECT} WHERE bookmark_id = $1"), &[&id]).await?.as_ref().map(Bookmark::from_row))
}
pub(crate) async fn bookmarks(ex: &(impl Executor + ?Sized), folder_ids: &[i64], now: i64) -> Result<Vec<Bookmark>, Error> {
    Ok(ex.query(&format!("{BOOKMARK_SELECT} WHERE folder_id = ANY($1) AND (expiry IS NULL OR expiry > $2) ORDER BY bookmark_id"), &[&folder_ids, &now]).await?.iter().map(Bookmark::from_row).collect())
}
pub(crate) async fn bookmarks_by_ids(ex: &(impl Executor + ?Sized), folder: i64, wanted: &[i64]) -> Result<Vec<Bookmark>, Error> {
    Ok(ex.query(&format!("{BOOKMARK_SELECT} WHERE folder_id = $1 AND bookmark_id = ANY($2) ORDER BY bookmark_id"), &[&folder, &wanted]).await?.iter().map(Bookmark::from_row).collect())
}
pub(crate) async fn subfolders(ex: &(impl Executor + ?Sized), folder_ids: &[i64]) -> Result<Vec<Subfolder>, Error> {
    Ok(ex.query("SELECT subfolder_id, folder_id, creator_id, name FROM game.bookmark_subfolders WHERE folder_id = ANY($1) ORDER BY subfolder_id", &[&folder_ids]).await?.iter().map(Subfolder::from_row).collect())
}
pub(crate) async fn add_folder(ex: &(impl Executor + ?Sized), f: &NewFolder<'_>) -> Result<Folder, Error> {
    let g = &f.groups;
    Ok(Folder::from_row(&ex.query_one(&format!("INSERT INTO game.bookmark_folders (owner_id, name, description, is_personal, admin_group_id, manage_group_id, use_group_id, view_group_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING {FOLDER_COLUMNS}"), &[&f.owner_id, &f.name, &f.description, &f.is_personal, &g.admin_group_id, &g.manage_group_id, &g.use_group_id, &g.view_group_id]).await?))
}
pub(crate) async fn update_folder(ex: &(impl Executor + ?Sized), folder: i64, name: &str, description: &str, groups: &FolderGroups) -> Result<u64, Error> {
    ex.execute("UPDATE game.bookmark_folders SET name=$2, description=$3, admin_group_id=$4, manage_group_id=$5, use_group_id=$6, view_group_id=$7 WHERE folder_id=$1", &[&folder, &name, &description, &groups.admin_group_id, &groups.manage_group_id, &groups.use_group_id, &groups.view_group_id]).await
}
pub(crate) async fn folders_using_group(ex: &(impl Executor + ?Sized), group: i64) -> Result<Vec<Folder>, Error> {
    Ok(ex.query(&format!("SELECT {FOLDER_COLUMNS} FROM game.bookmark_folders WHERE admin_group_id=$1 OR manage_group_id=$1 OR use_group_id=$1 OR view_group_id=$1 ORDER BY folder_id"), &[&group]).await?.iter().map(Folder::from_row).collect())
}
pub(crate) async fn clear_group(ex: &(impl Executor + ?Sized), group: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.bookmark_folders SET admin_group_id=CASE WHEN admin_group_id=$1 THEN NULL ELSE admin_group_id END, manage_group_id=CASE WHEN manage_group_id=$1 THEN NULL ELSE manage_group_id END, use_group_id=CASE WHEN use_group_id=$1 THEN NULL ELSE use_group_id END, view_group_id=CASE WHEN view_group_id=$1 THEN NULL ELSE view_group_id END WHERE admin_group_id=$1 OR manage_group_id=$1 OR use_group_id=$1 OR view_group_id=$1", &[&group]).await
}
pub(crate) async fn delete_folder(ex: &(impl Executor + ?Sized), folder: i64) -> Result<(), Error> {
    for sql in [
        "DELETE FROM game.bookmarks WHERE folder_id = $1",
        "DELETE FROM game.bookmark_subfolders WHERE folder_id = $1",
        "DELETE FROM game.bookmark_folders WHERE folder_id = $1",
    ] {
        ex.execute(sql, &[&folder]).await?;
    }
    Ok(())
}
pub(crate) async fn folder(ex: &(impl Executor + ?Sized), folder: i64) -> Result<Option<Folder>, Error> {
    Ok(ex.query_opt(&format!("SELECT {FOLDER_COLUMNS} FROM game.bookmark_folders WHERE folder_id = $1"), &[&folder]).await?.as_ref().map(Folder::from_row))
}
pub(crate) async fn subfolder(ex: &(impl Executor + ?Sized), folder: i64, id: i64) -> Result<Option<Subfolder>, Error> {
    Ok(ex.query_opt("SELECT subfolder_id, folder_id, creator_id, name FROM game.bookmark_subfolders WHERE folder_id = $1 AND subfolder_id = $2", &[&folder, &id]).await?.as_ref().map(Subfolder::from_row))
}
pub(crate) async fn add_subfolder(ex: &(impl Executor + ?Sized), folder: i64, creator: i64, name: &str) -> Result<Subfolder, Error> {
    Ok(Subfolder::from_row(&ex.query_one("INSERT INTO game.bookmark_subfolders (folder_id, creator_id, name) VALUES ($1, $2, $3) RETURNING subfolder_id, folder_id, creator_id, name", &[&folder, &creator, &name]).await?))
}
pub(crate) async fn update_subfolder(ex: &(impl Executor + ?Sized), folder: i64, subfolder: i64, name: &str) -> Result<u64, Error> {
    ex.execute(
        "UPDATE game.bookmark_subfolders SET name = $3 WHERE subfolder_id = $2 AND folder_id = $1",
        &[&folder, &subfolder, &name],
    ).await
}
pub(crate) async fn delete_subfolder_bookmarks(ex: &(impl Executor + ?Sized), folder: i64, subfolder: i64) -> Result<Vec<i64>, Error> {
    let rows = ex.query(
        "DELETE FROM game.bookmarks WHERE folder_id = $1 AND subfolder_id = $2 RETURNING bookmark_id",
        &[&folder, &subfolder],
    ).await?;
    Ok(rows.iter().map(|r| BookmarkId::from_row(r).bookmark_id).collect())
}
pub(crate) async fn delete_subfolder(ex: &(impl Executor + ?Sized), folder: i64, subfolder: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.bookmark_subfolders WHERE subfolder_id = $1 AND folder_id = $2", &[&subfolder, &folder]).await
}
pub(crate) struct UpdateBookmark<'a> {
    pub id: i64,
    pub folder: i64,
    pub memo: &'a str,
    pub note: &'a str,
    pub subfolder: Option<i64>,
    pub new_folder: i64,
    pub cancel: bool,
}
pub(crate) async fn update_bookmark(ex: &(impl Executor + ?Sized), b: &UpdateBookmark<'_>) -> Result<u64, Error> {
    ex.execute(
        "UPDATE game.bookmarks SET memo = $3, note = $4, subfolder_id = $5, folder_id = $6, expiry = CASE WHEN $7 THEN NULL ELSE expiry END
                     WHERE bookmark_id = $1 AND folder_id = $2",
        &[&b.id, &b.folder, &b.memo, &b.note, &b.subfolder, &b.new_folder, &b.cancel],
    ).await
}
pub(crate) async fn delete_bookmarks(ex: &(impl Executor + ?Sized), folder: i64, wanted: &[i64]) -> Result<Vec<i64>, Error> {
    let rows = ex.query(
        "DELETE FROM game.bookmarks WHERE folder_id = $1 AND bookmark_id = ANY($2) RETURNING bookmark_id",
        &[&folder, &wanted],
    ).await?;
    Ok(rows.iter().map(|r| BookmarkId::from_row(r).bookmark_id).collect())
}
pub(crate) async fn move_bookmarks(ex: &(impl Executor + ?Sized), from: i64, to: i64, sub: Option<i64>, wanted: &[i64]) -> Result<Vec<Moved>, Error> {
    let rows = ex.query(
        "UPDATE game.bookmarks SET folder_id = $2, subfolder_id = $3 WHERE folder_id = $1 AND bookmark_id = ANY($4) RETURNING bookmark_id, folder_id, subfolder_id",
        &[&from, &to, &sub, &wanted],
    ).await?;
    Ok(rows.iter().map(Moved::from_row).collect())
}
pub(crate) async fn copy_bookmarks(ex: &(impl Executor + ?Sized), from: i64, to: i64, sub: Option<i64>, creator: i64, wanted: &[i64]) -> Result<Vec<Bookmark>, Error> {
    let rows = ex.query(
        "INSERT INTO game.bookmarks (folder_id, subfolder_id, creator_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry)
                     SELECT $2, $3, $4, item_id, type_id, location_id, x, y, z, memo, note, created, expiry FROM game.bookmarks
                     WHERE folder_id = $1 AND bookmark_id = ANY($5) ORDER BY bookmark_id
                     RETURNING bookmark_id, folder_id, subfolder_id, item_id, type_id, location_id, x, y, z, memo, note, created, expiry, creator_id",
        &[&from, &to, &sub, &creator, &wanted],
    ).await?;
    Ok(rows.iter().map(Bookmark::from_row).collect())
}
