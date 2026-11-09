//! Skill service player-facing refusals.
//! These messages cover invalid history requests, remaps and unavailable remap entitlements.
pub const INVALID_REMAP: &str = "属性重置的数值无效。";
pub const REMAP_UNAVAILABLE: &str = "当前没有可用的属性重置次数。";
pub const INVALID_HISTORY_REQUEST: &str = "技能历史记录请求无效。";
pub const INVALID_ECONOMY_REQUEST: &str = "技能物品或技能点请求无效。";
pub const ALPHA_INJECTOR_REQUIRES_ALPHA: &str = "只有 Alpha 克隆状态才能使用每日 Alpha 技能注入器。";

/// Multi-character training activation is unavailable on this server.
pub const MULTI_CHARACTER_TRAINING_UNSUPPORTED: &str = "本服务器暂不支持多角色训练。";
