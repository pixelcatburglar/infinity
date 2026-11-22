//! Test-only `Default` for the static dataset; production constructs it
//! through `Sde::load`. The achievement catalogue has no derived default: it is
//! the verified retail payload, loaded exactly as the old inline impl did.
use super::*;

impl Default for Sde {
    fn default() -> Self {
        Self {
            achievements: infinity_content::achievements::Achievements::load(
                infinity_content::retail::cdn("achievements").expect("embedded achievement catalogue"),
            )
            .expect("valid embedded achievement catalogue"),
            contribution_methods: Default::default(),
            types: Default::default(),
            crates: Default::default(),
            sovereignty: Default::default(),
            compression: Default::default(),
            dogma: Default::default(),
            dbuffs: Default::default(),
            dynamic_items: Default::default(),
            expert_systems: Default::default(),
            skill_plans: Default::default(),
            accounting: Default::default(),
            map: Default::default(),
            missions: Default::default(),
            graphic_locations: Default::default(),
            dungeons: Default::default(),
            factions: Default::default(),
            corporation_roles: Default::default(),
            npc_corporations: Default::default(),
            localization: Default::default(),
            wrecks: Default::default(),
            industry: Default::default(),
            store: Default::default(),
            ship_skins: Default::default(),
            characters: Default::default(),
            rookie_fits: Default::default(),
            typelists: Default::default(),
            prices: Default::default(),
            lp_offers: Default::default(),
            repairable_ship: Default::default(),
            super_weapons: Default::default(),
        }
    }
}
