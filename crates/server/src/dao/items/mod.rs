//! Item loading, inventory, creation, transfers, quantities, positions, charges,
//! ship, stance, clone, combat, dogma, fitting, hacking, mining, service-life and social persistence.
//! Sources include `game.items`, `game.characters`, `game.module_charges`,
//! `game.pending_reloads`, `game.weapon_banks`, `game.fittings`, `game.blueprints`,
//! `game.industry_jobs`, `game.market_orders`, `game.corp_item_locks`, vote-case
//! item reservations on `game.corp_vote_cases`, `game.item_service_life` and SDE type,
//! attribute and station data.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod assets;
pub(crate) mod brain;
pub(crate) mod boarding;
pub(crate) mod charges;
pub(crate) mod crates;
pub(crate) mod clones;
pub(crate) mod combat;
pub(crate) mod corporation_locks;
pub(crate) mod creation;
pub(crate) mod deliveries;
pub(crate) mod dogma;
pub(crate) mod drones;
pub(crate) mod fit;
pub(crate) mod fitting;
pub(crate) mod hacking;
pub(crate) mod inventory;
pub(crate) mod loading;
pub(crate) mod loyalty;
pub(crate) mod mining;
pub(crate) mod npc_loot;
pub(crate) mod parent_locks;
pub(crate) mod pending_reloads;
pub(crate) mod positions;
pub(crate) mod quantities;
pub(crate) mod repair;
pub(crate) mod repairable_ship;
pub(crate) mod salvage;
pub(crate) mod service_life;
pub(crate) mod ship;
pub(crate) mod ship_appearance;
pub(crate) mod ship_configuration;
pub(crate) mod ship_dirt;
pub(crate) mod ship_kill_counts;
pub(crate) mod ship_stance;
pub(crate) mod space_ships;
pub(crate) mod skills;
pub(crate) mod social;
pub(crate) mod sovereignty;
pub(crate) mod transfers;
