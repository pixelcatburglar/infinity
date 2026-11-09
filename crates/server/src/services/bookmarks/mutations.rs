//! Bookmark mutations for `bookmarkSvc.py`, using bookmark folders and access groups.
//! Transactions serialize with access-group changes and deliver effects after commit.
use super::*;
use std::sync::Arc;

/// `appConst.py` bookmark limits.
const MAX_CREATED: usize = 30;
const MAX_SUBFOLDERS: usize = 30;
const MAX_PERSONAL_BOOKMARKS: usize = 3000;
const MAX_SHARED_BOOKMARKS: usize = 500;

/// `bookmarkConst.py` expiration choices.
const EXPIRY_NONE: i64 = 0;
const EXPIRY_FOUR_HOURS: i64 = 1;
const EXPIRY_TWO_DAYS: i64 = 2;
const EXPIRY_DAY: i64 = 3;
/// Durations corresponding to the client expiration choices.
const FOUR_HOURS: i64 = 4;
const TWO_DAYS: i64 = 2;
const DAY_HOURS: i64 = 24;

fn invalid() -> CallError { notify_error(messages::bookmarks::BOOKMARK_INVALID_TARGET) }
fn denied() -> CallError { notify_error(messages::bookmarks::BOOKMARK_ACCESS_DENIED) }
fn limit() -> CallError { notify_error(messages::bookmarks::BOOKMARK_LIMIT_REACHED) }
fn permitted(tier: i64, required: i64) -> bool { tier == access::PERSONAL || tier >= required }
fn int(call: &Call<'_>, i: usize) -> Result<i64, CallError> { call.args.get(i).and_then(Value::as_int).ok_or_else(invalid) }
fn optional(call: &Call<'_>, i: usize) -> Result<Option<i64>, CallError> {
    match call.args.get(i) { Some(Value::None) => Ok(None), Some(v) => v.as_int().map(Some).ok_or_else(invalid), None => Err(invalid()) }
}
fn boolean(call: &Call<'_>, i: usize) -> Result<bool, CallError> {
    match call.args.get(i) { Some(Value::Bool(v)) => Ok(*v), Some(v) if v.as_int() == Some(0) => Ok(false), Some(v) if v.as_int() == Some(1) => Ok(true), _ => Err(invalid()) }
}
fn text(call: &Call<'_>, i: usize, max: usize) -> Result<String, CallError> {
    let s = call.args.get(i).and_then(Value::as_text).ok_or_else(invalid)?;
    if s.chars().count() > max { return Err(invalid()); }
    Ok(s.into_owned())
}
fn ids(call: &Call<'_>, i: usize) -> Result<Vec<i64>, CallError> {
    let mut result = Vec::new();
    for v in call.args.get(i).and_then(Value::items).ok_or_else(invalid)? { result.push(v.as_int().ok_or_else(invalid)?); }
    result.sort_unstable(); result.dedup(); Ok(result)
}
fn groups(call: &Call<'_>, i: usize) -> Result<dao::FolderGroups, CallError> {
    Ok(dao::FolderGroups { admin_group_id: optional(call, i)?, manage_group_id: optional(call, i+1)?, use_group_id: optional(call, i+2)?, view_group_id: optional(call, i+3)? })
}
async fn validate_groups(ex: &(impl Executor + ?Sized), personal: bool, g: &dao::FolderGroups) -> Result<(), CallError> {
    let all = [g.admin_group_id, g.manage_group_id, g.use_group_id, g.view_group_id];
    if personal && all.iter().any(Option::is_some) { return Err(invalid()); }
    for id in all.into_iter().flatten() {
        if crate::dao::access_groups::group(ex, id).await.map_err(db_err)?.is_none() { return Err(invalid()); }
    }
    Ok(())
}
async fn folder(ex: &(impl Executor + ?Sized), me: i64, id: i64, required: i64) -> Result<(dao::Folder, i64), CallError> {
    let (folder, tier) = access::get(ex, me, id).await?.ok_or_else(invalid)?;
    if !permitted(tier, required) { return Err(denied()); }
    Ok((folder, tier))
}
async fn parent(ex: &(impl Executor + ?Sized), folder: i64, sub: Option<i64>) -> Result<(), CallError> {
    if let Some(sub) = sub { if dao::subfolder(ex, folder, sub).await.map_err(db_err)?.is_none() { return Err(invalid()); } }
    Ok(())
}
async fn capacity(ex: &(impl Executor + ?Sized), folder: &dao::Folder, additional: usize) -> Result<(), CallError> {
    let count = dao::bookmarks(ex, &[folder.folder_id], i64::MIN).await.map_err(db_err)?.len();
    if count.saturating_add(additional) > if folder.is_personal { MAX_PERSONAL_BOOKMARKS } else { MAX_SHARED_BOOKMARKS } { return Err(limit()); }
    Ok(())
}
async fn selected(ex: &(impl Executor + ?Sized), folder: i64, ids: &[i64]) -> Result<Vec<dao::Bookmark>, CallError> {
    let rows = dao::bookmarks_by_ids(ex, folder, ids).await.map_err(db_err)?;
    if rows.len() != ids.len() { return Err(invalid()); } Ok(rows)
}
fn ids_value(ids: &[i64]) -> Value { Value::list(ids.iter().copied().map(Value::Int)) }
fn rows_value(rows: &[dao::Bookmark]) -> Value { Value::list(rows.iter().map(bookmark_row)) }
fn move_effect(outbox: &mut Outbox, from: i64, to: i64, source: &[i64], dest: &[i64], rows: &[dao::Bookmark]) {
    let ids: Vec<_> = rows.iter().map(|r| r.bookmark_id).collect();
    if from == to { notifications::updated(outbox, dest, to, "bookmarksUpdated", rows_value(rows)); return; }
    let only_source: Vec<_> = source.iter().copied().filter(|id| !dest.contains(id)).collect();
    let only_dest: Vec<_> = dest.iter().copied().filter(|id| !source.contains(id)).collect();
    let both: Vec<_> = source.iter().copied().filter(|id| dest.contains(id)).collect();
    notifications::updated(outbox, &only_source, from, "bookmarksRemoved", ids_value(&ids));
    notifications::updated(outbox, &only_dest, to, "bookmarksAdded", rows_value(rows));
    notifications::updated(outbox, &both, from, "bookmarksMoved", Value::tuple([rows_value(rows), Value::Int(from), Value::Int(to)]));
}

pub(super) async fn add(c: &Client, me: i64, folder_id: i64, sub: Option<i64>, item: Option<i64>, type_id: Option<i32>, system: i64, pos: [f64;3], memo: &str, note: &str, expiry_option: Option<i64>) -> CallResult {
    if memo.chars().count() > MAX_NAME || note.chars().count() > MAX_DESCRIPTION || pos.iter().any(|v| !v.is_finite()) { return Err(invalid()); }
    let now = filetime_now();
    let duration = match expiry_option {
        None | Some(EXPIRY_NONE) => None,
        Some(EXPIRY_FOUR_HOURS) => Some(FOUR_HOURS * infinity_core::consts::FILETIME_HOUR),
        Some(EXPIRY_TWO_DAYS) => Some(TWO_DAYS * infinity_core::consts::FILETIME_DAY),
        Some(EXPIRY_DAY) => Some(DAY_HOURS * infinity_core::consts::FILETIME_HOUR),
        _ => return Err(notify_error(messages::bookmarks::BOOKMARK_INVALID_EXPIRY)),
    };
    let expiry = duration.map(|d| now.checked_add(d).ok_or_else(|| notify_error(messages::bookmarks::BOOKMARK_INVALID_EXPIRY))).transpose()?;
    let _guard = crate::services::access_groups::lock().await;
    let inputs = Arc::new((memo.to_owned(), note.to_owned()));
    c.db.transaction(|tx, outbox: &mut Outbox| { let inputs = Arc::clone(&inputs); Box::pin(async move {
        let (memo, note) = inputs.as_ref();
        let (folder, _) = folder(tx, me, folder_id, access::USE).await?;
        parent(tx, folder_id, sub).await?; capacity(tx, &folder, 1).await?;
        let row = dao::add(tx, &dao::NewBookmark { folder: folder_id, subfolder: sub, creator: me, item, type_id, system, pos, memo, note, created: now, expiry }).await.map_err(db_err)?;
        let audience = notifications::audience(tx, &folder, true).await?;
        notifications::updated(outbox, &audience, folder_id, "bookmarksAdded", rows_value(std::slice::from_ref(&row)));
        Ok(Value::tuple([Value::Int(row.bookmark_id), row.item_id.map(Value::Int).unwrap_or(Value::None), row.type_id.map(|v| Value::Int(v as i64)).unwrap_or(Value::None), Value::Float(row.x), Value::Float(row.y), Value::Float(row.z), Value::Int(row.location_id), row.expiry.map(Value::Int).unwrap_or(Value::None)]))
    }) }, |value, outbox| async move { crate::db::outbox::dispatch_world(value, outbox, &c.world) }).await
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if !matches!(call.method, "AddFolder" | "UpdateFolder" | "DeleteFolder" | "CreateSubfolder" | "UpdateSubfolder" | "DeleteSubfolder" | "UpdateBookmark" | "DeleteBookmarks" | "MoveBookmarksToFolderAndSubfolder" | "CopyBookmarksToFolderAndSubfolder") { return None; }
    Some(run(c, call).await)
}

async fn run(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").ok_or_else(denied)?;
    let _guard = crate::services::access_groups::lock().await;
    let online = c.world.online_pilots();
    match call.method {
        "AddFolder" => {
            let personal = boolean(call, 0)?; let name = text(call, 1, MAX_NAME)?; let description = text(call, 2, MAX_DESCRIPTION)?; let g = groups(call, 3)?;
            let inputs = Arc::new((name, description, g));
            c.db.transaction(|tx, _: &mut Outbox| { let inputs = Arc::clone(&inputs); Box::pin(async move {
                let (name, description, g) = inputs.as_ref();
                validate_groups(tx, personal, g).await?;
                if !personal { let admin = g.admin_group_id.ok_or_else(invalid)?; let membership = crate::services::access_groups::membership(tx, me, admin).await?; if !(0..=2).contains(&membership) { return Err(denied()); } }
                if dao::all_folders(tx).await.map_err(db_err)?.iter().filter(|f| f.owner_id == me).count() >= MAX_CREATED { return Err(limit()); }
                let known = dao::known::known(tx, me).await.map_err(db_err)?;
                if known.len() >= MAX_KNOWN { return Err(limit()); }
                let mut active = 0;
                for k in known.iter().filter(|k| k.is_active) { if let Some(f) = dao::folder(tx, k.folder_id).await.map_err(db_err)? { if f.is_personal == personal && access::tier(tx, me, &f).await? != access::NONE { active += 1; } } }
                if active >= if personal { MAX_ACTIVE_PERSONAL } else { MAX_ACTIVE_SHARED } { return Err(limit()); }
                let f = dao::add_folder(tx, &dao::NewFolder { owner_id: me, name, description, is_personal: personal, groups: g.clone() }).await.map_err(db_err)?;
                let tier = access::tier(tx, me, &f).await?; if !personal && tier != access::ADMIN { return Err(denied()); }
                dao::known::set_known(tx, me, f.folder_id, true).await.map_err(db_err)?;
                Ok(folder_row(&f, true, tier))
            }) }, |v, o| async move { crate::db::outbox::dispatch_world(v, o, &c.world) }).await
        }
        "UpdateFolder" => {
            let id = int(call, 0)?; let name = text(call, 1, MAX_NAME)?; let description = text(call, 2, MAX_DESCRIPTION)?; let g = groups(call, 3)?;
            let inputs = Arc::new((name, description, g, online));
            c.db.transaction(|tx, outbox: &mut Outbox| { let inputs = Arc::clone(&inputs); Box::pin(async move {
                let (name, description, g, online) = inputs.as_ref();
                let (f, _) = folder(tx, me, id, access::ADMIN).await?; validate_groups(tx, f.is_personal, g).await?;
                let before = notifications::snapshot_folder(tx, &f, online).await?;
                if dao::update_folder(tx, id, name, description, g).await.map_err(db_err)? != 1 { return Err(invalid()); }
                notifications::access_changes(tx, outbox, &before).await?;
                let updated = dao::folder(tx, id).await.map_err(db_err)?.ok_or_else(invalid)?;
                let audience = notifications::audience(tx, &updated, false).await?;
                notifications::updated(outbox, &audience, id, "folderUpdated", Value::tuple([Value::unicode(name.as_str()), Value::unicode(description.as_str())]));
                Ok(Value::Int(access::tier(tx, me, &updated).await?))
            }) }, |v, o| async move { crate::db::outbox::dispatch_world(v, o, &c.world) }).await
        }
        "DeleteFolder" => {
            let id = int(call, 0)?;
            c.db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
                let (f, _) = folder(tx, me, id, access::ADMIN).await?; let audience = notifications::audience(tx, &f, false).await?;
                dao::delete_folder(tx, id).await.map_err(db_err)?;
                for character in audience { outbox.notify_pilot(character, "OnSharedBookmarksFolderDeleted", vec![Value::Int(id)]); }
                Ok(Value::None)
            }), |v, o| async move { crate::db::outbox::dispatch_world(v, o, &c.world) }).await
        }
        "CreateSubfolder" | "UpdateSubfolder" | "DeleteSubfolder" => subfolder_mutation(c, call, me).await,
        "UpdateBookmark" => update_bookmark(c, call, me).await,
        "DeleteBookmarks" => delete_bookmarks(c, call, me).await,
        _ => transfer(c, call, me).await,
    }
}

