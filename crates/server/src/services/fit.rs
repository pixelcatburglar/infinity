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
//! replacing its projections or runtime activation state. Timed runtime
//! modifier sources land per `(receiver, collection, source)` scalar and fold
//! through the evaluator like every other operand; their storage and the
//! aggregate fold live in `fit/runtime.rs`.
//! A hull's persisted unlimited-cargo mode is tracked separately from numeric
//! evaluation and only reaches the wire representation, never the quota math.
//! The twelve damage resonance attributes are bounded below by
//! combat.resonance_floor at the public value boundary only when the attribute
//! has an authored source; operand reads through modifier_value and the memo
//! keep raw guarded results, and absent nodes or attributes stay zero.
//! A running adaptive armor hardener keeps its adapted operand profile as a
//! transient fit overlay (`reactive_armor.rs`); it is cleared by module
//! removal, offline, inactive or a committed replacement.


pub(crate) mod attributes;
pub(crate) mod cargo;
pub(crate) mod character;
pub(crate) mod loading;
mod evaluator;
pub(crate) mod reactive_armor;
pub(crate) mod runtime;
use self::runtime::RuntimeModifierSource;

use super::{db_err, CallError};
use infinity_core::consts::{dogma::ATTR_MASS, CATEGORY_MODULE, CATEGORY_SHIP, CATEGORY_SUBSYSTEM};
use crate::dao::items::fit as fit_dao;
use crate::db::Db;
use crate::sde::{dogma::{Attribute, Modifier}, types::TypeData, Sde};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use infinity_core::consts::{dogma::ATTR_AGILITY, dogma::ATTR_SKILL_LEVEL, CATEGORY_DRONE, DRONE_FALLBACK_AGILITY, DRONE_FALLBACK_MASS};

/// SDE dogma effect category: passive.
const EFFECT_CATEGORY_PASSIVE: i32 = 0;
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
/// Percentage points per whole multiplier.
pub(crate) const PERCENT_SCALE: f64 = 100.0;
/// SDE modifier skillTypeID sentinel selecting the source item's type.
const MODIFIER_SOURCE_ITEM_TYPE: i32 = -1;


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

/// Modifier sources admitted by a scoped evaluation. `All` is the ordinary
/// effective value. `Modules` (category Module sources) and `Hull` (the ship
/// and category Subsystem sources) mirror the two origins the client's probe
/// scanner breakdown attributes separately
/// (probescanning/scanBonusController.py GetBonusFromModulesToNurfedAttribute,
/// GetBonusFromShip, GetBonusFromSubsystems).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SourceScope { All, Modules, Hull }

