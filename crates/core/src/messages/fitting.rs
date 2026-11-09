//! Fitting refusals for constraints presented as client UI labels rather than
//! catalogue errors. `feedbackLabelCont.py` names WarningNotForThisHullCategory,
//! WarningHullCantFitThisModule, WarningIncorrectSlotForModule,
//! WarningModuleSlotInUse, WarningRigNotRightSize, WarningNotEnoughTurretSlotsFree,
//! WarningNotEnoughLauncherSlots, WarningNotEnoughFreeSlots and the calibration
//! overload warning. Saved fitting input refuses malformed triples with one
//! CustomNotify message. No database data or client localisation resource is loaded.

pub const CATEGORY: &str = "此装备不能安装在该舰船类别上。";
pub const HULL: &str = "此装备不能安装在这艘舰船上。";
pub const RACK: &str = "此装备不能安装在该槽位。";
pub const OCCUPIED: &str = "该装备槽位已被占用。";
pub const RIG_SIZE: &str = "改装件尺寸不适合这艘舰船。";
pub const TURRET: &str = "没有足够的炮台安装位。";
pub const LAUNCHER: &str = "没有足够的发射器安装位。";
pub const CALIBRATION: &str = "舰船校准值不足。";
pub const SLOTS: &str = "没有足够的装备槽位。";
pub const BOARD_UNAVAILABLE: &str = "无法登上该舰船。";
/// Malformed saved fitting triples are rejected before any database write.
pub const INVALID_SAVED_DATA: &str = "保存的装配数据无效：物品类型和标志必须是范围内的整数，数量必须是范围内的正整数。";

/// Render missing skill names and required levels for the client error payload.
pub fn required_skills(skills: &[(String, i32)]) -> String {
    skills.iter().map(|(name, level)| format!("{name} {level}级")).collect::<Vec<_>>().join("、")
}
