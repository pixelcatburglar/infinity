//! Expert-system store category title, lifecycle and GM slash messages.
//! The client audit exposes no native `UserError` catalogue key for these
//! lifecycle refusals; they use server-authored Simplified Chinese text.

/// New Eden Store category title.
pub const STORE_CATEGORY: &str = "专家系统";

/// Expert-system type ID is invalid or absent from the SDE catalogue.
pub const INVALID: &str = "无效的专家系统。";
/// The requested expert system is not installed on this character.
pub const NOT_INSTALLED: &str = "未安装该专家系统。";
/// A concurrent lifecycle update won the race.
pub const CHANGED_RETRY: &str = "专家系统状态已改变，请重试。";
/// The consumed item cannot be found.
pub const ITEM_NOT_FOUND: &str = "找不到专家系统物品。";
/// The consumed item is not owned by the current character.
pub const ITEM_NOT_OWNED: &str = "专家系统物品不属于你。";
/// An installed system is not eligible for renewal yet.
pub const CANNOT_RENEW: &str = "暂时无法续期该专家系统。";
/// The configured duration cannot be represented.
pub const DURATION_OVERFLOW: &str = "专家系统时长超出范围。";
/// A character cannot have more than the lifecycle limit active.
pub const ACTIVE_LIMIT: &str = "最多只能安装三个专家系统。";
/// A supplied item type is not an expert system.
pub const INVALID_ITEM: &str = "无效的专家系统物品。";