async fn subfolder_mutation(c: &Client, call: &Call<'_>, me: i64) -> CallResult {
    let folder_id = int(call, 0)?;
    let create = call.method == "CreateSubfolder";
    let delete = call.method == "DeleteSubfolder";
    let sub = if create { None } else { Some(int(call, 1)?) };
    let name = if delete { String::new() } else { text(call, if create { 1 } else { 2 }, MAX_NAME)? };
    let name = Arc::new(name);
    c.db.transaction(|tx, outbox: &mut Outbox| { let name = Arc::clone(&name); Box::pin(async move {
        let name = name.as_ref();
        let (f, tier) = folder(tx, me, folder_id, access::USE).await?;
        let audience = notifications::audience(tx, &f, true).await?;
        if create {
            if dao::subfolders(tx, &[folder_id]).await.map_err(db_err)?.len() >= MAX_SUBFOLDERS { return Err(limit()); }
            let row = dao::add_subfolder(tx, folder_id, me, name).await.map_err(db_err)?;
            let result = subfolder_row(&row);
            notifications::updated(outbox, &audience, folder_id, "subfolderAdded", result.clone());
            return Ok(result);
        }
        let sub = sub.ok_or_else(invalid)?;
        let row = dao::subfolder(tx, folder_id, sub).await.map_err(db_err)?.ok_or_else(invalid)?;
        if !permitted(tier, access::MANAGE) && row.creator_id != me { return Err(denied()); }
        if !delete {
            if dao::update_subfolder(tx, folder_id, sub, name).await.map_err(db_err)? != 1 { return Err(invalid()); }
            notifications::updated(outbox, &audience, folder_id, "subfolderUpdated", Value::tuple([Value::Int(sub), Value::unicode(name.as_str())]));
            return Ok(Value::Bool(true));
        }
        let contained: Vec<_> = dao::bookmarks(tx, &[folder_id], i64::MIN).await.map_err(db_err)?.into_iter().filter(|b| b.subfolder_id == Some(sub)).collect();
        if !permitted(tier, access::MANAGE) && contained.iter().any(|b| b.creator_id != me) { return Err(denied()); }
        let deleted = dao::delete_subfolder_bookmarks(tx, folder_id, sub).await.map_err(db_err)?;
        if deleted.len() != contained.len() || dao::delete_subfolder(tx, folder_id, sub).await.map_err(db_err)? != 1 { return Err(invalid()); }
        notifications::updated(outbox, &audience, folder_id, "bookmarksRemoved", ids_value(&deleted));
        notifications::updated(outbox, &audience, folder_id, "subfolderRemoved", Value::Int(sub));
        Ok(ids_value(&deleted))
    }) }, |v, o| async move { crate::db::outbox::dispatch_world(v, o, &c.world) }).await
}

