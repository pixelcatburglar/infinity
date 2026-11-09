//! Player-visible refusals for unavailable external platform services.
//! The guard and SDK callers cannot obtain external credentials, orders or URLs.
//! No data source or external platform integration is used.

pub const GUARD_UNAVAILABLE: &str = "外部安全服务不可用。";
pub const RMT_UNAVAILABLE: &str = "外部交易服务不可用。";
pub const AUTH_UNAVAILABLE: &str = "外部平台认证不可用。";
pub const PAYMENT_UNAVAILABLE: &str = "外部支付和兑换服务不可用。";
