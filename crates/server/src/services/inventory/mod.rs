//! Inventory broker and bound inventory routing for `eve/client/script/environment/invCache.py`
//! and `invCacheContainer`. Items and custody come from `game.items` and corporate
//! records, with type and volume data from the SDE and space reachability from the
//! system actor. Crystal damage reads use dogma overrides; ALSC configuration,
//! item-price reads and asset safety are not modelled. Stale writes and staged
//! trade items are guarded; deliveries obey corporate roles. Shared-bay movement
//! uses owner-bound permissions and retains ownership on deposit; loot rights,
//! uses owner-bound permissions and retains ownership on deposit; loot rights,
//! suspect flags and POS tower control are not implemented. Ordinary NPC wreck
//! roots and their untaken loot live only in the owning system actor: reads and
//! in-place mutations never reach `game.items` or the economy lock, only the
//! taken slice is staged inside the existing movement transaction, and
//! persistent deposits into transient roots are refused.

use crate::client::Client;
use infinity_core::consts::{FLAG_FITTING_FIRST, FLAG_FITTING_LAST, FLAG_HIDDEN_MODIFIERS, FLAG_RIG_FIRST, FLAG_RIG_LAST, FLAG_SUBSYSTEM_FIRST, FLAG_SUBSYSTEM_LAST};
use super::{Call, CallResult};

pub(super) mod gm;
pub(crate) mod expert_systems;
pub(crate) mod holds;
pub(crate) mod hold_storage;
pub(crate) mod movement;
pub(crate) mod destination;
pub(crate) mod reachability;
pub(in crate::services) mod bay_access;
pub(crate) mod access;
pub(crate) mod notifications;
pub(in crate::services) mod lifecycle;
pub(in crate::services) mod debris;
pub(crate) mod stacks;
pub(crate) mod labels;
pub(crate) mod trashing;
mod inv_broker;
mod reads;
mod crystals;
pub(in crate::services) mod npc_loot;
mod plex;
mod inv_inventory;

/// const.containerHangar: "my hangar in the current station".
pub(in crate::services) const CONTAINER_HANGAR: i64 = 10004;
/// inventorycommon.const.containerCorpMarket: corporation station deliveries.
const CONTAINER_CORP_MARKET: i64 = 10012;
/// Internal bound-object name for a single inventory.
pub const INVENTORY: &str = "invInventory";

/// Fitting slot flags: low, medium, high, rig and subsystem slots.
pub(crate) fn is_fitting_flag(flag: i32) -> bool {
    (FLAG_FITTING_FIRST..=FLAG_FITTING_LAST).contains(&flag) || (FLAG_RIG_FIRST..=FLAG_RIG_LAST).contains(&flag) || (FLAG_SUBSYSTEM_FIRST..=FLAG_SUBSYSTEM_LAST).contains(&flag)
}

/// `inventorycommon/const.py` `flagHiddenModifers` (original spelling): the
/// internal tactical-mode flag; such items move only with their whole hull and
/// are never a selected move or destruction root.
pub(crate) fn is_internal_modifier_flag(flag: i32) -> bool {
    flag == FLAG_HIDDEN_MODIFIERS
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        INVENTORY => Some(inv_inventory::inventory(c, call).await),
        "invbroker" => inv_broker::broker(c, call).await,
        _ => None,
    }
}

#[cfg(all(test, feature = "tests-inventory"))] mod tests;
