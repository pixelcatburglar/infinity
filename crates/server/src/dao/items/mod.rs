//! Item loading, inventory, creation, transfers, quantities, positions, charges,
//! ship, clone, combat, dogma, fitting, hacking, mining and social persistence.
//! Sources include `game.items`, `game.characters`, `game.module_charges`,
//! `game.pending_reloads`, `game.weapon_banks`, `game.fittings`, `game.blueprints`,
//! `game.industry_jobs`, `game.market_orders` and SDE type, attribute and station data.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod brain;
pub(crate) mod boarding;
pub(crate) mod charges;
pub(crate) mod crates;
pub(crate) mod clones;
pub(crate) mod combat;
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
pub(crate) mod pending_reloads;
pub(crate) mod positions;
pub(crate) mod quantities;
pub(crate) mod repair;
pub(crate) mod salvage;
pub(crate) mod ship;
pub(crate) mod space_ships;
pub(crate) mod skills;
pub(crate) mod social;
pub(crate) mod transfers;
