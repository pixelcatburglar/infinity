//! Player-visible refusals for the repairable-ship recovery lifecycle.
//! The client's `repairableShipMgr` remote service raises UserError messages
//! through the shared catalogue; this module accesses no data.

/// The requested recovery group or member does not exist or belongs to another character.
pub const SELECTION_INVALID: &str = "回收项目不存在或不属于当前角色。";
/// The recovery payment window has already closed.
pub const PAYMENT_EXPIRED: &str = "支付期限已过，该回收项目已失效。";
/// A member's stored state no longer permits the requested action.
pub const STATE_CHANGED: &str = "回收项目状态已变更，请刷新后重试。";
/// Withdrawal was requested before delivery completed.
pub const NOT_DELIVERED: &str = "物品尚未送达，暂无法领取。";
/// The withdrawal window for a delivered item has already closed.
pub const WITHDRAWAL_EXPIRED: &str = "领取期限已过，物品已被清理。";
/// A destination declaration names a structure instead of an NPC station.
pub const DESTINATION_INVALID: &str = "回收目的地无效。";
/// One or more selected items cannot enter the disassembly flow.
pub const DISASSEMBLE_REJECTED: &str = "所选物品无法拆解。";
/// The refurbishment request is invalid or the components do not suffice.
pub const REFURBISHMENT_REJECTED: &str = "翻新请求无效或翻新组件不足。";
/// A damaged service-life item cannot be repackaged or used as mutation input.
pub const DAMAGED_OPERATION: &str = "物品剩余使用寿命不足，无法执行该操作。";
