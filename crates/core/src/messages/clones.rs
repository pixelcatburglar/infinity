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
