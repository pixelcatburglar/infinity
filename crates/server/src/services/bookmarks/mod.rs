//! Personal and shared location bookmarks for `bookmarkSvc.py` and beyonce.
//! Uses bookmark folders, known subscriptions, access groups, SDE locations and space state.
//! Corporation and alliance affiliation changes are checked on each access, but do not
//! immediately evict previously cached folders from clients.
mod access;
mod access_group_bookmark_mgr;
pub(crate) mod beyonce;
mod mutations;
pub(crate) mod notifications;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "accessGroupBookmarkMgr" => access_group_bookmark_mgr::dispatch(c, call).await,
        _ => None,
    }
}

use super::{db_err, keyval, notify_error, value_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::TYPE_SOLAR_SYSTEM;
use crate::dao::bookmarks as dao;
use crate::db::executor::Executor;
use crate::db::outbox::Outbox;
use crate::messages;
use marshal::Value;

/// Server bookmark folder and memo name length limit, in characters.
const MAX_NAME: usize = 100;
/// Server bookmark and folder description limit, in characters.
const MAX_DESCRIPTION: usize = 3900;
/// Server limit on a bookmark scan-result identifier, in characters.
const MAX_SCAN_RESULT_ID: usize = 32;
/// `appConst.py` maximum known bookmark folders.
const MAX_KNOWN: usize = 30;
/// `appConst.py` maximum active personal bookmark folders.
const MAX_ACTIVE_PERSONAL: usize = 5;
/// `appConst.py` maximum active shared bookmark folders.
const MAX_ACTIVE_SHARED: usize = 3;


fn folder_row(r: &dao::Folder, active: bool, tier: i64) -> Value {
    let opt = |value: Option<i64>| value.map(Value::Int).unwrap_or(Value::None);
    keyval([
        ("folderID", Value::Int(r.folder_id)),
        ("folderName", Value::unicode(r.name.as_str())),
        ("description", Value::unicode(r.description.as_str())),
        ("isPersonal", Value::Bool(r.is_personal)),
        ("isActive", Value::Bool(active)),
        ("accessLevel", Value::Int(tier)),
        ("creatorID", Value::Int(r.owner_id)),
        ("ownerID", Value::Int(r.owner_id)),
        ("adminGroupID", opt(r.admin_group_id)),
        ("manageGroupID", opt(r.manage_group_id)),
        ("useGroupID", opt(r.use_group_id)),
        ("viewGroupID", opt(r.view_group_id)),
    ])
}

fn subfolder_row(r: &dao::Subfolder) -> Value {
    keyval([
        ("subfolderID", Value::Int(r.subfolder_id)),
        ("folderID", Value::Int(r.folder_id)),
        ("subfolderName", Value::unicode(r.name.as_str())),
        ("creatorID", Value::Int(r.creator_id)),
    ])
}


fn bookmark_row(r: &dao::Bookmark) -> Value {
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    keyval([
        ("bookmarkID", Value::Int(r.bookmark_id)),
        ("folderID", Value::Int(r.folder_id)),
        ("subfolderID", opt(r.subfolder_id)),
        ("itemID", opt(r.item_id)),
        ("typeID", r.type_id.map(|t| Value::Int(t as i64)).unwrap_or(Value::None)),
        ("flag", Value::None),
        ("locationID", Value::Int(r.location_id)),
        ("x", Value::Float(r.x)),
        ("y", Value::Float(r.y)),
        ("z", Value::Float(r.z)),
        ("memo", Value::unicode(r.memo.as_str())),
        ("note", Value::unicode(r.note.as_str())),
        ("created", Value::Int(r.created)),
        ("expiry", opt(r.expiry)),
        ("creatorID", Value::Int(r.creator_id)),
    ])
}


/// The bookmark dialog's expiry choice, or None when omitted.
fn expiry(call: &Call<'_>, i: usize) -> Result<Option<i64>, CallError> {
    match call.args.get(i) {
        None | Some(Value::None) => Ok(None),
        Some(value) => value.as_int().map(Some).ok_or_else(|| notify_error(messages::bookmarks::BOOKMARK_INVALID_EXPIRY)),
    }
}

fn subfolder_arg(call: &Call<'_>, i: usize) -> Option<i64> {
    call.kwargs.get("subfolderID").or(call.args.get(i)).and_then(|v| v.as_int())
}




#[cfg(test)] mod tests;
