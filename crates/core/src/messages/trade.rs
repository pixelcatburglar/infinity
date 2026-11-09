//! Player-visible direct-trade refusals and client catalogue keys.
//! The trade window uses `eve/client/script/ui/shared/pvptrade/pvptradewnd.py`.
//! These strings do not cover fitted modules or asset safety.

pub const TRADE_SELF: &str = "不能与自己交易。";
pub const TRADE_NOT_DOCKED: &str = "停靠空间站后才能交易。";
pub const TRADE_TARGET_NOT_HERE: &str = "交易对象不在同一空间站。";
pub const TRADE_BUSY: &str = "已有进行中的交易。";
pub const TRADE_SOURCE_NOT_HANGAR: &str = "只能交易空间站机库中的物品。";
pub const TRADE_ITEM_RESERVED: &str = "交易中的物品不能进行此操作。";
pub const TRADE_CONTAINER_RESERVED: &str = "不能直接访问交易容器。";
pub const TRADE_PLEX_UNSUPPORTED: &str = "暂不支持从 PLEX 仓库直接交易。";
pub const MSG_PEOPLE_ABOARD_SHIP: &str = "PeopleAboardShip";
pub const MSG_CANNOT_TRADE_NON_TRADABLE: &str = "CannotTradeNonTradable";
