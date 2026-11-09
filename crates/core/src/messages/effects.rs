//! Server-authored dogma effect and warp refusal text.
//! The client displays this text through `eveexceptions.UserError` using the
//! `CustomNotify` catalogue key. Effect names come from SDE data and activation
//! requests; no database is read here. This module does not implement effects.

/// Refusal for an effect that the server does not implement.
pub fn effect_not_supported(effect: &str) -> String { format!("不支持效果：{effect}。") }

/// Warp refusal corresponding to the autopilot.py warp-scrambled message.
pub const WARP_SCRAMBLED: &str = "你的舰船受到跃迁扰断，无法跃迁。";

/// Cloaking refusals displayed through CustomNotify.
pub const CLOAK_TOO_CLOSE: &str = "距离附近物体太近，无法启动隐形装置。";
pub const CLOAK_REACTIVATION_DELAY: &str = "隐形装置尚未完成重新激活冷却。";
pub const CLOAK_TARGETING_DELAY: &str = "解除隐形后的锁定延迟尚未结束。";
pub const CLOAK_UNSUITABLE: &str = "当前舰船状态不允许启动隐形装置。";
pub const CLOAK_ALREADY_ACTIVE: &str = "隐形装置已经启动。";
/// Micro jump drive refusal displayed through CustomNotify.
pub const MICRO_JUMP_BLOCKED: &str = "舰船受到跃迁干扰，无法启动微型跳跃引擎。";
/// A module cannot start its cycle without its required fuel.
pub const INSUFFICIENT_FUEL: &str = "燃料不足，无法启动模块。";
/// The selected neutral Hub or the pilot's Entosis authority is unavailable.
pub const ENTOSIS_UNAVAILABLE: &str = "无法对当前目标启动主权链接。";
/// CCP Entosis Link: an active link prevents the ship from leaving or cloaking.
pub const ENTOSIS_ACTION_BLOCKED: &str = "主权链接仍在运行，无法执行此操作。";
/// CCP Entosis Link: capital ships cannot receive remote assistance while linked.
pub const ENTOSIS_ASSISTANCE_BLOCKED: &str = "目标正在使用主权链接，无法接受远程支援。";

/// Server policy: the bounded combat fact buffer cannot accept another cycle.
pub const COMBAT_CAPACITY_FULL: &str = "战斗记录暂时已满，模块效果无法生效，请稍后重试。";
