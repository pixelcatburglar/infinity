//! Player-visible clone grade request and authorization refusals.
//! These server-authored messages are used by the client slash service; they
//! read no database or other data source and do not implement grade mechanics
//! or localisation.

pub(crate) const INVALID_GRADE_REQUEST: &str = "克隆等级或时长请求无效。";
pub(crate) const GRADE_CHANGE_FORBIDDEN: &str = "你没有权限更改克隆等级。";
pub(crate) const ACCOUNT_NOT_FOUND: &str = "目标账号不存在。";
