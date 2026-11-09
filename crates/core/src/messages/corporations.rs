//! Player-visible corporation governance refusals for share transfers, dividends and votes.
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
pub const VOTE_UNSUPPORTED: &str = "暂不支持这种投票。";
pub const VOTE_INVALID: &str = "投票内容无效。";
pub const VOTE_AUTHORITY_REQUIRED: &str = "你没有创建投票的权限。";
pub const VOTE_NOT_VISIBLE: &str = "你无权查看该投票。";
pub const VOTE_NOT_ELIGIBLE: &str = "你没有该投票的表决权。";
pub const VOTE_CLOSED: &str = "该投票已经结束。";
pub const VOTE_ALREADY_CAST: &str = "你已经投过票，不能更改选项。";
pub const CEO_RESIGN_DOCK: &str = "请先停靠空间站，再辞去 CEO 职务。";
pub const DISSOLVE_ALLIANCE: &str = "军团仍在联盟中，无法解散。";
pub const DIVISION_CEO_REQUIRED: &str = "只有现任 CEO 才能修改部门名称。";
