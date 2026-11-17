//! Titan positional-superweapon catalogue from the server's own embedded
//! content. Each row declares one module type's activation class: the area
//! shape matching the client's `doomsdayAOEShape` values, the positional
//! activation mode and the visual GUID family for OnSpecialFX presentation.
//! The driving dogma effect is derived from the class, never restated in
//! content, and startup validation checks it against the loaded type rows.
//! Unlisted group-588 neighbours — the Gravitational Transportation Field
//! Oscillator and every other refused type — are absent by design: admission
//! consults this index only. `arc_path_half_angle_proxy` is the single named
//! tunable (see `docs/limitations.md`); damage, timing, signature and range
//! values are never overridden here and come from the SDE and the
//! actor-resident fit.

use std::collections::HashMap;

use anyhow::bail;
use serde::Deserialize;
use infinity_core::consts::dogma::{EFFECT_DOOMSDAY_BEAM_DOT, EFFECT_DOOMSDAY_CONE_DOT, EFFECT_DOOMSDAY_SLASH};
use super::types::Types;

const CONTENT: &str = include_str!("../../content/super_weapons.json");

/// The hit-test family of one area weapon; the names match the client's
/// `doomsdayAOEShape` attribute values in `positionalControl.py:130-137`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Classification { Cylinder, Cone, Slice }
/// The positional RPC that drives the class: `positionalControl.py:111-120`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode { Point, Slash }
/// The OnSpecialFX GUID family, resolved with the type's SDE race.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GuidFamily { DamageBeam, SlashBeam, Cone }

impl Classification {
    fn parse(raw: &str) -> Option<Self> {
        match raw { "cylinder" => Some(Self::Cylinder), "cone" => Some(Self::Cone), "slice" => Some(Self::Slice), _ => None }
    }
    /// The only activation mode the class accepts.
    fn mode(self) -> Mode {
        match self { Self::Cylinder | Self::Cone => Mode::Point, Self::Slice => Mode::Slash }
    }
    /// The dogma effect this class fires; SDE `dgmEffects` IDs.
    fn effect(self) -> i32 {
        match self { Self::Cylinder => EFFECT_DOOMSDAY_BEAM_DOT, Self::Cone => EFFECT_DOOMSDAY_CONE_DOT, Self::Slice => EFFECT_DOOMSDAY_SLASH }
    }
    /// The only visual GUID family the class presents.
    fn family(self) -> GuidFamily {
        match self { Self::Cylinder => GuidFamily::DamageBeam, Self::Slice => GuidFamily::SlashBeam, Self::Cone => GuidFamily::Cone }
    }
}
impl Mode {
    fn parse(raw: &str) -> Option<Self> {
        match raw { "point" => Some(Self::Point), "slash" => Some(Self::Slash), _ => None }
    }
}
impl GuidFamily {
    fn parse(raw: &str) -> Option<Self> {
        match raw { "SWDamageBeam" => Some(Self::DamageBeam), "SWSlashBeam" => Some(Self::SlashBeam), "SuperWeaponCone" => Some(Self::Cone), _ => None }
    }
    /// Whether the family's GUIDs carry a racial suffix (`SWDamageBeam{race}`,
    /// `SWSlashBeam{race}`); the cone family is race-free.
    fn needs_race(self) -> bool { self != Self::Cone }
}

/// One admitted positional superweapon type.
pub(crate) struct Definition {
    pub(crate) type_id: i32,
    pub(crate) classification: Classification,
    pub(crate) mode: Mode,
    pub(crate) family: GuidFamily,
    /// The derived dogma effect this type fires.
    pub(crate) effect: i32,
    /// The type's SDE race, resolved at load; absent under test fixtures that
    /// skip type validation.
    pub(crate) race_id: Option<i32>,
}

/// SDE invTypes raceID → the client effect-GUID racial suffix.
pub(crate) fn race_suffix(race: Option<i32>) -> Option<&'static str> {
    match race? {
        1 => Some("Caldari"),
        2 => Some("Minmatar"),
        4 => Some("Amarr"),
        8 => Some("Gallente"),
        _ => None,
    }
}

