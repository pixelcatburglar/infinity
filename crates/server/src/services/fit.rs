//! Effective dogma attributes for characters, ships, fittings, skills, active
//! brain items, drones and explicitly supplied incoming projections. Modifier
//! rows and defaults come from the SDE; owned items and overrides come from the
//! game database. Mirrors `dogma/effects/modifiers.py`, `dogma/const.py`, and
//! `dogma/attributes/skillLevelAttribute.py`. Brain snapshots filter boosters
//! at the evaluation time and apply only selected chance effects; historical
//! snapshots cover rows still present, not removed items. Projection lifetime
//! and overload activation are not managed here; callers provide a fit snapshot.
//! Unpiloted hulls include their fittings, overrides and supplied projections
//! without pilot skills, brain items, controlled drones or active module state.
//! Skill effects with fittingUsageChance require loading and filtering
//! that attribute if such a skill appears in the SDE.
//! Passive skill effects use effective levels, including expert-only grants,
//! once per skill type. The loader keeps the raw trained, installation and
//! brain rows it already read, so a live fit can replace its whole character
//! domain on a committed publish and let real booster and expert deadlines
//! expire in place without another query. Expired expert grants fall back to
//! trained levels; selected booster chance effects are never rerolled.
//! Committed fitting changes merge persisted fields into a current fit without
//! replacing its projections or runtime activation state.
//! A hull's persisted unlimited-cargo mode is tracked separately from numeric
//! evaluation and only reaches the wire representation, never the quota math.
//! The twelve damage resonance attributes are bounded below by
//! combat.resonance_floor at the public value boundary only when the attribute
//! has an authored source; operand reads through modifier_value and the memo
//! keep raw guarded results, and absent nodes or attributes stay zero.


pub(crate) mod attributes;
pub(crate) mod cargo;
pub(crate) mod character;

use super::{db_err, CallError};
use infinity_core::consts::{dogma::ATTR_MASS, dogma::ATTR_MAX_VELOCITY, dogma::ATTR_REQUIRED_SKILLS, dogma::ATTR_SPEED_FACTOR, CATEGORY_CHARGE, CATEGORY_SHIP, CATEGORY_SKILL, CATEGORY_SUBSYSTEM, dogma::EFFECT_AFTERBURNER, dogma::EFFECT_MICROWARPDRIVE, dogma::EFFECT_ONLINE};
use crate::dao::items::fit as fit_dao;
use crate::db::Db;
use crate::sde::{dogma::{Attribute, Modifier}, types::TypeData, Sde};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use infinity_core::consts::{dogma::ATTR_AGILITY, dogma::ATTR_SKILL_LEVEL, CATEGORY_DRONE, CATEGORY_IMPLANT, DRONE_FALLBACK_AGILITY, DRONE_FALLBACK_MASS};
use infinity_core::consts::dogma::{ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE};
use infinity_core::consts::dogma::{ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE};
use infinity_core::consts::dogma::{ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE};

/// SDE dogma effect category: passive.
const EFFECT_CATEGORY_PASSIVE: i32 = 0;
/// SDE dogma effect category: active.
const EFFECT_CATEGORY_ACTIVE: i32 = 1;
/// SDE dogma effect category: online.
const EFFECT_CATEGORY_ONLINE: i32 = 4;
/// SDE dgmAttributeTypes: speedBoostFactor.
const ATTR_SPEED_BOOST_FACTOR: i32 = 567;
/// SDE dogma modifier operation: PreAssign.
pub(crate) const OP_PRE_ASSIGN: i32 = -1;
/// SDE dogma modifier operation: PreMul.
pub(crate) const OP_PRE_MUL: i32 = 0;
/// SDE dogma modifier operation: PreDiv.
pub(crate) const OP_PRE_DIV: i32 = 1;
/// SDE dogma modifier operation: ModAdd.
pub(crate) const OP_MOD_ADD: i32 = 2;
/// SDE dogma modifier operation: ModSub.
pub(crate) const OP_MOD_SUB: i32 = 3;
/// SDE dogma modifier operation: PostMul.
pub(crate) const OP_POST_MUL: i32 = 4;
/// SDE dogma modifier operation: PostDiv.
pub(crate) const OP_POST_DIV: i32 = 5;
/// SDE dogma modifier operation: PostPercent.
pub(crate) const OP_POST_PERCENT: i32 = 6;
/// SDE dogma modifier operation: PostAssign.
pub(crate) const OP_POST_ASSIGN: i32 = 7;
/// Dogma PostPercentUnNerfed, evaluated after PostPercent.
pub(crate) const OP_POST_PERCENT_UNNERFED: i32 = 8;
/// Dogma effect category: targeted.
const EFFECT_CATEGORY_TARGET: i32 = 2;
/// Client dogma/attributes/stackingNurfedAttribute.py uses eight stacking denominators.
const STACKING_LIMIT: usize = 8;
/// Dogma effect category: overload.
const EFFECT_CATEGORY_OVERLOAD: i32 = 5;
/// Dogma stacking penalty curve divisor, used by the server fitting formula.
const STACKING_PENALTY_DIVISOR: f64 = 2.67;
/// Server dogma stacking penalty curve squares the normalized modifier index.
const STACKING_PENALTY_EXPONENT: i32 = 2;
/// Percentage points per whole multiplier.
pub(crate) const PERCENT_SCALE: f64 = 100.0;
/// SDE modifier skillTypeID sentinel selecting the source item's type.
const MODIFIER_SOURCE_ITEM_TYPE: i32 = -1;
/// The twelve effective damage resonance attributes bounded at the public
/// evaluation boundary: structure 109/110/111/113, armor 267-270, shield 271-274.
/// Hull resonance operand attributes 974-977 are modifier inputs, not bounded.
const RESONANCE_FLOOR_ATTRIBUTES: [i32; 12] = [
    ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE,
    ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE,
    ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE,
    ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE,
    ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE,
    ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE,
];


