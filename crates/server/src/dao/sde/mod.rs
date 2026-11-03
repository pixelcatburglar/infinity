//! Static-data projections used to construct immutable startup datasets from typed SDE and selected native sources, including compression mappings, type lists and graphic locations.
//! This DAO has no direct client mirror; consumers include
//! `eve/common/script/dogma/baseDogmaLocation.py:1960-1967`,
//! `dogma/effects/modifiers.py:8-140`, `dogma/const.py:7-51`,
//! `eve/client/script/remote/michelle.py:491-532` and
//! `eve/client/script/parklife/autopilot.py:260-261,329-332`.
//! Other SDE tables remain with their existing consumers until migrated.
pub(crate) mod crates;
pub(crate) mod characters;
pub(crate) mod compression;
pub(crate) mod corporations;
pub(crate) mod dogma;
pub(crate) mod dungeons;
pub(crate) mod dynamic_items;
pub(crate) mod factions;
pub(crate) mod graphic_locations;
pub(crate) mod industry;
pub(crate) mod localization;
pub(crate) mod map;
pub(crate) mod missions;
pub(crate) mod store;
pub(crate) mod types;
pub(crate) mod typelists;
pub(crate) mod wrecks;