/// The immutable catalogue, indexed by module type ID.
pub(crate) struct SuperWeapons {
    defs: Vec<Definition>,
    by_type: HashMap<i32, usize>,
    arc_path_half_angle_proxy: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContent { arc_path_half_angle_proxy: f64, weapons: Vec<RawWeapon> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWeapon { type_id: i32, classification: String, mode: String, guid_family: String }

impl SuperWeapons {
    /// Parse and validate the embedded catalogue against the loaded types:
    /// every row must name a real type carrying its derived effect, and a
    /// racial GUID family requires a resolvable SDE race.
    pub(crate) fn load(types: &Types) -> anyhow::Result<Self> {
        let mut this = Self::parse()?;
        for def in &mut this.defs {
            let Some(ty) = types.get(def.type_id) else { bail!("superweapon catalogue names unknown type {}", def.type_id) };
            def.race_id = ty.race_id;
            if !ty.effects().iter().any(|effect| effect.effect_id == def.effect) {
                bail!("superweapon type {} lacks derived effect {}", def.type_id, def.effect);
            }
            if def.family.needs_race() && race_suffix(ty.race_id).is_none() {
                bail!("superweapon type {} has no racial visual GUID", def.type_id);
            }
        }
        Ok(this)
    }
    /// Structural parse of the embedded content: consistent class, mode and
    /// GUID family, unique types and a positive proxy. Type rows are not
    /// consulted, so test defaults share exactly this validation.
    fn parse() -> anyhow::Result<Self> {
        let raw: RawContent = serde_json::from_str(CONTENT)?;
        if !raw.arc_path_half_angle_proxy.is_finite() || raw.arc_path_half_angle_proxy <= 0.0 {
            bail!("superweapon arc_path_half_angle_proxy must be a positive finite value");
        }
        if raw.weapons.is_empty() { bail!("superweapon catalogue is empty"); }
        let mut defs = Vec::with_capacity(raw.weapons.len());
        let mut by_type = HashMap::with_capacity(raw.weapons.len());
        for (index, row) in raw.weapons.iter().enumerate() {
            let Some(classification) = Classification::parse(&row.classification) else { bail!("superweapon {} has invalid classification", row.type_id) };
            let Some(mode) = Mode::parse(&row.mode) else { bail!("superweapon {} has invalid mode", row.type_id) };
            let Some(family) = GuidFamily::parse(&row.guid_family) else { bail!("superweapon {} has invalid guid_family", row.type_id) };
            if mode != classification.mode() { bail!("superweapon {} mode conflicts with its classification", row.type_id); }
            if family != classification.family() { bail!("superweapon {} guid_family conflicts with its classification", row.type_id); }
            if by_type.insert(row.type_id, index).is_some() { bail!("duplicate superweapon type {}", row.type_id); }
            defs.push(Definition {
                type_id: row.type_id,
                classification,
                mode,
                family,
                effect: classification.effect(),
                race_id: None,
            });
        }
        Ok(Self { defs, by_type, arc_path_half_angle_proxy: raw.arc_path_half_angle_proxy })
    }
    /// The admitted catalogue entry of one module type, if any.
    pub(crate) fn get(&self, type_id: i32) -> Option<&Definition> {
        self.by_type.get(&type_id).map(|&index| &self.defs[index])
    }
    /// The approved cone hit-test half-angle in degrees.
    pub(crate) fn arc_path_half_angle_proxy(&self) -> f64 { self.arc_path_half_angle_proxy }
}

#[cfg(test)]
impl Default for SuperWeapons {
    /// Test-only default: the same embedded content and structural
    /// validation as `load`, without type rows. Races stay unresolved.
    fn default() -> Self { Self::parse().expect("valid embedded superweapon catalogue") }
}

#[cfg(all(test, feature = "tests-sde"))]
mod tests;