/// An item in the fit.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Key {
    Char,
    Ship,
    Module(i64),
    Charge(i64),
    Skill(i32),
    Implant(i64),
    Booster(i64),
    Drone(i64),
    Projected(i64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Location { Ship, Char, Space }

#[derive(Clone)]
struct Node {
    type_id: i32,
    // Positions remain stable in the immutable types owned by the retained Arc<Sde>.
    data_position: Option<usize>,
    location: Location,
    online: bool,
    active: bool,
    overloaded: bool,
    projected_effect: Option<i32>,
    receiver: Option<Key>,
    charge_type: Option<i32>,
    level: Option<i32>,
    overrides: Vec<(i32, f64)>,
    cargo_unlimited: bool,
    charge_values: Vec<(i32, f64)>,
}

fn physical_base(category: Option<i32>, attr: i32, value: Option<f64>) -> Option<f64> {
    if category == Some(CATEGORY_DRONE) && !value.is_some_and(|v| v > 0.0) {
        match attr {
            ATTR_MASS => return Some(DRONE_FALLBACK_MASS),
            ATTR_AGILITY => return Some(DRONE_FALLBACK_AGILITY),
            _ => {}
        }
    }
    value
}

pub(crate) fn base_attribute(data: Option<&TypeData>, meta: Option<&Attribute>, attr: i32) -> f64 {
    physical_base(data.and_then(|t| t.category_id), attr, data.and_then(|t| t.fit_attribute(attr)))
        .unwrap_or_else(|| meta.and_then(|m| m.default_value).unwrap_or(0.0))
}

impl Node {
    fn base(&self, data: Option<&TypeData>, meta: Option<&Attribute>, attr: i32) -> f64 {
        if attr == ATTR_SKILL_LEVEL {
            if let Some(level) = self.level { return level as f64; }
        }
        if let Ok(index) = self.overrides.binary_search_by_key(&attr, |entry| entry.0) {
            return self.overrides[index].1;
        }
        base_attribute(data, meta, attr)
    }
    fn charge_base(&self, data: Option<&TypeData>, meta: Option<&Attribute>, attr: i32) -> f64 {
        self.charge_values.binary_search_by_key(&attr, |entry| entry.0)
            .map(|index| self.charge_values[index].1)
            .unwrap_or_else(|_| base_attribute(data, meta, attr))
    }
}


// SDE positions remain stable in the immutable Arc<Sde>.

/// A ship or character and the items that modify it.
pub struct Fit {
    nodes: HashMap<Key, Node>,
    passive_modifiers: Vec<(Key, i32, usize)>,
    selected_effects: HashMap<i64, Vec<i32>>,
    passive_by_attribute: HashMap<i32, Vec<usize>>,
    sde: Arc<Sde>,
    memo: Mutex<HashMap<(Key, i32), f64>>,
    ship_id: Option<i64>,
    projection_revision: Option<u64>,
    drone_projection_revision: Option<u64>,
    character: Option<character::CharacterInputs>,
    character_at: i64,
    charge_incarnations: HashMap<i64, u64>,
    loaded_seeds: Vec<crate::space::combat::loaded::LoadedSeed>,
}

impl Fit {
    pub(crate) fn sde(&self) -> &Sde { &self.sde }

    /// Return the cached fitting state without reloading dogma data.
    pub(crate) fn module_state(&self, item: i64) -> Option<(bool, bool)> {
        self.nodes.get(&Key::Module(item)).map(|node| (node.online, node.active))
    }

    /// Fitted module nodes as (item id, type id) pairs without copying the fit.
    pub(crate) fn fitted_modules(&self) -> impl Iterator<Item = (i64, i32)> + '_ {
        self.nodes.iter().filter_map(|(key, node)| match key {
            Key::Module(item) => Some((*item, node.type_id)),
            _ => None,
        })
    }
    #[cfg(all(test, any(feature = "tests-ships", feature = "tests-slash")))]
    pub(crate) fn module_overloaded(&self, item: i64) -> Option<bool> { self.nodes.get(&Key::Module(item)).map(|node| node.overloaded) }

    /// Load a fit outside space, without incoming projections.
    pub async fn load(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, ship: i64) -> Result<Fit, CallError> {
        Self::load_projected(db, sde, char_id, ship, Vec::new()).await
    }

    /// Load a fitted hull without any pilot, skills, brain or controlled drones.
    pub(crate) async fn load_unpiloted(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, ship: i64) -> Result<Fit, CallError> {
        Self::load_unpiloted_projected(db, sde, ship, Vec::new()).await
    }

    /// Load an unpiloted fitted hull with explicit incoming projections.
    pub(crate) async fn load_unpiloted_projected(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, ship: i64, projections: Vec<super::effects::projection::Projection>) -> Result<Fit, CallError> {
        Self::load_at(db, sde, None, ship, projections, crate::client::filetime_now(), None, None).await
    }


    /// Snapshot incoming effects from the live system before loading the fit.
    pub(crate) async fn load_in_space(db: &(impl crate::db::executor::Executor + ?Sized), world: &crate::space::World, system: i64, char_id: i64, ship: i64) -> Result<Fit, CallError> {
        let (projections, revision) = world.with(system, move |s| (s.projections_on(ship), s.projection_revision(ship))).await
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
        let mut fit = Self::load_projected(db, world.sde().clone(), char_id, ship, projections).await?;
        fit.projection_revision = Some(revision);
        Ok(fit)
    }

    /// Revision of the actor snapshot, or None for a non-space fit.
    pub(crate) fn projection_revision(&self) -> Option<u64> { self.projection_revision }

    /// Construct an unpiloted space type using the same projection evaluator.
    pub(crate) fn for_space_type(sde: Arc<Sde>, type_id: i32, projections: Vec<super::effects::projection::Projection>) -> Fit {
        let mut nodes = HashMap::new();
        nodes.insert(Key::Ship, Node {
            type_id, data_position: sde.types.position(type_id), location: Location::Space,
            online: true, active: false, overloaded: false, projected_effect: None, receiver: None,
            charge_type: None, level: None, overrides: Vec::new(), cargo_unlimited: false, charge_values: Vec::new(),
        });
        Self::insert_projections(&sde, &mut nodes, Key::Ship, projections);
        Fit { nodes, selected_effects: HashMap::new(), passive_modifiers: Vec::new(), passive_by_attribute: HashMap::new(), sde, memo: Mutex::new(HashMap::new()), ship_id: None, projection_revision: None, drone_projection_revision: None, character: None, character_at: 0, charge_incarnations: HashMap::new(), loaded_seeds: Vec::new() }
    }

    fn insert_projections(sde: &Sde, nodes: &mut HashMap<Key, Node>, receiver: Key, projections: Vec<super::effects::projection::Projection>) {
        for p in projections {
            let mut values = p.values;
            values.sort_by_key(|v| v.0);
            let mut charge_values = p.charge_values;
            charge_values.sort_by_key(|v| v.0);
            let mut key = p.source_module;
            while nodes.contains_key(&Key::Projected(key)) { key = key.wrapping_add(1); }
            nodes.insert(Key::Projected(key), Node {
                type_id: p.source_type, data_position: sde.types.position(p.source_type),
                location: Location::Space, online: true, active: true, overloaded: false,
                projected_effect: Some(p.effect_id), receiver: Some(receiver), charge_type: p.charge_type, level: None, overrides: values, cargo_unlimited: false, charge_values,
            });
        }
    }

    /// Load the fit and explicit source-effective incoming projected effects.
    pub(crate) async fn load_projected(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, ship: i64, projections: Vec<super::effects::projection::Projection>) -> Result<Fit, CallError> {
        Self::load_at(db, sde, Some(char_id), ship, projections, crate::client::filetime_now(), None, None).await
    }

    /// Load a character-only snapshot at an explicit FILETIME.
    pub async fn load_character_at(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, at: i64) -> Result<Fit, CallError> {
        Self::load_at(db, sde, Some(char_id), 0, Vec::new(), at, None, None).await
    }

    /// Evaluate a character fit from supplied trained rows and expert grants.
    pub(crate) async fn load_character_with_skills_at(
        db: &(impl crate::db::executor::Executor + ?Sized),
        sde: Arc<Sde>,
        char_id: i64,
        at: i64,
        skills: &[crate::dao::skills::training::Skill],
        installations: &[crate::dao::skills::expert_systems::ExpertSystem],
    ) -> Result<Fit, CallError> {
        Self::load_at(db, sde, Some(char_id), 0, Vec::new(), at, Some(skills), Some(installations)).await
    }

    async fn load_at(
        db: &(impl crate::db::executor::Executor + ?Sized),
        sde: Arc<Sde>,
        char_id: Option<i64>,
        ship: i64,
        projections: Vec<super::effects::projection::Projection>,
        at: i64,
        supplied_skills: Option<&[crate::dao::skills::training::Skill]>,
        supplied_installations: Option<&[crate::dao::skills::expert_systems::ExpertSystem]>,
    ) -> Result<Fit, CallError> {
        let ship_type = crate::dao::items::inventory::type_id(db, ship).await.map_err(db_err)?.unwrap_or(0);
        let (trained, installations) = if let Some(skills) = supplied_skills {
            (skills.to_vec(), supplied_installations.unwrap_or(&[]).to_vec())
        } else if let Some(char_id) = char_id {
            tokio::try_join!(
                crate::dao::skills::training::current(db, char_id),
                crate::dao::skills::expert_systems::active(db, char_id, at),
            ).map_err(db_err)?
        } else {
            (Vec::new(), Vec::new())
        };
        let modules = fit_dao::modules(db, ship).await.map_err(db_err)?;
        let charges = crate::dao::items::charges::all(db, ship).await.map_err(db_err)?;
        let pendings: HashSet<i32> = if ship > 0 {
            crate::dao::items::pending_reloads::for_ship(db, ship).await.map_err(db_err)?.into_iter().map(|r| r.flag_id).collect()
        } else { HashSet::new() };
        let brain = if let Some(char_id) = char_id { fit_dao::brain_items(db, char_id, at).await.map_err(db_err)? } else { Vec::new() };
        let drones = if let Some(char_id) = char_id { fit_dao::drones(db, char_id, ship).await.map_err(db_err)? } else { Vec::new() };
        let ids: Vec<i64> = std::iter::once(ship).chain(char_id).chain(modules.iter().map(|m| m.item_id))
            .chain(brain.iter().map(|m| m.item_id)).chain(drones.iter().map(|m| m.item_id)).collect();
        let mut overrides: HashMap<i64, Vec<(i32, f64)>> = HashMap::new();
        let mut ship_cargo_unlimited = false;
        for row in crate::dao::items::dogma::overrides_for(db, &ids).await.map_err(db_err)? {
            if row.item_id == ship && row.attribute_id == infinity_core::consts::dogma::ATTR_CAPACITY {
                ship_cargo_unlimited = row.cargo_unlimited;
            }
            overrides.entry(row.item_id).or_default().push((row.attribute_id, row.value));
        }
        for values in overrides.values_mut() { values.sort_by_key(|entry| entry.0); }
        let character = if let Some(char_id) = char_id {
            let mut character: HashMap<i32, f64> = super::skills::attributes::base_character_attributes(db, &sde, char_id).await?
                .into_iter().filter_map(|(attr, value)| i32::try_from(attr).ok().map(|attr| (attr, value))).collect();
            character.extend(overrides.remove(&char_id).unwrap_or_default());
            let mut character: Vec<_> = character.into_iter().collect();
            character.sort_by_key(|v| v.0);
            Some(character)
        } else { None };
        let active = if char_id.is_some() { super::combat::active_modules() } else { HashSet::new() };
        let overloaded = if char_id.is_some() { super::effects::heat::overloaded_modules() } else { HashSet::new() };
        let mut nodes = HashMap::new();
        let node = |type_id, location, online, active, level, overrides| Node {
            type_id, data_position: sde.types.position(type_id), location, online, active,
            overloaded: false, projected_effect: None, receiver: None, charge_type: None, level, overrides, cargo_unlimited: false, charge_values: Vec::new(),
        };

        let mut ship_node = node(ship_type, Location::Space, true, false, None, overrides.remove(&ship).unwrap_or_default());
        ship_node.cargo_unlimited = ship_cargo_unlimited;
        nodes.insert(Key::Ship, ship_node);

        let mut module_by_flag = HashMap::new();
        for r in &modules {
            module_by_flag.insert(r.flag_id, r.item_id);
            let mut module = node(r.type_id, Location::Ship, r.online, char_id.is_some() && r.online && active.contains(&r.item_id), None, overrides.remove(&r.item_id).unwrap_or_default());
            module.overloaded = char_id.is_some() && r.online && overloaded.contains(&r.item_id);
            nodes.insert(Key::Module(r.item_id), module);
        }
        let mut charge_incarnations = HashMap::new();
        for r in &charges {
            if let Some(m) = module_by_flag.get(&r.flag_id) {
                nodes.insert(Key::Charge(*m), node(r.type_id, Location::Ship, true, false, None, Vec::new()));
                charge_incarnations.insert(*m, crate::space::targeting::next_generation());
            }
        }
        let brain_inputs: Vec<character::BrainInput> = brain.into_iter().map(|item| character::BrainInput {
            overrides: overrides.remove(&item.item_id).unwrap_or_default(),
            item,
        }).collect();
        for r in &drones {
            if sde.types.get(r.type_id).and_then(|d| d.category_id) == Some(CATEGORY_DRONE) {
                nodes.insert(Key::Drone(r.item_id), node(r.type_id, Location::Space, true, false, None, overrides.remove(&r.item_id).unwrap_or_default()));
            }
        }
        Self::insert_projections(&sde, &mut nodes, Key::Ship, projections);
        let character = char_id.map(|_| character::CharacterInputs {
            at, attributes: character, trained, installations, brain: brain_inputs });
        let ship_id = (ship > 0 && sde.types.get(ship_type).and_then(|data| data.category_id) == Some(CATEGORY_SHIP)).then_some(ship);
        let charged: HashMap<i32, &crate::dao::items::charges::Charge> = charges.iter().map(|r| (r.flag_id, r)).collect();
        let loaded_seeds = modules.iter().filter_map(|m| {
            let charge = charged.get(&m.flag_id);
            (charge.is_some() || pendings.contains(&m.flag_id)).then_some(crate::space::combat::loaded::LoadedSeed {
                module: m.item_id, flag: m.flag_id, type_id: charge.map(|c| c.type_id),
                quantity: charge.map_or(0, |c| c.quantity), pending: pendings.contains(&m.flag_id) })
        }).collect();
        let mut fit = Fit { nodes, selected_effects: HashMap::new(), passive_modifiers: Vec::new(), passive_by_attribute: HashMap::new(), sde, memo: Mutex::new(HashMap::new()), ship_id, projection_revision: None, drone_projection_revision: None, character, character_at: at, charge_incarnations, loaded_seeds };
        fit.rebuild_character_domain(at);
        Ok(fit)
    }

    /// Include a prospective fitted item with its persisted dogma overrides.
    pub(crate) async fn include_module(&mut self, db: &(impl crate::db::executor::Executor + ?Sized), item: i64, type_id: i32) -> Result<(), CallError> {
        if self.nodes.contains_key(&Key::Module(item)) { return Ok(()); }
        let mut overrides: Vec<_> = crate::dao::items::dogma::overrides_for(db, &[item]).await.map_err(db_err)?
            .into_iter().map(|r| (r.attribute_id, r.value)).collect();
        overrides.sort_by_key(|v| v.0);
        self.nodes.insert(Key::Module(item), Node {
            type_id, data_position: self.sde.types.position(type_id), location: Location::Ship,
            online: false, active: false, overloaded: false, projected_effect: None, receiver: None,
            charge_type: None, level: None, overrides, cargo_unlimited: false, charge_values: Vec::new(),
        });
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        Ok(())
    }

    /// Replace or add an owned drone, invalidating values that depended on its old state.
    pub(crate) fn include_drone(&mut self, id: i64, type_id: i32, mut overrides: Vec<(i32, f64)>) {
        overrides.sort_by_key(|v| v.0);
        self.nodes.insert(Key::Drone(id), Node {
            type_id, data_position: self.sde.types.position(type_id),
            location: Location::Space, online: true, active: false, overloaded: false,
            projected_effect: None, receiver: None, charge_type: None, level: None, overrides, cargo_unlimited: false, charge_values: Vec::new(),
        });
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
    }

    /// Change a prospective module state and invalidate all dependent values.
    pub(crate) fn set_module_online(&mut self, item: i64, online: bool) -> bool {
        let Some(node) = self.nodes.get_mut(&Key::Module(item)) else { return false };
        if node.online != online {
            node.online = online;
            if !online { node.active = false; node.overloaded = false; }
            crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        }
        true
    }


    pub(crate) fn replace_projections_for(&mut self, receiver: Key, projections: Vec<super::effects::projection::Projection>, revision: Option<u64>) {
        self.nodes.retain(|key, node| !matches!(key, Key::Projected(_)) || node.receiver != Some(receiver));
        Self::insert_projections(&self.sde, &mut self.nodes, receiver, projections);
        match receiver {
            Key::Ship => self.projection_revision = revision,
            Key::Drone(_) => self.drone_projection_revision = revision,
            _ => {}
        }
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
    }

    /// Change a prospective module active flag and invalidate dependent
    /// values; returns whether the bit actually changed.
    pub(crate) fn set_module_active(&mut self, item: i64, active: bool) -> bool {
        let Some(node) = self.nodes.get_mut(&Key::Module(item)) else { return false };
        if node.active == active { return false; }
        node.active = active;
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        true
    }

    pub(crate) fn set_module_overloaded(&mut self, item: i64, overloaded: bool) -> bool {
        let Some(node) = self.nodes.get_mut(&Key::Module(item)) else { return false };
        if node.overloaded != overloaded {
            node.overloaded = overloaded;
            crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        }
        true
    }

    /// Load the selected character ship and one owned drone, including launched drones.
    pub(crate) async fn drone(db: &Db, sde: Arc<Sde>, char_id: i64, drone_item: i64) -> Result<Fit, CallError> {
        let ship = crate::dao::characters::selection::ship_id(db, char_id).await.map_err(db_err)?
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("character has no selected ship")))?;
        Self::load_drone(db, sde, char_id, ship, drone_item, Vec::new(), Vec::new()).await
    }

    /// Snapshot both receiving objects before loading their owner fit.
    pub(crate) async fn drone_in_space(db: &Db, world: &crate::space::World, system: i64, char_id: i64, drone_item: i64) -> Result<Fit, CallError> {
        let ship = crate::dao::characters::selection::ship_id(db, char_id).await.map_err(db_err)?
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("character has no selected ship")))?;
        let (ship_projections, ship_revision, drone_projections, drone_revision) = world.with(system, move |s|
            (s.projections_on(ship), s.projection_revision(ship), s.projections_on(drone_item), s.projection_revision(drone_item)))
            .await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
        let mut fit = Self::load_drone(db, world.sde().clone(), char_id, ship, drone_item, ship_projections, drone_projections).await?;
        fit.projection_revision = Some(ship_revision);
        fit.drone_projection_revision = Some(drone_revision);
        Ok(fit)
    }

    /// Revision of the drone projection snapshot, if present.
    pub(crate) fn drone_projection_revision(&self) -> Option<u64> { self.drone_projection_revision }

    pub(crate) async fn load_drone(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, ship: i64, drone_item: i64,
        ship_projections: Vec<super::effects::projection::Projection>, drone_projections: Vec<super::effects::projection::Projection>) -> Result<Fit, CallError> {
        use crate::dao::items::loading::{load_items, ItemQuery};
        let item = load_items(db, ItemQuery::Id(drone_item)).await.map_err(db_err)?
            .into_iter().find(|item| item.owner_id == char_id && sde.types.get(item.type_id).and_then(|d| d.category_id) == Some(CATEGORY_DRONE))
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("owned drone not found")))?;
        let mut fit = Self::load_projected(db, sde, char_id, ship, ship_projections).await?;
        let overrides = crate::dao::items::dogma::overrides_for(db, &[drone_item]).await.map_err(db_err)?
            .into_iter().map(|r| (r.attribute_id, r.value)).collect();
        fit.include_drone(drone_item, item.type_id, overrides);
        Self::insert_projections(&fit.sde, &mut fit.nodes, Key::Drone(drone_item), drone_projections);
        crate::poison::lock_or_exit(fit.memo.get_mut()).clear();
        Ok(fit)
    }

    /// Visit passive skill and active brain-item modifiers in stable order.
    pub(crate) fn brain_modifiers(&self, mut visit: impl FnMut(Key, &Modifier)) {
        self.visit_passive_modifiers(0..self.passive_modifiers.len(), &mut visit);
        let mut keys: Vec<_> = self.nodes.keys().copied().filter(|k| matches!(k, Key::Implant(_) | Key::Booster(_))).collect();
        keys.sort_unstable();
        for key in keys {
            let node = &self.nodes[&key];
            if let Some(data) = node.data_position.map(|i| self.sde.types.at_position(i)) {
                for entry in data.effects() {
                    if let Some(effect) = self.sde.dogma.effect(entry.effect_id) {
                        if effect.effect_category.unwrap_or(0) == EFFECT_CATEGORY_PASSIVE && chance_enabled(&self.selected_effects, key, data, effect) {
                            for modifier in effect.modifiers() { visit(key, modifier); }
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn source_type(&self, source: Key) -> Option<i32> {
        self.nodes.get(&source).map(|node| node.type_id)
    }

    fn visit_passive_modifiers(&self, indices: impl IntoIterator<Item = usize>, mut visit: impl FnMut(Key, &Modifier)) {
        for index in indices {
            let (source, effect_id, position) = self.passive_modifiers[index];
            let modifier = &self.sde.dogma.effect(effect_id).expect("indexed skill effect").modifiers()[position];
            visit(source, modifier);
        }
    }

    pub(crate) fn required_skill(&self, source: Key, skill: Option<i32>) -> Option<i32> {
        match skill {
            Some(MODIFIER_SOURCE_ITEM_TYPE) | None => self.nodes.get(&source).map(|n| n.type_id),
            value => value,
        }
    }
    pub(crate) fn overrides(&self, key: Key) -> &[(i32, f64)] {
        self.nodes.get(&key).map_or(&[], |node| node.overrides.as_slice())
    }

    pub(crate) fn type_has_attribute(&self, key: Key, attr: i32) -> bool {
        self.nodes.get(&key).and_then(|node| node.data_position)
            .is_some_and(|position| attr == infinity_core::consts::dogma::ATTR_CAPACITY || self.sde.types.at_position(position).attribute(attr).is_some())
    }


    pub(crate) fn base(&self, key: Key, attr: i32) -> f64 {
        let Some(node) = self.nodes.get(&key) else { return 0.0 };
        node.base(node.data_position.map(|i| self.sde.types.at_position(i)), self.sde.dogma.attribute(attr), attr)
    }

    /// Does modifier `m` from `source` apply to `target`?
    fn applies(&self, source: Key, m: &Modifier, target: Key) -> bool {
        let Some(t) = self.nodes.get(&target) else { return false };
        // targetID denotes the receiving hull, including when the modified item
        // is a module located on it.
        if m.target_category.is_some() {
            let receiver = self.nodes.get(&source).and_then(|node| node.receiver).unwrap_or(Key::Ship);
            let category = self.nodes.get(&receiver).and_then(|hull| hull.data_position)
                .and_then(|i| self.sde.types.at_position(i).category_id);
            if !category_matches(m.target_category, category) { return false; }
        }
        let group = t.data_position.and_then(|i| self.sde.types.at_position(i).group_id);
        let required = |skill: Option<i32>| {
            let skill = self.required_skill(source, skill);
            skill.is_some_and(|s| t.data_position.is_some_and(|i| {
                let data = self.sde.types.at_position(i);
                ATTR_REQUIRED_SKILLS.iter().filter_map(|a| data.fit_attribute(*a)).any(|v| v as i32 == s && s > 0) }))
        };
        modifier_applies(source, self.nodes.get(&source).and_then(|node| node.receiver), m, target, t, group, required(m.skill))
    }

    /// The effective value of `attr` on `key`. The twelve damage resonance
    /// attributes are bounded below by combat.resonance_floor at this public
    /// boundary only when `key` actually carries the attribute; operand reads
    /// use modifier_value for raw results.
    pub fn value(&self, key: Key, attr: i32) -> f64 {
        let value = self.value_guarded(key, attr, &mut HashSet::new());
        if RESONANCE_FLOOR_ATTRIBUTES.contains(&attr) && self.attribute_present(key, attr) { bounded(attr, value) } else { value }
    }

    /// The raw effective value of `attr` on `key` for dogma operand reads, with
    /// no resonance boundary applied. Brain compilation and projected snapshots
    /// consume operands through this so the floor cannot leak into arithmetic.
    pub(crate) fn modifier_value(&self, key: Key, attr: i32) -> f64 {
        self.value_guarded(key, attr, &mut HashSet::new())
    }

    /// Whether `key` carries an authored source for `attr`: an override, a type
    /// attribute, or an attribute default. The same inputs `base` reads.
    /// Missing nodes and absent attributes keep the historical zero result, so
    /// the resonance floor bounds only values that actually exist.
    fn attribute_present(&self, key: Key, attr: i32) -> bool {
        let Some(node) = self.nodes.get(&key) else { return false };
        node.overrides.binary_search_by_key(&attr, |entry| entry.0).is_ok()
            || node.data_position.is_some_and(|i| self.sde.types.at_position(i).fit_attribute(attr).is_some())
            || self.sde.dogma.attribute(attr).is_some_and(|meta| meta.default_value.is_some())
    }

    fn value_guarded(&self, key: Key, attr: i32, visiting: &mut HashSet<(Key, i32)>) -> f64 {
        if let Some(value) = crate::poison::lock_or_exit(self.memo.lock()).get(&(key, attr)).copied() {
            return value;
        }
        let base = self.base(key, attr);
        if attr == ATTR_SKILL_LEVEL && matches!(key, Key::Skill(_)) { return base; }
        if matches!(key, Key::Projected(_)) { return base; }
        if !visiting.insert((key, attr)) {
            return base;
        }
        // (operation, value, penalized)
        let mut mods: Vec<(i32, f64, bool)> = Vec::new();
        self.visit_passive_modifiers(self.passive_by_attribute.get(&attr).into_iter().flatten().copied(), |source, m| {
            if self.applies(source, m, key) {
                let v = self.value_guarded(source, m.modifying, visiting);
                mods.push((m.op, v, false));
            }
        });
        if self.sde.types.modifies_attribute(attr) || self.nodes.values().any(|node|
            node.projected_effect.and_then(|id| self.sde.dogma.effect(id))
                .is_some_and(|effect| effect.modifiers().iter().any(|m| m.modified == attr))) {
            let stackable = self.sde.dogma.attribute(attr).and_then(|m| m.stackable).unwrap_or(true);
            let mut sources: Vec<_> = self.nodes.keys().copied().collect();
            sources.sort_unstable();
            for source in sources {
                let node = &self.nodes[&source];
                if matches!(source, Key::Skill(_)) { continue; }
                let Some(data) = node.data_position.map(|i| self.sde.types.at_position(i)) else { continue };
                if let Some(effect_id) = node.projected_effect {
                    if let Some(effect) = self.sde.dogma.effect(effect_id) {
                        for m in effect.modifiers().iter().filter(|m| m.modified == attr) {
                            if !effect_running(source, node, effect_id, effect.effect_category.unwrap_or(0), &m.domain)
                                || !self.applies(source, m, key) { continue; }
                            let value = projected_value(node, Some(data), self.sde.dogma.attribute(m.modifying), m.modifying);
                            mods.push((m.op, value, !stackable && !stacking_exempt(data.category_id.unwrap_or(0))));
                        }
                    }
                } else {
                    for (effect, m) in data.modifiers(&self.sde.dogma, attr) {
                        if !chance_enabled(&self.selected_effects, source, data, effect) || !effect_running(source, node, effect.effect_id, effect.effect_category.unwrap_or(0), &m.domain) { continue; }
                        if !self.applies(source, m, key) { continue; }
                        let value = self.value_guarded(source, m.modifying, visiting);
                        mods.push((m.op, value, !stackable && !stacking_exempt(data.category_id.unwrap_or(0))));
                    }
                }
                if let Some(charge) = node.charge_type.and_then(|t| self.sde.types.get(t)) {
                    for (_, m) in charge.modifiers(&self.sde.dogma, attr) {
                        if m.domain != "targetID" || !self.applies(source, m, key) { continue; }
                        let value = node.charge_base(Some(charge), self.sde.dogma.attribute(m.modifying), m.modifying);
                        mods.push((m.op, value, false));
                    }
                }
            }
        }
        if key == Key::Ship {
            mods.extend(self.propulsion(attr, visiting));
        }
        visiting.remove(&(key, attr));
        let value = apply(base, &mods);
        crate::poison::lock_or_exit(self.memo.lock()).insert((key, attr), value);
        value
    }
}

fn chance_enabled(selected: &HashMap<i64, Vec<i32>>, source: Key, data: &TypeData, effect: &crate::sde::dogma::Effect) -> bool {
    if !effect.fitting_usage_chance_attribute_id.is_some_and(|id| data.attribute(id).is_some()) { return true; }
    match source {
        Key::Booster(id) => selected.get(&id).is_some_and(|ids| ids.binary_search(&effect.effect_id).is_ok()),
        Key::Skill(_) | Key::Implant(_) => false,
        _ => true,
    }
}

fn stacking_exempt(category: i32) -> bool {
    matches!(category, CATEGORY_SHIP | CATEGORY_CHARGE | CATEGORY_SKILL | CATEGORY_IMPLANT | CATEGORY_SUBSYSTEM)
}

fn projected_value(node: &Node, data: Option<&TypeData>, meta: Option<&Attribute>, attr: i32) -> f64 {
    node.base(data, meta, attr)
}

/// Boundary clamp for the twelve damage resonance attributes: finite values
/// below combat.resonance_floor rise to the floor; nonfinite results and every
/// other attribute pass through unchanged. Only Fit::value applies this, and
/// only for attributes with an authored source. The guarded evaluator, the
/// memo and operand reads keep raw values.
fn bounded(attr: i32, value: f64) -> f64 {
    if !RESONANCE_FLOOR_ATTRIBUTES.contains(&attr) || !value.is_finite() { return value; }
    value.max(crate::config::game().combat.resonance_floor)
}

fn effect_running(source: Key, node: &Node, effect_id: i32, category: i32, domain: &str) -> bool {
    if matches!(source, Key::Projected(_)) {
        return node.projected_effect == Some(effect_id) && domain == "targetID";
    }
    // The SDE categorizes online as active, but the client's OnlineEffect
    // lifecycle applies its modifiers while the module is online, not active.
    if effect_id == EFFECT_ONLINE as i32 {
        return node.online;
    }
    match category {
        EFFECT_CATEGORY_PASSIVE => true,
        EFFECT_CATEGORY_ONLINE => node.online,
        EFFECT_CATEGORY_ACTIVE => node.active,
        EFFECT_CATEGORY_TARGET => false,
        EFFECT_CATEGORY_OVERLOAD => node.online && node.overloaded,
        _ => false,
    }
}

fn category_matches(required: Option<i32>, actual: Option<i32>) -> bool {
    required.is_none() || required == actual
}

fn modifier_applies(source: Key, receiver: Option<Key>, m: &Modifier, target: Key, node: &Node, group: Option<i32>, required: bool) -> bool {
    let domain = match m.domain.as_str() {
        "itemID" => Some(source),
        "shipID" => Some(Key::Ship),
        "charID" => Some(Key::Char),
        "targetID" if matches!(source, Key::Projected(_)) => receiver,
        "otherID" => match source {
            Key::Module(id) => Some(Key::Charge(id)),
            Key::Charge(id) => Some(Key::Module(id)),
            _ => None,
        },
        _ => None,
    };
    let located = match domain {
        Some(Key::Ship) => node.location == Location::Ship || matches!(target, Key::Drone(_)),
        Some(Key::Char) => (target != Key::Char && node.location == Location::Char) || matches!(target, Key::Drone(_)),
        _ => false,
    };
    match m.func.as_str() {
        "ItemModifier" => domain == Some(target),
        "LocationModifier" => located,
        "LocationGroupModifier" => located && m.group.is_some() && m.group == group,
        "LocationRequiredSkillModifier" => located && required,
        "OwnerRequiredSkillModifier" => domain == Some(Key::Char) && (node.location == Location::Ship || matches!(target, Key::Ship | Key::Drone(_))) && required,
        _ => false,
    }
}

impl Fit {
    /// Running propulsion's computed max-velocity multiplier; mass and signature
    /// modifiers come from the ordinary SDE modifier index.
    fn propulsion(&self, attr: i32, visiting: &mut HashSet<(Key, i32)>) -> Vec<(i32, f64, bool)> {
        if attr != ATTR_MAX_VELOCITY {
            return vec![];
        }
        let mut mods = vec![];
        for (&key, node) in &self.nodes {
            let effects = node.data_position.map(|i| self.sde.types.at_position(i).effects()).unwrap_or(&[]);
            let mwd = effects.iter().any(|effect| effect.effect_id == EFFECT_MICROWARPDRIVE);
            if !matches!(key, Key::Module(_)) || !node.active || !(mwd || effects.iter().any(|effect| effect.effect_id == EFFECT_AFTERBURNER)) {
                continue;
            }
            let mass = self.value_guarded(Key::Ship, ATTR_MASS, visiting).max(1.0);
            let factor = self.value_guarded(key, ATTR_SPEED_FACTOR, visiting) / PERCENT_SCALE;
            let thrust = self.value_guarded(key, ATTR_SPEED_BOOST_FACTOR, visiting);
            mods.push((OP_POST_MUL, 1.0 + factor * thrust / mass, false));
        }
        mods
    }
}

/// Apply dogma operations in order: PreAssign, PreMul, PreDiv, ModAdd, ModSub, PostMul,
/// PostDiv, PostPercent, PostAssign; penalized multipliers diminish (1 + (m-1)e^-(i/2.67)^2).
fn apply(base: f64, mods: &[(i32, f64, bool)]) -> f64 {
    let mut v = base;
    let of = |op: i32| mods.iter().filter(move |m| m.0 == op);
    if let Some(m) = of(OP_PRE_ASSIGN).last() {
        v = m.1;
    }
    let multiply = |v: f64, factors: Vec<(f64, bool)>| -> f64 {
        let mut out = v;
        let (mut up, mut down): (Vec<f64>, Vec<f64>) = (vec![], vec![]);
        for (f, penalized) in factors {
            if !penalized {
                out *= f;
            } else if f >= 1.0 {
                up.push(f);
            } else {
                down.push(f);
            }
        }
        up.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        down.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for list in [up, down] {
            for (i, f) in list.into_iter().take(STACKING_LIMIT).enumerate() {
                out *= 1.0 + (f - 1.0) * (-((i as f64) / STACKING_PENALTY_DIVISOR).powi(STACKING_PENALTY_EXPONENT)).exp();
            }
        }
        out
    };
    v = multiply(v, of(OP_PRE_MUL).map(|m| (m.1, m.2)).collect());
    v = multiply(v, of(OP_PRE_DIV).filter(|m| m.1 != 0.0).map(|m| (1.0 / m.1, m.2)).collect());
    v += of(OP_MOD_ADD).map(|m| m.1).sum::<f64>();
    v -= of(OP_MOD_SUB).map(|m| m.1).sum::<f64>();
    v = multiply(v, of(OP_POST_MUL).map(|m| (m.1, m.2)).collect());
    v = multiply(v, of(OP_POST_DIV).filter(|m| m.1 != 0.0).map(|m| (1.0 / m.1, m.2)).collect());
    v = multiply(v, of(OP_POST_PERCENT).map(|m| (1.0 + m.1 / PERCENT_SCALE, m.2)).collect());
    v = multiply(v, of(OP_POST_PERCENT_UNNERFED).map(|m| (1.0 + m.1 / PERCENT_SCALE, false)).collect());
    if let Some(m) = of(OP_POST_ASSIGN).last() {
        v = m.1;
    }
    v
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
