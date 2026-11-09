//! Player-visible refusals for unavailable reports and support tickets.
//! Mirrors reportSvc.py, petition.py, petitionWindow.py and notificationCenter.py
//! refusal callers. These texts are server-authored and use no data source; they
//! do not implement reporting or support services.

pub const REPORT_UNAVAILABLE: &str = "举报服务暂不可用，无法提交举报。";
pub const PETITION_UNAVAILABLE: &str = "客服服务暂不可用，无法处理请求。";
