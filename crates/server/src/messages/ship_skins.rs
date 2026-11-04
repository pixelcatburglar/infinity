//! Player-facing refusals for first-party ship SKIN operations.
//! The wire calls are consumed by `cosmeticsSvc.py`; license data comes from the SDE.
//! Structure, single-use and timed activation are not implemented here.

pub(crate) const INVALID_ARGUMENT: &str = "无效的涂装请求。";
pub(crate) const INVALID_ITEM: &str = "无法使用该涂装许可证或舰船。";
pub(crate) const INVALID_SKIN: &str = "该涂装不可用于此舰船。";
pub(crate) const CHANGED: &str = "物品状态已改变，请重试。";
pub(crate) const SINGLE_USE: &str = "暂不支持激活单次使用涂装许可证。";
pub(crate) const TIMED: &str = "暂不支持激活限时涂装许可证。";
pub(crate) const STRUCTURE: &str = "暂不支持激活建筑涂装许可证。";