async fn update_bookmark(c: &Client, call: &Call<'_>, me: i64) -> CallResult {
    let id = int(call, 0)?; let from = int(call, 1)?;
    let memo = text(call, 2, MAX_NAME)?; let note = text(call, 3, MAX_DESCRIPTION)?;
    let sub = optional(call, 4)?; let to = int(call, 5)?; let cancel = boolean(call, 6)?;
    let inputs = Arc::new((memo, note));
    c.db.transaction(|tx, outbox: &mut Outbox| { let inputs = Arc::clone(&inputs); Box::pin(async move {
        let (memo, note) = inputs.as_ref();
        let (src, tier) = folder(tx, me, from, access::USE).await?;
        let (dst, _) = folder(tx, me, to, access::USE).await?;
        let rows = selected(tx, from, &[id]).await?;
        if !permitted(tier, access::MANAGE) && (from != to || rows[0].creator_id != me) { return Err(denied()); }
        parent(tx, to, sub).await?;
        if from != to { capacity(tx, &dst, 1).await?; }
        let source = notifications::audience(tx, &src, true).await?;
        let dest = notifications::audience(tx, &dst, true).await?;
        if dao::update_bookmark(tx, &dao::UpdateBookmark { id, folder: from, memo, note, subfolder: sub, new_folder: to, cancel }).await.map_err(db_err)? != 1 { return Err(invalid()); }
        let rows = selected(tx, to, &[id]).await?;
        move_effect(outbox, from, to, &source, &dest, &rows);
        Ok(Value::None)
    }) }, |v, o| async move { crate::db::outbox::dispatch_world(v, o, &c.world) }).await
}

