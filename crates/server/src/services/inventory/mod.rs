//! Inventory broker and bound inventory routing for `eve/client/script/environment/invCache.py`
//! and `invCacheContainer`. Items and custody come from `game.items` and corporate
//! records, with type and volume data from the SDE and space reachability from the
//! system actor. Crystal damage reads use dogma overrides; ALSC configuration,
//! item-price reads and asset safety are not modelled. Stale writes and staged
//! trade items are guarded; deliveries obey corporate roles. Loot rights, suspect
//! flags, foreign shared-bay permissions and POS tower control are not implemented.

use crate::client::Client;
use crate::consts::{FLAG_FITTING_FIRST, FLAG_FITTING_LAST, FLAG_RIG_FIRST, FLAG_RIG_LAST, FLAG_SUBSYSTEM_FIRST, FLAG_SUBSYSTEM_LAST};
use super::{Call, CallResult};

pub(super) mod gm;
pub(crate) mod expert_systems;
pub(crate) mod movement;
pub(in crate::services) mod reachability;
pub(crate) mod access;
pub(in crate::services) mod notifications;
pub(crate) mod stacks;
pub(crate) mod labels;
pub(crate) mod trashing;
mod inv_broker;
mod crystals;
mod inv_inventory;

/// const.containerHangar: "my hangar in the current station".
const CONTAINER_HANGAR: i64 = 10004;
/// inventorycommon.const.containerCorpMarket: corporation station deliveries.
const CONTAINER_CORP_MARKET: i64 = 10012;
/// Internal bound-object name for a single inventory.
pub const INVENTORY: &str = "invInventory";

/// Fitting slot flags: low, medium, high, rig and subsystem slots.
pub(in crate::services) fn is_fitting_flag(flag: i32) -> bool {
    (FLAG_FITTING_FIRST..=FLAG_FITTING_LAST).contains(&flag) || (FLAG_RIG_FIRST..=FLAG_RIG_LAST).contains(&flag) || (FLAG_SUBSYSTEM_FIRST..=FLAG_SUBSYSTEM_LAST).contains(&flag)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        INVENTORY => Some(inv_inventory::inventory(c, call).await),
        "invbroker" => inv_broker::broker(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
