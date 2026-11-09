//! Player-visible New Eden Store purchase and redeem errors.
//! Mirrors vgsClient.py and redeemsvc.py; no database access or localisation lookup is performed.

/// A redeem token could not be delivered.
pub const REDEEM_FAILED: &str = "无法领取此物品，请稍后重试。";

/// External period purchases cannot be marked before payment.
pub const EXTERNAL_PAYMENT_UNSUPPORTED: &str = "暂不支持外部支付的周期商品，请使用商店内的 PLEX 商品。";
