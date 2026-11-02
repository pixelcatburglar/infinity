//! Refusals for exploration scan requests.
//! These server-authored messages have no database or client source.
//! Other exploration messages are defined in the parent catalogue.

pub(crate) const QA_REQUIRED: &str = "此操作需要 QA 权限。";
pub(crate) const SPACE_REQUIRED: &str = "请先选择角色并进入太空。";
pub(crate) const INVALID_TARGETS: &str = "扫描目标参数无效。";
