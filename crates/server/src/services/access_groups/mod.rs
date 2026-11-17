//! Access groups for `eve/client/script/ui/structure/accessGroups/` and shared bookmarks.
//! Membership is stored in game access-group tables and resolved against live affiliations.
//! Structure ACL policies and immediate affiliation-change cache eviction are unsupported.
mod authority;
mod owner_group_manager;

use std::sync::OnceLock;
use tokio::sync::{Mutex, MutexGuard};
use crate::services::{Call, CallResult, Client};
pub(crate) use authority::membership;

/// `ownergroups/ownergroupConst.py`: no matching membership.
pub(crate) const NONE: i32 = -1;
/// `ownergroups/ownergroupConst.py`: member.
pub(crate) const MEMBER: i32 = 0;
/// `ownergroups/ownergroupConst.py`: manager.
pub(crate) const MANAGER: i32 = 1;
/// `ownergroups/ownergroupConst.py`: administrator.
pub(crate) const ADMIN: i32 = 2;
/// `ownergroups/ownergroupConst.py`: excluded.
pub(crate) const EXCLUDED: i32 = 3;
/// `ownergroups/ownergroupConst.py`: public owner ID.
pub(crate) const PUBLIC: i64 = 0;

/// Serializes commits and notifications so revocation cannot be overtaken by an older
/// update. Shared reads also hold this gate so an old read cannot refill an evicted cache.
pub(crate) async fn lock() -> MutexGuard<'static, ()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(())).lock().await
}

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service == "ownerGroupManager" { owner_group_manager::dispatch(c, call).await } else { None }
}

#[cfg(all(test, feature = "tests-social"))] mod tests;
