//! Player-visible refusals for station repair.
//! The client repair shop uses UserError messages; this module accesses no data.
//! Structure repair is not modelled.
/// The repair request is invalid or no longer matches the current quote.
pub const INVALID: &str = "维修请求无效或报价已变更，请重新获取报价。";
/// The current location has no supported repair service.
pub const UNSUPPORTED: &str = "当前位置不支持维修服务。";
