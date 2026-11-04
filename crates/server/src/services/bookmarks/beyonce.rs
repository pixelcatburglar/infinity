//! Bookmark creation mechanisms for `eve/client/script/ui/shared/bookmarks/bookmarkSvc.py`.
//! Uses bookmark tables and space state for personal and shared folders.
//! Live location bookmarks use the system actor's sampled position.
use super::*;
/// beyonce `BookmarkLocation(itemID, folderID, name, comment, expiry, subfolderID=)`: something in
/// space, or (the pilot's own ship) the spot they are at.
pub(crate) async fn space_location(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let Some(system) = c.session.int("solarsystemid") else {
        return Err(CallError::User { msg: messages::MSG_BKM_MUST_BE_IN_FLIGHT.into(), dict: vec![] });
    };
    let item = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let Some(ball) = c.world.with(system, move |s| s.sampled_ball(item)).await.flatten() else {
        return Err(notify_error(messages::BOOKMARK_TARGET_MOVED_OR_MISSING));
    };
    let own = Some(item) == c.session.int("shipid");
    let (item, type_id) = if own { (None, Some(TYPE_SOLAR_SYSTEM)) } else { (Some(item), Some(ball.type_id)) };
    let folder = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
    mutations::add(c, me, folder, subfolder_arg(call, 5), item, type_id, system, ball.pos, &value_text(call.args.get(2), MAX_NAME), &value_text(call.args.get(3), MAX_DESCRIPTION), expiry(call, 4)?).await
}

/// beyonce `BookmarkScanResult(locationID, name, comment, resultID, folderID, expiry, subfolderID=)`.
pub(crate) async fn scan_result(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let system = call.args.first().and_then(|v| v.as_int()).or(c.session.int("solarsystemid2")).unwrap_or(0);
    let target = value_text(call.args.get(3), MAX_SCAN_RESULT_ID);
    let Some(pos) = super::super::exploration::scan_result_position(&c.db, system, me, &target).await? else {
        return Err(notify_error(messages::BOOKMARK_REQUIRES_FULL_SCAN));
    };
    let folder = call.args.get(4).and_then(|v| v.as_int()).unwrap_or(0);
    mutations::add(c, me, folder, subfolder_arg(call, 6), None, Some(TYPE_SOLAR_SYSTEM), system, pos, &value_text(call.args.get(1), MAX_NAME), &value_text(call.args.get(2), MAX_DESCRIPTION), expiry(call, 5)?).await
}
