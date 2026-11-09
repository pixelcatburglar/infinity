//! Player-visible overload and heat damage refusals.
//! These messages use the existing CustomNotify client error.
//! Heat and fitting state are maintained by the effects services.
//! No client message-catalogue keys are introduced here.

pub const BURNT_OUT: &str = "该装备已烧毁，必须先修理才能使用。";
pub const INVALID_OVERLOAD: &str = "该装备无法超载。";
pub const THERMODYNAMICS_REQUIRED: &str = "热力学技能等级不足，无法超载该装备。";
pub const INVALID_RACK: &str = "无法超载该装备槽组。";
