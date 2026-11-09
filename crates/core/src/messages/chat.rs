//! Server-authored chat refusal messages for RPC and XMPP clients.
pub const FORBIDDEN: &str = "你没有权限执行此聊天频道操作。";
pub const INVALID: &str = "聊天频道或角色无效。";
pub const NAME_TAKEN: &str = "该聊天频道名称已被使用。";
pub const UNSUPPORTED_PASSWORD: &str = "暂不支持聊天频道密码。";
pub const RESTRICTION_ABSENT: &str = "该角色当前没有对应的禁言或封禁。";
pub const UNSUPPORTED_AFFILIATION: &str = "暂不支持军团或联盟频道成员授权。";
