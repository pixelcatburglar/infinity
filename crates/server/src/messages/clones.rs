//! Clone service argument and authorization refusals.
//! Native catalogue errors for clone services live here alongside clone-specific refusals.

// Jump-clone station service native UserError catalogue keys.
pub(crate) const MSG_STANDINGS_RESTRICTION_GENERIC: &str = "StandingsRestrictionGeneric";
pub(crate) const MSG_NOT_IN_RIGHT_MILITIA_UNABLE_TO_USE_SERVICE: &str = "NotInRightMilitiaUnableToUseService";
pub(crate) const MSG_JUMP_CHECK_WILL_LOSE_EXISTING_CLONE: &str = "JumpCheckWillLoseExistingClone";
pub(crate) const INVALID_ARGUMENTS: &str = "远距克隆请求参数无效。";
pub(crate) const RESET_ROLE_REQUIRED: &str = "你没有重置远距克隆冷却时间的权限。";
pub(crate) const VALIDATION_CAPACITY: &str = "UI/Medical/Errors/JumpCloneValidationCloneCapacityReached";
pub(crate) const VALIDATION_FUNDS: &str = "UI/Medical/Errors/JumpCloneValidationInsufficientFunds";
pub(crate) const VALIDATION_BAY: &str = "UI/Medical/Errors/JumpCloneValidationNoCloneBay";
pub(crate) const VALIDATION_DOCKED: &str = "UI/Medical/Errors/JumpCloneValidationNotDocked";
pub(crate) const VALIDATION_SKILL: &str = "UI/Medical/Errors/JumpCloneValidationSkillReqNotFulfilled";
pub(crate) const VALIDATION_STANDINGS: &str = "UI/Medical/Errors/JumpCloneValidationStandingsRequirement";
pub(crate) const VALIDATION_OPPONENT: &str = "UI/Medical/Errors/JumpCloneValidationCannotUseOpponentFacility";
pub(crate) const VALIDATION_DUPLICATE: &str = "UI/Messages/1009/JumpAlreadyHaveStationCloneBody";
pub(crate) const CAPACITY_REACHED: &str = "跳跃克隆体数量已达到上限。";
pub(crate) const NO_CLONE_BAY: &str = "此空间站没有跳跃克隆设施。";
pub(crate) const INVALID_CLONE_NAME: &str = "克隆体名称不能超过100个字符。";
pub(crate) const COOLDOWN_ACTIVE: &str = "跳跃克隆冷却尚未结束。";
pub(crate) const STALE_BODY: &str = "当前身体状态已更改。";
pub(crate) const INVALID_IMPLANT_SLOTS: &str = "植入体槽位无效或冲突。";
