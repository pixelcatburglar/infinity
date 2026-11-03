//! Immutable server-startup datasets for inventory, compression, type lists, dogma, maps and other static game data.
//! The loader has no direct client mirror; consumers include `dogma/effects/modifiers.py:8-140`,
//! `dogma/const.py:7-51`, `eve/common/script/dogma/baseDogmaLocation.py:1960-1967`,
//! `eve/client/script/remote/michelle.py:491-532` and
//! `eve/client/script/parklife/autopilot.py:260-261,329-332`.
//! Sources are typed SDE, including races, bloodlines and NPC characters, and selected native
//! projections, including compression mappings, type lists, graphic locations and dynamic item definitions, via `dao::sde`.
//! Expert-system definitions are loaded from `sde_native.expertsystems` and validated
//! against typed inventory categories; lifecycle state and acquisition are not handled here.
//! Celestial collision radii are resolved here for the wire and placement callers.
//! Typed dogma_effect_modifiers and build_info are intentionally unused: modifiers come from dogma_effects instead.
//! Native blueprints and stationoperations are unused under the typed-table ruling.
pub(crate) mod characters;
pub(crate) mod compression;
pub(crate) mod corporations;
pub(crate) mod crates;
pub(crate) mod dogma;
pub(crate) mod dungeons;
pub(crate) mod dynamic_items;
pub(crate) mod expert_systems;
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

use crate::db::Db;
use std::time::Instant;

/// Existing World wire policy for celestial radii missing from both SDE rows.
const CELESTIAL_FALLBACK_RADIUS: f64 = 1000.0;

pub(crate) struct Sde {
    pub(crate) types: types::Types,
    pub(crate) crates: crates::Crates,
    pub(crate) compression: compression::Compression,
    pub(crate) dogma: dogma::Dogma,
    pub(crate) dynamic_items: dynamic_items::DynamicItems,
    pub(crate) expert_systems: expert_systems::ExpertSystems,
    pub(crate) map: map::Map,
    pub(crate) missions: missions::Missions,
    pub(crate) graphic_locations: graphic_locations::GraphicLocations,
    pub(crate) dungeons: dungeons::Dungeons,
    pub(crate) factions: factions::Factions,
    pub(crate) npc_corporations: corporations::NpcCorporations,
    pub(crate) localization: localization::Localization,
    pub(crate) wrecks: wrecks::Wrecks,
    pub(crate) industry: industry::Industry,
    pub(crate) store: store::Store,
    pub(crate) characters: characters::Characters,
    pub(crate) typelists: typelists::TypeLists,
}
impl Sde {
    pub(crate) fn celestial_radius(&self, celestial: &map::Celestial) -> f32 {
        celestial.radius.or_else(|| celestial.type_id.and_then(|id| i32::try_from(id).ok())
            .and_then(|id| self.types.get(id)).and_then(|ty| ty.radius))
            .unwrap_or(CELESTIAL_FALLBACK_RADIUS) as f32
    }

    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let started = Instant::now();
        let (mut types, dogma, map, factions, npc_corporations, localization, wrecks, industry, store, missions, mut dungeons, characters, graphic_locations, dynamic_rows) = tokio::try_join!(
            types::Types::load(db), dogma::Dogma::load(db), map::Map::load(db),
            factions::Factions::load(db), corporations::NpcCorporations::load(db), localization::Localization::load(db), wrecks::Wrecks::load(db),
            async { industry::Industry::load(db).await.map_err(anyhow::Error::from) },
            async { store::Store::load(db).await.map_err(anyhow::Error::from) },
            missions::Missions::load(db), dungeons::Dungeons::load(db),
            characters::Characters::load(db),
            graphic_locations::GraphicLocations::load(db),
            async { crate::dao::sde::dynamic_items::load(db).await.map_err(anyhow::Error::from) },
        )?;
        let (compression, typelists, crates, expert_systems) = tokio::try_join!(
            compression::Compression::load(db, &types),
            typelists::TypeLists::load(db, &types),
            crates::Crates::load(db, &types),
            expert_systems::ExpertSystems::load(db, &types),
        )?;
        let dynamic_items = dynamic_items::DynamicItems::from_rows(dynamic_rows, &types, &dogma);
        types.index_modifiers(&dogma);
        dungeons.index_map(&map);
        tracing::info!(types = types.len(), dogma_attributes = dogma.attribute_count(), dogma_effects = dogma.effect_count(), systems = map.systems().count(), stations = map.stations().count(), celestials = map.all_celestials().count(), factions = factions.len(), npc_corporations = npc_corporations.len(), localization = localization.len(), expert_systems = expert_systems.iter().count(), elapsed_ms = started.elapsed().as_millis(), "SDE loaded");
        Ok(Self { types, crates, compression, dogma, dynamic_items, expert_systems, map, graphic_locations, factions, npc_corporations, localization, wrecks, industry, store, missions, dungeons, characters, typelists })
    }
    /// Mirrors the client's non-tradable and soulbound type-list checks.
    pub(crate) fn is_tradable(&self, type_id: i32) -> bool {
        let data = self.types.get(type_id);
        self.typelists.is_tradable(type_id, data.and_then(|ty| ty.group_id), data.and_then(|ty| ty.category_id))
    }
}
