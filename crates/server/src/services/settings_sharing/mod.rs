//! Routing for permanent item-filter and broadcast snapshots. Client callers include
//! `sharedSettings/util.py:GetDataFromSettingKey` and
//! `eve/client/script/ui/shared/inventory/filterSvc.py:OnFilterDragged`.
//! Data lives in `game.shared_setting_links`; revocation, expiry, private access
//! and overview links are not implemented.
mod codec;
mod shared_settings_mgr;

use crate::client::Client;
use crate::services::{Call, CallResult};

/// `sharedSettings/constants.py:SHARED_SETTING_ITEMS`.
const ITEMS: i32 = 2;
/// `sharedSettings/constants.py:SHARED_SETTING_BROADCAST`.
const BROADCAST: i32 = 3;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service == "sharedSettingsMgr" { shared_settings_mgr::dispatch(c, call).await } else { None }
}
