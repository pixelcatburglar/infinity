//! Player-facing ship service refusals for `shipConfigSvc.py` and ship operations.
//! These server-authored messages read no data and do not provide localisation.
//! Delegated POS operations are unavailable; no client state is changed here.
//! The cleaning message applies to inaccessible physical hulls.

pub const CONFIGURATION_INACCESSIBLE: &str = "无法查看该舰船的舱室配置。";
pub const CLEANING_INACCESSIBLE: &str = "无法清洁或查看该舰船的污渍。";
pub const LEAVE_UNAVAILABLE: &str = "暂不支持在建筑内离开舰船。";
pub const PASSWORD_UNAVAILABLE: &str = "暂不支持设置星基地力场密码。";
pub const TARGET_OBO_UNAVAILABLE: &str = "暂不支持代星基地或其他对象管理目标。";
