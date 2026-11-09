//! Player-visible planetary compatibility refusals.
//! The client uses `eveexceptions.UserError`; no colony gameplay is provided.
//! No data source is read and localisation beyond these messages is unsupported.

pub const UNAVAILABLE: &str = "行星开发暂不可用。";
pub const INVALID_PLANET: &str = "无法读取该行星的信息。";