async fn delete_bookmarks(c: &Client, call: &Call<'_>, me: i64) -> CallResult {
    let id = int(call, 0)?; let wanted = ids(call, 1)?;
    let wanted = Arc::new(wanted);
    c.db.transaction(|tx, outbox: &mut Outbox| { let wanted = Arc::clone(&wanted); Box::pin(async move {
        let wanted = wanted.as_ref();
        let (f, tier) = folder(tx, me, id, access::USE).await?;
        let rows = selected(tx, id, wanted).await?;
        if !permitted(tier, access::MANAGE) && rows.iter().any(|r| r.creator_id != me) { return Err(denied()); }
        let audience = notifications::audience(tx, &f, true).await?;
        let deleted = dao::delete_bookmarks(tx, id, wanted).await.map_err(db_err)?;
        if deleted.len() != wanted.len() { return Err(invalid()); }
        notifications::updated(outbox, &audience, id, "bookmarksRemoved", ids_value(&deleted));
        Ok(ids_value(&deleted))
    }) }, |v, o| async move { crate::db::outbox::dispatch_world(v, o, &c.world) }).await
}

async fn transfer(c: &Client, call: &Call<'_>, me: i64) -> CallResult {
    let from = int(call, 0)?; let to = int(call, 1)?; let sub = optional(call, 2)?; let wanted = ids(call, 3)?;
    let copy = call.method == "CopyBookmarksToFolderAndSubfolder";
    let wanted = Arc::new(wanted);
    c.db.transaction(|tx, outbox: &mut Outbox| { let wanted = Arc::clone(&wanted); Box::pin(async move {
        let wanted = wanted.as_ref();
        let (src, _) = folder(tx, me, from, if copy { access::VIEW } else { access::MANAGE }).await?;
        let (dst, _) = folder(tx, me, to, access::USE).await?;
        parent(tx, to, sub).await?;
        selected(tx, from, wanted).await?;
        if copy || from != to { capacity(tx, &dst, wanted.len()).await?; }
        let dest = notifications::audience(tx, &dst, true).await?;
        if copy {
            let rows = dao::copy_bookmarks(tx, from, to, sub, me, wanted).await.map_err(db_err)?;
            if rows.len() != wanted.len() { return Err(invalid()); }
            notifications::updated(outbox, &dest, to, "bookmarksAdded", rows_value(&rows));
            let result = Value::Dict(rows.iter().map(|r| (Value::Int(r.bookmark_id), bookmark_row(r))).collect());
            return Ok(Value::tuple([result, Value::None]));
        }
        let source = notifications::audience(tx, &src, true).await?;
        let moved = dao::move_bookmarks(tx, from, to, sub, wanted).await.map_err(db_err)?;
        if moved.len() != wanted.len() { return Err(invalid()); }
        let rows = selected(tx, to, wanted).await?;
        move_effect(outbox, from, to, &source, &dest, &rows);
        let result = Value::list(moved.iter().map(|r| keyval([
            ("bookmarkID", Value::Int(r.bookmark_id)), ("folderID", Value::Int(r.folder_id)),
            ("subfolderID", r.subfolder_id.map(Value::Int).unwrap_or(Value::None)),
        ])));
        Ok(Value::tuple([result, Value::None]))
    }) }, |v, o| async move { crate::db::outbox::dispatch_world(v, o, &c.world) }).await
}
