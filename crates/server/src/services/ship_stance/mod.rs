//! Tactical ship stance state shared by the stance service and fitting
//! publication.
//! Client `shipmode/` exposes stances only through real hidden mode items
//! (`inventorycommon/const.py` flagHiddenModifers); this module owns the shared
//! types while `state` reads and repairs them and `switch` runs the switch
//! mechanic; `ship_stance_mgr` answers the client call.
pub(crate) mod state;
pub(in crate::services) mod switch;

use crate::dao::items::loading::Item;

/// The persisted stance of one hull: the real mode item plus its timing row,
/// with the current stance key derived from the item type.
pub(crate) struct StanceSnapshot {
    /// The recognized hidden mode item of the hull.
    pub(crate) item: Item,
    /// Stance key before the last accepted switch; `None` means never switched.
    pub(crate) previous_stance_id: Option<i64>,
    /// FILETIME when the last switch was accepted; `0` on initialization.
    pub(crate) switched_at: i64,
    /// FILETIME when the next switch is admitted; `0` means no cooldown.
    pub(crate) ready_at: i64,
    /// Current stance key derived from the mode item type.
    pub(crate) current: i64,
}

/// Facts of one committed stance switch handed to the fitting publication
/// receipt. This is a notification receipt, not a second writable current
/// state: the mode item remains the only persisted fact.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StanceTransition {
    /// Stance key before the switch.
    pub(crate) old: i64,
    /// Stance key after the switch.
    pub(crate) new: i64,
    /// FILETIME when the switch was accepted.
    pub(crate) switched_at: i64,
    /// The newly created mode item ID.
    pub(crate) new_item: i64,
}
