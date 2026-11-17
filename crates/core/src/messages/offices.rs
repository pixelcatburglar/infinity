//! Player-visible text for corporation offices (services/offices).
//! The Infinity client displays Simplified Chinese.

/// Renting while a lease is already active; renewal goes through the rental bill.
pub const OFFICE_ALREADY_RENTED: &str = "该办公室已在租用中，请通过账单续租。";
/// The corporation has no office at this station.
pub const OFFICE_NOT_RENTED: &str = "你的军团在此空间站没有租用办公室。";
/// No free office slot at the station.
pub const OFFICE_NO_FREE_SLOTS: &str = "该空间站没有空余的办公室。";
/// The bound station does not support office rentals.
pub const OFFICE_STATION_UNSUPPORTED: &str = "该空间站不支持办公室租用。";
/// Nothing impounded can be released or destroyed here.
pub const OFFICE_IMPOUND_EMPTY: &str = "此处没有可赎回或销毁的扣押资产。";
/// The impound release quote no longer matches the current price.
pub const OFFICE_RELEASE_QUOTE_CHANGED: &str = "扣押赎回价格已变动，请重新查询。";
/// Impounded contents contain items that cannot be moved out or destroyed.
pub const OFFICE_IMPOUND_UNSAFE: &str = "扣押物品中包含无法移出或销毁的物品。";
