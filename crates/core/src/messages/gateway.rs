//! Gateway response text for requestBroker.py response consumers.
//! These server-authored messages use no database or client data and do not
//! expose internal diagnostics or provide localisation.
pub const INVALID_REQUEST: &str = "请求无效";
pub const UNAUTHORIZED: &str = "未登录或未选择角色";
pub const NOT_IMPLEMENTED: &str = "功能尚未实现";
pub const INTERNAL_ERROR: &str = "服务器内部错误";
pub const FORBIDDEN: &str = "无权执行此请求";
/// Server-authored gateway response text when a requested object is absent.
pub const NOT_FOUND: &str = "请求的对象不存在";
