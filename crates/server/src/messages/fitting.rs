//! Fitting refusals for constraints presented as client UI labels rather than
//! catalogue errors. `feedbackLabelCont.py` names WarningNotForThisHullCategory,
//! WarningHullCantFitThisModule, WarningIncorrectSlotForModule,
//! WarningModuleSlotInUse, WarningRigNotRightSize, WarningNotEnoughTurretSlotsFree,
//! WarningNotEnoughLauncherSlots, WarningNotEnoughFreeSlots and the calibration
//! overload warning. No database data or client localisation resource is loaded.

pub(crate) const CATEGORY: &str = "此装备不能安装在该舰船类别上。";
pub(crate) const HULL: &str = "此装备不能安装在这艘舰船上。";
pub(crate) const RACK: &str = "此装备不能安装在该槽位。";
pub(crate) const OCCUPIED: &str = "该装备槽位已被占用。";
pub(crate) const RIG_SIZE: &str = "改装件尺寸不适合这艘舰船。";
pub(crate) const TURRET: &str = "没有足够的炮台安装位。";
pub(crate) const LAUNCHER: &str = "没有足够的发射器安装位。";
pub(crate) const CALIBRATION: &str = "舰船校准值不足。";
pub(crate) const SLOTS: &str = "没有足够的装备槽位。";

/// Render missing skill names and required levels for the client error payload.
pub(crate) fn required_skills(skills: &[(String, i32)]) -> String {
    skills.iter().map(|(name, level)| format!("{name} {level}级")).collect::<Vec<_>>().join("、")
}
