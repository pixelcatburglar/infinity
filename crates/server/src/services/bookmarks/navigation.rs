//! Authoritative destinations for bookmark navigation consumers in
//! `eve/client/script/ui/shared/bookmarks/bookmarkSvc.py` and the in-flight menu.
//! Reads bookmark folders, access-group memberships, bookmark rows and SDE systems.
//! Agent bookmarks are not handled here.
use super::{access, dao, db_err, filetime_now, notify_error, CallError, Client};
use infinity_core::messages;

pub(crate) struct Destination {
    pub system: i64,
    pub pos: [f64; 3],
    pub bookmark_id: i64,
}

pub(crate) async fn resolve_for_use(c: &Client, bookmark_id: i64) -> Result<Destination, CallError> {
    let invalid = || notify_error(messages::bookmarks::BOOKMARK_INVALID_TARGET);
    let denied = || notify_error(messages::bookmarks::BOOKMARK_ACCESS_DENIED);
    let character = c.session.int("charid").ok_or_else(denied)?;
    let _guard = crate::services::access_groups::lock().await;
    let bookmark = dao::bookmark(&c.db, bookmark_id).await.map_err(db_err)?.ok_or_else(invalid)?;
    let folder = dao::folder(&c.db, bookmark.folder_id).await.map_err(db_err)?.ok_or_else(invalid)?;
    let tier = access::tier(&c.db, character, &folder).await?;
    if tier != access::PERSONAL && tier < access::USE { return Err(denied()); }
    if let Some(subfolder) = bookmark.subfolder_id {
        if dao::subfolder(&c.db, folder.folder_id, subfolder).await.map_err(db_err)?.is_none() {
            return Err(invalid());
        }
    }
    let pos = [bookmark.x, bookmark.y, bookmark.z];
    if bookmark.expiry.is_some_and(|expiry| expiry <= filetime_now())
        || pos.iter().any(|value| !value.is_finite())
        || c.sde.map.system(bookmark.location_id).is_none()
    {
        return Err(invalid());
    }
    Ok(Destination { system: bookmark.location_id, pos, bookmark_id: bookmark.bookmark_id })
}