/// Whether `scope` counts `source` of category `category`.
fn scope_admits(scope: SourceScope, source: Key, category: Option<i32>) -> bool {
    match scope {
        SourceScope::All => true,
        SourceScope::Modules => matches!(source, Key::Module(_)) && category == Some(CATEGORY_MODULE),
        SourceScope::Hull => source == Key::Ship || (matches!(source, Key::Module(_)) && category == Some(CATEGORY_SUBSYSTEM)),
    }
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
    projection_resistance_attribute: Option<i32>,
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
    /// Runtime modifier sources anchored to this fit's receivers, sorted by
    /// `(receiver, collection_id, source)`; see `runtime.rs`.
    runtime_sources: Vec<RuntimeModifierSource>,

    /// The running adaptive armor hardener's transient operand profile; see
    /// `reactive_armor.rs`. Never persisted and absent from every cold load.
    reactive_armor: Option<reactive_armor::ReactiveArmorOverride>,
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
        Self::load_at(db, sde, None, ship, projections, crate::client::filetime_now(), None, None, Vec::new()).await
    }


    /// Snapshot incoming effects from the live system before loading the fit.
    pub(crate) async fn load_in_space(db: &(impl crate::db::executor::Executor + ?Sized), world: &crate::space::World, system: i64, char_id: i64, ship: i64) -> Result<Fit, CallError> {
        let (projections, revision, runtime_sources) = world.with(system, move |s|
            (s.projections_on(ship), s.projection_revision(ship),
                s.runtime_sources_anchored(ship, s.ball(ship).and_then(|ball| ball.char_id)))).await
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
        let mut fit = Self::load_projected_with_runtime(db, world.sde().clone(), char_id, ship, projections, runtime_sources).await?;
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
            online: true, active: false, overloaded: false, projected_effect: None, projection_resistance_attribute: None, receiver: None,
            charge_type: None, level: None, overrides: Vec::new(), cargo_unlimited: false, charge_values: Vec::new(),
        });
        Self::insert_projections(&sde, &mut nodes, Key::Ship, projections);
        Fit { nodes, selected_effects: HashMap::new(), passive_modifiers: Vec::new(), passive_by_attribute: HashMap::new(), sde, memo: Mutex::new(HashMap::new()), ship_id: None, projection_revision: None, drone_projection_revision: None, character: None, character_at: 0, charge_incarnations: HashMap::new(), loaded_seeds: Vec::new(), runtime_sources: Vec::new(), reactive_armor: None }
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
                projected_effect: Some(p.effect_id), projection_resistance_attribute: p.resistance_attribute, receiver: Some(receiver), charge_type: p.charge_type, level: None, overrides: values, cargo_unlimited: false, charge_values,
            });
        }
    }

    /// Load the fit and explicit source-effective incoming projected effects.
    pub(crate) async fn load_projected(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, ship: i64, projections: Vec<super::effects::projection::Projection>) -> Result<Fit, CallError> {
        Self::load_projected_with_runtime(db, sde, char_id, ship, projections, Vec::new()).await
    }

    /// The same load with the resident runtime modifier sources captured at
    /// the actor alongside the projections.
    async fn load_projected_with_runtime(
        db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, ship: i64,
        projections: Vec<super::effects::projection::Projection>,
        runtime_sources: Vec<RuntimeModifierSource>,
    ) -> Result<Fit, CallError> {
        Self::load_at(db, sde, Some(char_id), ship, projections, crate::client::filetime_now(), None, None, runtime_sources).await
    }

    /// Load a character-only snapshot at an explicit FILETIME.
    pub async fn load_character_at(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, at: i64) -> Result<Fit, CallError> {
        Self::load_at(db, sde, Some(char_id), 0, Vec::new(), at, None, None, Vec::new()).await
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
        Self::load_at(db, sde, Some(char_id), 0, Vec::new(), at, Some(skills), Some(installations), Vec::new()).await
    }


    /// Include a prospective fitted item with its persisted dogma overrides.
    pub(crate) async fn include_module(&mut self, db: &(impl crate::db::executor::Executor + ?Sized), item: i64, type_id: i32) -> Result<(), CallError> {
        if self.nodes.contains_key(&Key::Module(item)) { return Ok(()); }
        let mut overrides: Vec<_> = crate::dao::items::dogma::overrides_for(db, &[item]).await.map_err(db_err)?
            .into_iter().map(|r| (r.attribute_id, r.value)).collect();
        overrides.sort_by_key(|v| v.0);
        self.nodes.insert(Key::Module(item), Node {
            type_id, data_position: self.sde.types.position(type_id), location: Location::Ship,
            online: false, active: false, overloaded: false, projected_effect: None, projection_resistance_attribute: None, receiver: None,
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
            projected_effect: None, projection_resistance_attribute: None, receiver: None, charge_type: None, level: None, overrides, cargo_unlimited: false, charge_values: Vec::new(),
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
        if !online { self.clear_reactive_armor(item); }
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
        if !active { self.clear_reactive_armor(item); }
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
        let (ship_projections, ship_revision, drone_projections, drone_revision, runtime_sources) = world.with(system, move |s| {
            let mut sources = s.runtime_sources_anchored(ship, s.ball(ship).and_then(|b| b.char_id));
            sources.extend(s.runtime_sources_anchored(drone_item, None));
            (s.projections_on(ship), s.projection_revision(ship), s.projections_on(drone_item), s.projection_revision(drone_item), sources)
        }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
        let mut fit = Self::load_drone_with_runtime(db, world.sde().clone(), char_id, ship, drone_item, ship_projections, drone_projections, runtime_sources).await?;
        fit.projection_revision = Some(ship_revision);
        fit.drone_projection_revision = Some(drone_revision);
        Ok(fit)
    }

    /// Revision of the drone projection snapshot, if present.
    pub(crate) fn drone_projection_revision(&self) -> Option<u64> { self.drone_projection_revision }

    pub(crate) async fn load_drone(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, ship: i64, drone_item: i64,
        ship_projections: Vec<super::effects::projection::Projection>, drone_projections: Vec<super::effects::projection::Projection>) -> Result<Fit, CallError> {
        Self::load_drone_with_runtime(db, sde, char_id, ship, drone_item, ship_projections, drone_projections, Vec::new()).await
    }

    async fn load_drone_with_runtime(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, ship: i64, drone_item: i64,
        ship_projections: Vec<super::effects::projection::Projection>, drone_projections: Vec<super::effects::projection::Projection>,
        runtime_sources: Vec<RuntimeModifierSource>) -> Result<Fit, CallError> {
        use crate::dao::items::loading::{load_items, ItemQuery};
        let item = load_items(db, ItemQuery::Id(drone_item)).await.map_err(db_err)?
            .into_iter().find(|item| item.owner_id == char_id && sde.types.get(item.type_id).and_then(|d| d.category_id) == Some(CATEGORY_DRONE))
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("owned drone not found")))?;
        let mut fit = Self::load_projected_with_runtime(db, sde, char_id, ship, ship_projections, runtime_sources).await?;
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
}

fn chance_enabled(selected: &HashMap<i64, Vec<i32>>, source: Key, data: &TypeData, effect: &crate::sde::dogma::Effect) -> bool {
    if !effect.fitting_usage_chance_attribute_id.is_some_and(|id| data.attribute(id).is_some()) { return true; }
    match source {
        Key::Booster(id) => selected.get(&id).is_some_and(|ids| ids.binary_search(&effect.effect_id).is_ok()),
        Key::Skill(_) | Key::Implant(_) => false,
        _ => true,
    }
}

