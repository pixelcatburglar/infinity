//! Simplified Chinese operator messages for durable scheduler commands.
//! The slash service displays these as text; this module accesses no data source
//! and does not expose job payloads or implement scheduler behavior.

pub const HELP: &str = "/jobs | /jobs retry <任务ID> | /jobs cancel <任务ID> — 查看或管理定时任务";
pub const USAGE: &str = "用法：/jobs，/jobs retry <任务ID>，或 /jobs cancel <任务ID>。";
pub const COUNTS: &str = "任务数量（类型、状态、数量）：";
pub const UPCOMING: &str = "最近到期的活动任务（ID、类型、状态、FILETIME）：";
pub const EMPTY: &str = "（无）";

// Quote delimiters and controls, and escape markup before displaying operator data.
fn text(value: &str) -> String {
    let escaped = value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!("{escaped:?}")
}
pub fn count(kind: &str, state: &str, count: i64) -> String {
    format!("{}；{}；{count}", text(kind), text(state))
}
pub fn due(id: &str, kind: &str, state: &str, due_at: i64) -> String {
    format!("{}；{}；{}；{due_at}", text(id), text(kind), text(state))
}
pub fn missing(id: &str) -> String { format!("任务不存在：{}", text(id)) }
pub fn retried(id: &str) -> String { format!("已安排任务立即重试：{}", text(id)) }
pub fn cancelled(id: &str) -> String { format!("已取消任务：{}", text(id)) }
