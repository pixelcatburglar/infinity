//! Chinese refusals for shared settings requests consumed by the client through
//! `sharedSettings/util.py`. Text is defined here, not loaded from a database;
//! localization beyond Chinese is not supported.
pub(crate) const AUTH: &str = "请先登录后再使用共享设置。";
pub(crate) const INVALID: &str = "共享设置数据无效或超出大小限制。";
