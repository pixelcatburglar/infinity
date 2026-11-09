//! Refusals for encoded-item media decoding.
//! Mirrors the client's encoded-items menu flow; no database or item state is accessed.
//! Media decoding and item consumption are not supported.

/// Encoded-item media is unavailable and the item remains unchanged.
pub const DECODE_UNAVAILABLE: &str = "暂不支持解码此物品。";
