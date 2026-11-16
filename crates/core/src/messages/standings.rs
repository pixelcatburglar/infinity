//! Standings text: the refusal of an absolute standing outside the client's
//! -10 to 10 range, standing-history fallbacks and access refusals. Reads no
//! tables and validates nothing.

/// Refusal of a non-finite standing or one outside -10..=10.
pub const STANDING_OUT_OF_RANGE: &str = "声望必须是 -10 到 10 之间的有限数值。";
/// History fallback shown for a GM slash-command standing change without a reason.
pub const REASON_GM_COMMAND: &str = "GM 命令。";
/// History fallback shown for a standing change caused by a server script.
pub const REASON_SCRIPT: &str = "脚本调整。";
/// History fallback shown for a mission standing change whose name is unavailable.
pub const REASON_MISSION: &str = "任务。";

/// Refusal when a standing history query carries wrong arguments or another character.
pub const HISTORY_INVALID: &str = "无效的声望历史查询。";
/// Refusal when no agent corporation or faction standing can be raised.
pub const NO_AGENT_OWNERS: &str = "没有可更新的代理人军团或势力。";
