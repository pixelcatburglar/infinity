//! Player-visible weapon bank refusals.
//! These server-authored messages complement clientDogmaLocation's catalogue errors;
//! they use no database and do not localize SDE names.

pub const INVALID_MODULE: &str = "模块未安装在当前飞船的高槽中。";
pub const NOT_YOUR_SHIP: &str = "只能操作当前飞船的武器组。";
pub const TYPE_MISMATCH: &str = "只有相同类型的武器才能编组。";
pub const NOT_GROUPABLE: &str = "该模块不能加入武器组。";
pub const CHARGE_MISMATCH: &str = "武器的弹药类型和数量必须相同。";
pub const BANK_NOT_FOUND: &str = "武器组不存在。";
