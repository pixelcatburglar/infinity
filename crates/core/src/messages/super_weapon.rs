//! Refusal text for positional superweapon activations.
//! `positionalControl.py:111-120` ignores the return value, so every refusal
//! reaches the client as a CustomNotify message.

/// Refusal for a module type or activation shape the superweapon catalogue
/// does not admit, including refused superweapon classes.
pub const UNSUPPORTED_TYPE: &str = "该装备不支持此超级武器操作。";
/// Refusal for a malformed point or slash request: wrong arity, a bad
/// identifier or non-finite coordinates.
pub const INVALID_REQUEST: &str = "无效的超级武器操作请求。";
/// The blanket refusal sent before the positional route existed.
pub const UNAVAILABLE: &str = "位置超级武器操作暂不可用。";

/// Refusal while a superweapon firing's immobility deadline still runs: the
/// firing ship may not warp.
pub const WARP_LOCKED: &str = "超级武器开火后的固定时间尚未结束，无法跃迁。";
/// Refusal while a superweapon firing's no-jump deadline still runs: the
/// firing ship may not use a gate or wormhole.
pub const JUMP_LOCKED: &str = "超级武器开火后的禁止跳跃时间尚未结束，无法跳跃。";
/// Refusal while a superweapon firing's no-cloak deadline still runs: the
/// firing ship's cloak may not activate.
pub const CLOAK_LOCKED: &str = "超级武器开火后的禁止隐形时间尚未结束，无法启动隐形装置。";
