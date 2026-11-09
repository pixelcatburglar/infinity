//! Plain data access for accounts, alliances, asteroids, bills, bookmarks, bounties, calendar,
//! characters, chat channels, client errors and crashes, contracts, corporations, dynamic items,
//! daily goals, exploration, industry, insurance, items, kill rights, killmails, mail, market,
//! mining ledger, missions, NPE, offices, orgs, ship SKINs, skills, store, wars and wallets.
//! Aggregates read and write `game` tables and use typed `sde` and `sde_native`
//! static data; individual headers describe their sources and coverage.
//! This layer has no direct client mirror or wire contract; owning services supply those.
//! Callers own policy, transactions, Outbox effects, refusals, notifications and World.

pub(crate) mod access_groups;
pub(crate) mod accounts;
pub(crate) mod alliances;
pub(crate) mod asteroids;
pub(crate) mod bills;
pub(crate) mod bookmarks;
pub(crate) mod bounties;
pub(crate) mod calendar;
pub(crate) mod characters;
pub(crate) mod chat_channels;
pub(crate) mod client_errors;
pub(crate) mod combat;
pub(crate) mod contracts;
pub(crate) mod corporations;
pub(crate) mod daily_goals;
pub(crate) mod dynamic_items;
pub(crate) mod exploration;
pub(crate) mod industry;
pub(crate) mod insurance;
pub(crate) mod items;
pub(crate) mod jobs;
pub(crate) mod kill_rights;
pub(crate) mod killmails;
pub(crate) mod lua_content;
pub(crate) mod lua_store;
pub(crate) mod lp_offers;
pub(crate) mod mail;
pub(crate) mod mailing_lists;
pub(crate) mod map_telemetry;
pub(crate) mod market;
pub(crate) mod mining_ledger;
pub(crate) mod missions;
pub(crate) mod npe;
pub(crate) mod offices;
pub(crate) mod orgs;
pub(crate) mod progress;
pub(crate) mod sde;
pub(crate) mod shared_settings;
pub(crate) mod ship_adaption;
pub(crate) mod ship_skins;
pub(crate) mod skills;
pub(crate) mod slash;
pub(crate) mod sovereignty;
pub(crate) mod store;
pub(crate) mod trades;
pub(crate) mod wallets;
pub(crate) mod wars;

#[cfg(test)]
pub(crate) mod tests;
