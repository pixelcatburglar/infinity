//! Player-visible overload and heat damage refusals.
//! These messages use the existing CustomNotify client error.
//! Heat and fitting state are maintained by the effects services.
//! The repair-material catalogue key lives in `messages.rs` with the other client keys.

pub const BURNT_OUT: &str = "该装备已烧毁，必须先修理才能使用。";
pub const INVALID_OVERLOAD: &str = "该装备无法超载。";
pub const THERMODYNAMICS_REQUIRED: &str = "热力学技能等级不足，无法超载该装备。";
pub const INVALID_RACK: &str = "无法超载该装备槽组。";
pub const BEING_REPAIRED: &str = "该装备正在修理中，暂时无法使用。";
pub const REPAIR_MODULE_ACTIVE: &str = "装备运行中，无法开始修理。";
pub const REPAIR_NOT_DAMAGED: &str = "该装备没有损伤，无需修理。";
