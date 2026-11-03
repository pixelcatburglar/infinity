//! Inventory refusals and client catalogue text for unsupported inventory operations.
//! These messages mirror inventorysvc.py, invCache.py and invControllers.py; no
//! database or inventory state is accessed here.

/// ALSC configuration changes are not modelled and must not be acknowledged.
pub(crate) const ALSC_CONFIG_UNSUPPORTED: &str = "库存配置暂不支持。";
/// Item prices are unavailable until the market price source is modelled.
pub(crate) const ITEM_PRICES_UNSUPPORTED: &str = "物品价格暂不可用。";
