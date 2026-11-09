//! Character creation, selection, membership, roles, attributes, clones, ship and
//! location, Crimewatch, standings, identity, contacts and social data persistence.
//! Sources include `game.characters`, `game.items`, `game.corporations`, `game.corp_titles`,
//! `game.jump_clones`, `game.char_notes`, `game.char_contacts`, `game.char_blocked`,
//! `game.standings` and SDE owner, station and map data.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod attributes;
pub(crate) mod clones;
pub(crate) mod contacts;
pub(crate) mod creation;
pub(crate) mod crimewatch;
pub(crate) mod lookup;
pub(crate) mod members;
pub(crate) mod newbie;
pub(crate) mod persistence;
pub(crate) mod roles;
pub(crate) mod routing;
pub(crate) mod selection;
pub(crate) mod social;
pub(crate) mod ship;
pub(crate) mod standings;
