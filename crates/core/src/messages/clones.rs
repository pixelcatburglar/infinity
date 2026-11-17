//! Clone service argument and authorization refusals.
//! Native catalogue errors for clone services live here alongside clone-specific refusals.

// Jump-clone station service native UserError catalogue keys.
pub const MSG_STANDINGS_RESTRICTION_GENERIC: &str = "StandingsRestrictionGeneric";
pub const MSG_NOT_IN_RIGHT_MILITIA_UNABLE_TO_USE_SERVICE: &str = "NotInRightMilitiaUnableToUseService";
pub const MSG_JUMP_CHECK_WILL_LOSE_EXISTING_CLONE: &str = "JumpCheckWillLoseExistingClone";
pub const INVALID_ARGUMENTS: &str = "远距克隆请求参数无效。";
pub const RESET_ROLE_REQUIRED: &str = "你没有重置远距克隆冷却时间的权限。";
pub const VALIDATION_CAPACITY: &str = "UI/Medical/Errors/JumpCloneValidationCloneCapacityReached";
pub const VALIDATION_FUNDS: &str = "UI/Medical/Errors/JumpCloneValidationInsufficientFunds";
pub const VALIDATION_BAY: &str = "UI/Medical/Errors/JumpCloneValidationNoCloneBay";
pub const VALIDATION_DOCKED: &str = "UI/Medical/Errors/JumpCloneValidationNotDocked";
pub const VALIDATION_SKILL: &str = "UI/Medical/Errors/JumpCloneValidationSkillReqNotFulfilled";
pub const VALIDATION_STANDINGS: &str = "UI/Medical/Errors/JumpCloneValidationStandingsRequirement";
pub const VALIDATION_OPPONENT: &str = "UI/Medical/Errors/JumpCloneValidationCannotUseOpponentFacility";
pub const VALIDATION_DUPLICATE: &str = "UI/Messages/1009/JumpAlreadyHaveStationCloneBody";
pub const CAPACITY_REACHED: &str = "跳跃克隆体数量已达到上限。";
pub const NO_CLONE_BAY: &str = "此空间站没有跳跃克隆设施。";
pub const INVALID_CLONE_NAME: &str = "克隆体名称不能超过100个字符。";
pub const COOLDOWN_ACTIVE: &str = "跳跃克隆冷却尚未结束。";
pub const STALE_BODY: &str = "当前身体状态已更改。";
pub const INVALID_IMPLANT_SLOTS: &str = "植入体槽位无效或冲突。";

// Docked clone self-destruction (`home_station.self_destruct_clone`).
pub const SELF_DESTRUCT_INVALID_ARGUMENTS: &str = "自毁克隆请求参数无效。";
pub const SELF_DESTRUCT_ALREADY_HOME: &str = "你已经位于家站，无需自毁当前克隆体。";
pub const SELF_DESTRUCT_NOT_DOCKED: &str = "你必须停靠在空间站内才能自毁当前克隆体。";
pub const SELF_DESTRUCT_NOT_CAPSULE: &str = "你必须先进入太空舱才能自毁当前克隆体。";

/// Every failed self-destruct precondition joined by newlines in the client's
/// validation order (already home, then not docked, then not in a capsule).
/// Call only when at least one condition failed; the result is never empty.
pub fn self_destruct_validation_error(already_home: bool, not_docked: bool, not_capsule: bool) -> String {
    let mut text = String::new();
    let mut append = |reason: &str| {
        if !text.is_empty() { text.push('\n'); }
        text.push_str(reason);
    };
    if already_home { append(SELF_DESTRUCT_ALREADY_HOME); }
    if not_docked { append(SELF_DESTRUCT_NOT_DOCKED); }
    if not_capsule { append(SELF_DESTRUCT_NOT_CAPSULE); }
    text
}
