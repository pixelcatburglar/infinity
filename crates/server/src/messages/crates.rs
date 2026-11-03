//! Player-visible refusals for unavailable crate contents and metadata.
//! Crate definitions come from startup static data and server policy; opening is unsupported.

pub(crate) const METADATA_UNAVAILABLE: &str = "该箱子的特殊奖励信息暂不可用。";
pub(crate) const OPENING_UNAVAILABLE: &str = "暂不支持开启箱子或领取箱子奖励。";
pub(crate) const INVALID_SOURCE: &str = "箱子不存在或无法访问。";
