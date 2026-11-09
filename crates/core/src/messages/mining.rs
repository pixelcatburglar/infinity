//! Player-visible refusals for mining ledgers, moon mining and compression.
//! Clients include `ledgerSvc.py`, `schedulingSvc.py`, `auto_moon_miner_window.py`
//! and `itemCompression.py`; this module reads no data.

pub const INVALID_LEDGER_REQUEST: &str = "采矿记录请求包含无效参数。";
pub const COMPRESSION_ARGUMENT: &str = "压缩请求包含无效参数。";
pub const COMPRESSION_CONTEXT: &str = "只能在太空中使用当前飞船进行压缩。";
pub const COMPRESSION_ITEM: &str = "无法压缩该物品或其所在货舱。";
pub const COMPRESSION_TYPE: &str = "该物品无法压缩。";
pub const COMPRESSION_RESERVED: &str = "交易中预留的物品无法压缩。";
pub const COMPRESSION_FACILITY: &str = "压缩设施不可用、不受支持或超出范围。";
pub const COMPRESSION_CHANGED: &str = "物品已发生变化，请重试。";
pub const COMPRESSION_AUTO: &str = "自动压缩尚不受支持。";
pub const MOON_EXTRACTION_UNAVAILABLE: &str = "月矿开采尚不受支持。";
pub const STRUCTURE_COMPRESSION_UNAVAILABLE: &str = "建筑中的压缩和解压尚不受支持。";
pub const AUTO_MOON_MINING_UNAVAILABLE: &str = "自动月矿开采尚不受支持。";
