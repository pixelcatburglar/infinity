//! Inventory refusals and client catalogue text for unsupported inventory operations.
//! These messages mirror inventorysvc.py, invCache.py and invControllers.py; no
//! database or inventory state is accessed here.

/// ALSC configuration changes are not modelled and must not be acknowledged.
pub(crate) const ALSC_CONFIG_UNSUPPORTED: &str = "库存配置暂不支持。";
/// Item prices are unavailable until the market price source is modelled.
pub(crate) const ITEM_PRICES_UNSUPPORTED: &str = "物品价格暂不可用。";
/// Name is outside client limits or contains a control/newline character.
pub(crate) const NAME_INVALID: &str = "物品名称无效。";
/// This item type does not support client renaming.
pub(crate) const NAME_NOT_RENAMEABLE: &str = "该物品不能改名。";
pub(crate) const ITEM_WRONG_LOCATION: &str = "物品不在指定位置。";
