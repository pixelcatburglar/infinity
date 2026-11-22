//! Player-facing ship-stance refusals for `shipStanceMgr.SetShipStance`.
//! These server-authored messages read no data and do not provide localisation.
//! Corrupt mode items and timing rows are server integrity faults, not
//! player-visible refusals.

/// Not a valid stance switch for this hull, stance key or caller.
pub const INVALID_REQUEST: &str = "无效的舰船姿态切换请求。";
/// The hull does not carry stance modes.
pub const UNSUPPORTED_HULL: &str = "该舰船不支持战术姿态。";
/// The stance switch cooldown has not elapsed yet.
pub const SWITCH_NOT_READY: &str = "舰船姿态尚未就绪，无法切换。";
