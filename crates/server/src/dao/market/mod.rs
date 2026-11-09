//! Market startup seeding, atomic legacy cutover, orders, listings and trading persistence using
//! `game.market_orders`, `game.market_order_history`, `game.market_transactions`, `game.retail_prices`,
//! `game.market_book_versions`, `game.market_legacy_npc_backup`, the startup SDE station loader,
//! `sde_native.schools`, `sde.sta_stations`, `sde.inv_type_materials`, `sde.inv_types`
//! and the startup seed query `sql/market_seed.sql`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Matching policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod migration;
pub(crate) mod startup;
pub(crate) mod listings;
pub(crate) mod orders;
pub(crate) mod trading;
pub(crate) mod versions;

#[cfg(test)]
pub(crate) mod tests;
