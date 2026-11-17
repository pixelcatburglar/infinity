//! Player-visible corporation refusals for founding, share transfers, dividends and votes.
//! These texts accompany client corporation and voting calls; they contain no database state.

/// AddCorporation requires the complete fifteen-argument client signature.
pub const INVALID_CORPORATION_ARGUMENTS: &str = "军团创建参数无效。";
/// Client base_corporation.py KickOutMultipleMembers rejects a batch containing the caller.
pub const CANNOT_KICK_SELF_IN_MULTIKICK: &str = "CorpCantKickSelfInMultiKick";
pub const INVALID_SHARE_TRANSFER: &str = "无法转移这些股份。";
pub const SHARE_AUTHORITY_REQUIRED: &str = "你没有转移这些股份的权限。";
pub const SHARES_UNAVAILABLE: &str = "该军团的股份不可用。";
pub const INVALID_DIVIDEND: &str = "股息金额无效。";
pub const DIVIDEND_AUTHORITY_REQUIRED: &str = "只有军团 CEO 才能发放股息。";
pub const MEMBER_DIVIDEND_UNSUPPORTED: &str = "暂不支持向军团成员平均发放股息。";
pub const NO_DIVIDEND_RECIPIENTS: &str = "没有符合条件的股息收款人。";
pub const VOTE_UNSUPPORTED: &str = "不支持战争或未知类型的投票。";
pub const VOTE_INVALID: &str = "投票内容无效。";
pub const VOTE_AUTHORITY_REQUIRED: &str = "你没有创建投票的权限。";
pub const VOTE_NOT_VISIBLE: &str = "你无权查看该投票。";
pub const VOTE_NOT_ELIGIBLE: &str = "你没有该投票的表决权。";
pub const VOTE_CLOSED: &str = "该投票已经结束。";
pub const VOTE_ALREADY_CAST: &str = "你已经投过票，不能更改选项。";
pub const CEO_RESIGN_DOCK: &str = "请先停靠空间站，再辞去 CEO 职务。";
pub const DISSOLVE_ALLIANCE: &str = "军团仍在联盟中，无法解散。";
pub const DIVISION_CEO_REQUIRED: &str = "只有现任 CEO 才能修改部门名称。";
/// CEO 选举被占用时的拒绝。
pub const VOTE_ELECTION_ACTIVE: &str = "已有进行中的 CEO 选举。";
/// 决议只能由当前有效 CEO 执行。
pub const SANCTION_AUTHORITY_REQUIRED: &str = "只有现任 CEO 才能执行该决议。";
/// 决议的执行窗口已过。
pub const SANCTION_EXPIRED: &str = "该决议已过执行期限。";
/// 决议针对的股份、成员或蓝图已经失效。
pub const VOTE_TARGET_INVALID: &str = "该决议的目标已失效。";
/// 蓝图已被军团锁定或被未决投票保留。
pub const ITEM_LOCKED_OR_RESERVED: &str = "该蓝图已被军团锁定，或正被另一项投票保留。";
/// 蓝图不满足军团原件锁定条件。
pub const ITEM_LOCK_TARGET_INVALID: &str = "该蓝图不是可锁定的军团原件。";
/// 决议选项文案：CEO 选举。
pub const VOTE_OPTION_YES_CEO: &str = "由提案人接任 CEO。";
pub const VOTE_OPTION_NO_CEO: &str = "不更换 CEO。";
/// 决议选项文案：增发股份。
pub const VOTE_OPTION_YES_SHARES: &str = "增发新股。";
pub const VOTE_OPTION_NO_SHARES: &str = "不增发新股。";
/// 决议选项文案：驱逐成员。
pub const VOTE_OPTION_YES_KICK: &str = "将目标成员驱逐出军团。";
pub const VOTE_OPTION_NO_KICK: &str = "不驱逐该成员。";
/// 决议选项文案：锁定蓝图。
pub const VOTE_OPTION_YES_LOCKDOWN: &str = "锁定该蓝图。";
pub const VOTE_OPTION_NO_LOCKDOWN: &str = "不锁定该蓝图。";
/// 决议选项文案：解除锁定。
pub const VOTE_OPTION_YES_UNLOCK: &str = "解除该蓝图的锁定。";
pub const VOTE_OPTION_NO_UNLOCK: &str = "保持该蓝图的锁定。";

/// The client-compatible player corporation identifier range is exhausted.
pub const CORPORATION_IDS_EXHAUSTED: &str = "军团编号已用尽，无法创建军团。";
