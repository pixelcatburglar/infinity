//! Player-facing ship service refusals for `shipConfigSvc.py` and ship operations.
//! These server-authored messages read no data and do not provide localisation.
//! Delegated POS operations are unavailable; no client state is changed here.
//! The cleaning and shared appearance messages apply to inaccessible hulls.
//! Voluntary eject has its own unavailability refusal and weapons-timer action text.

pub const CONFIGURATION_INACCESSIBLE: &str = "无法查看该舰船的舱室配置。";
pub const CLEANING_INACCESSIBLE: &str = "无法清洁或查看该舰船的污渍。";
/// Inaccessible physical hull appearance; shared by dirt, kill counts and reads.
pub const HULL_APPEARANCE_INACCESSIBLE: &str = "无法访问该舰船的外观状态。";
pub const LEAVE_UNAVAILABLE: &str = "暂不支持在建筑内离开舰船。";
pub const PASSWORD_UNAVAILABLE: &str = "暂不支持设置星基地力场密码。";
pub const TARGET_OBO_UNAVAILABLE: &str = "暂不支持代星基地或其他对象管理目标。";
/// Voluntary eject refused: not in space, stale session, not the piloted hull, already a capsule, warping or cloaked.
pub const EJECT_UNAVAILABLE: &str = "现在无法离开舰船。";
/// Action word for `messages::weapons_timer_message` when ejecting.
pub const WEAPONS_TIMER_EJECT: &str = "离开舰船";

/// Ship change refused: the shared ten-second interval since the last
/// committed change or live registration has not elapsed.
pub const SHIP_CHANGE_NOT_READY: &str = "更换舰船过于频繁，请稍后再试。";
/// Ship change refused: a CONCORD response is inbound for the pilot.
pub const SHIP_CHANGE_PURSUIT: &str = "统合部正在逼近，现在无法更换舰船。";
/// Foreign Board refused: another player holds a completed lock on the hull.
pub const SHIP_LOCKED_BY_OTHER: &str = "该舰船正被其他飞行员锁定，现在无法登机。";
