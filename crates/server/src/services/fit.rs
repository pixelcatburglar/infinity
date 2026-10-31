//! Effective dogma attributes for characters, ships, fittings, skills, active
//! brain items, drones and explicitly supplied incoming projections. Modifier
//! rows and defaults come from the SDE; owned items and overrides come from the
//! game database. Mirrors `dogma/effects/modifiers.py`, `dogma/const.py`, and
//! `dogma/attributes/skillLevelAttribute.py`. Projection lifetime and overload
//! activation are not managed here; callers provide a fit snapshot.
//! Skill effects with fittingUsageChance require loading and filtering
//! that attribute if such a skill appears in the SDE.
//! Owner-fit drone queries do not include incoming drone projections;
//! add a receiving drone fit when the in-space drone runtime exists.

use super::{db_err, CallError};
use crate::consts::{ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_REQUIRED_SKILLS, ATTR_SPEED_FACTOR, CATEGORY_CHARGE, CATEGORY_SHIP, CATEGORY_SKILL, CATEGORY_SUBSYSTEM, EFFECT_AFTERBURNER, EFFECT_MICROWARPDRIVE, EFFECT_ONLINE, EFFECT_SKILL, TYPE_CHARACTER};
use crate::dao::items::fit as fit_dao;
use crate::db::Db;
use crate::sde::{dogma::{Attribute, Modifier}, types::TypeData, Sde};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::consts::{ATTR_SKILL_LEVEL, ATTR_SKILL_POINTS, ATTR_SKILL_TIME_CONSTANT, CATEGORY_DRONE, FLAG_IMPLANT};
/// SDE invCategories: Implant, exempt from stacking penalties.
const CATEGORY_IMPLANT: i32 = 20;
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

struct Node {
    type_id: i32,
    // Positions remain stable in the immutable types owned by the retained Arc<Sde>.
    data_position: Option<usize>,
    location: Location,
    online: bool,
    active: bool,
    overloaded: bool,
    projected_effect: Option<i32>,
    charge_type: Option<i32>,
    level: Option<i32>,
    overrides: Vec<(i32, f64)>,
    charge_values: Vec<(i32, f64)>,
}

pub(crate) fn base_attribute(data: Option<&TypeData>, meta: Option<&Attribute>, attr: i32) -> f64 {
    data.and_then(|t| t.fit_attribute(attr))
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

/// A character's active ship with everything that modifies it.
pub struct Fit {
    nodes: HashMap<Key, Node>,
    passive_modifiers: Vec<(Key, i32, usize)>,
    passive_by_attribute: HashMap<i32, Vec<usize>>,
    sde: Arc<Sde>,
    memo: Mutex<HashMap<(Key, i32), f64>>,
    projection_revision: Option<u64>,
}

impl Fit {
    /// Load a fit outside space, without incoming projections.
    pub async fn load(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, ship: i64) -> Result<Fit, CallError> {
        Self::load_projected(db, sde, char_id, ship, Vec::new()).await
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
            online: true, active: false, overloaded: false, projected_effect: None,
            charge_type: None, level: None, overrides: Vec::new(), charge_values: Vec::new(),
        });
        Self::insert_projections(&sde, &mut nodes, projections);
        Fit { nodes, passive_modifiers: Vec::new(), passive_by_attribute: HashMap::new(), sde, memo: Mutex::new(HashMap::new()), projection_revision: None }
    }

    fn insert_projections(sde: &Sde, nodes: &mut HashMap<Key, Node>, projections: Vec<super::effects::projection::Projection>) {
        for p in projections {
            let mut values = p.values;
            values.sort_by_key(|v| v.0);
            let mut charge_values = p.charge_values;
            charge_values.sort_by_key(|v| v.0);
            nodes.insert(Key::Projected(p.source_module), Node {
                type_id: p.source_type, data_position: sde.types.position(p.source_type),
                location: Location::Space, online: true, active: true, overloaded: false,
                projected_effect: Some(p.effect_id), charge_type: p.charge_type, level: None, overrides: values, charge_values,
            });
        }
    }

    /// Load the fit and explicit source-effective incoming projected effects.
    pub(crate) async fn load_projected(db: &(impl crate::db::executor::Executor + ?Sized), sde: Arc<Sde>, char_id: i64, ship: i64, projections: Vec<super::effects::projection::Projection>) -> Result<Fit, CallError> {
        let ship_type = crate::dao::items::inventory::type_id(db, ship).await.map_err(db_err)?.unwrap_or(0);
        let skills = crate::dao::skills::training::current(db, char_id).await.map_err(db_err)?;
        let modules = fit_dao::modules(db, ship).await.map_err(db_err)?;
        let charges = fit_dao::charges(db, ship).await.map_err(db_err)?;
        let brain = fit_dao::brain_items(db, char_id).await.map_err(db_err)?;
        let drones = fit_dao::drones(db, char_id, ship).await.map_err(db_err)?;
        let ids: Vec<i64> = [ship, char_id].into_iter().chain(modules.iter().map(|m| m.item_id))
            .chain(brain.iter().map(|m| m.item_id)).chain(drones.iter().map(|m| m.item_id)).collect();
        let mut overrides: HashMap<i64, Vec<(i32, f64)>> = HashMap::new();
        for row in crate::dao::items::dogma::overrides_for(db, &ids).await.map_err(db_err)? {
            overrides.entry(row.item_id).or_default().push((row.attribute_id, row.value));
        }
        for values in overrides.values_mut() { values.sort_by_key(|entry| entry.0); }
        let mut character: HashMap<i32, f64> = super::skills::attributes::base_character_attributes(db, &sde, char_id).await?
            .into_iter().filter_map(|(attr, value)| i32::try_from(attr).ok().map(|attr| (attr, value))).collect();
        character.extend(overrides.remove(&char_id).unwrap_or_default());
        let mut character: Vec<_> = character.into_iter().collect();
        character.sort_by_key(|v| v.0);
        let active = super::combat::active_modules();
        let mut nodes = HashMap::new();
        let node = |type_id, location, online, active, level, overrides| Node {
            type_id, data_position: sde.types.position(type_id), location, online, active,
            overloaded: false, projected_effect: None, charge_type: None, level, overrides, charge_values: Vec::new(),
        };
        nodes.insert(Key::Char, node(TYPE_CHARACTER, Location::Char, true, false, None, character));
        nodes.insert(Key::Ship, node(ship_type, Location::Space, true, false, None, overrides.remove(&ship).unwrap_or_default()));
        for r in &skills {
            let rank = sde.types.get(r.type_id).and_then(|d| d.fit_attribute(ATTR_SKILL_TIME_CONSTANT)).unwrap_or(0.0);
            let level = super::skills::formulas::level_from_points(r.skill_points, rank);
            nodes.insert(Key::Skill(r.type_id), node(r.type_id, Location::Char, true, false, Some(level), vec![(ATTR_SKILL_POINTS, r.skill_points as f64)]));
        }
        let mut module_by_flag = HashMap::new();
        for r in &modules {
            module_by_flag.insert(r.flag_id, r.item_id);
            nodes.insert(Key::Module(r.item_id), node(r.type_id, Location::Ship, r.online, r.online && active.contains(&r.item_id), None, overrides.remove(&r.item_id).unwrap_or_default()));
        }
        for r in &charges {
            if let Some(m) = module_by_flag.get(&r.flag_id) {
                nodes.insert(Key::Charge(*m), node(r.type_id, Location::Ship, true, false, None, Vec::new()));
            }
        }
        for r in &brain {
            let key = if r.flag_id == FLAG_IMPLANT { Key::Implant(r.item_id) } else { Key::Booster(r.item_id) };
            nodes.insert(key, node(r.type_id, Location::Char, true, false, None, overrides.remove(&r.item_id).unwrap_or_default()));
        }
        for r in &drones {
            if sde.types.get(r.type_id).and_then(|d| d.category_id) == Some(CATEGORY_DRONE) {
                nodes.insert(Key::Drone(r.item_id), node(r.type_id, Location::Space, true, false, None, overrides.remove(&r.item_id).unwrap_or_default()));
            }
        }
        Self::insert_projections(&sde, &mut nodes, projections);
        let mut skill_effects = Vec::new();
        for r in &skills {
            if nodes.get(&Key::Skill(r.type_id)).and_then(|n| n.level).unwrap_or(0) <= 0 { continue; }
            if let Some(data) = sde.types.get(r.type_id) {
                for entry in data.effects() {
                    if entry.effect_id == EFFECT_SKILL { continue; }
                    if sde.dogma.effect(entry.effect_id).is_some_and(|effect| effect.effect_category.unwrap_or(0) == EFFECT_CATEGORY_PASSIVE) {
                        skill_effects.push((r.type_id, entry.effect_id));
                    }
                }
            }
        }
        skill_effects.sort_unstable();
        let mut passive_modifiers = Vec::new();
        let mut passive_by_attribute: HashMap<i32, Vec<usize>> = HashMap::new();
        for (id, effect_id) in skill_effects {
            let Some(effect) = sde.dogma.effect(effect_id) else { continue };
            for (position, modifier) in effect.modifiers().iter().enumerate() {
                passive_by_attribute.entry(modifier.modified).or_default().push(passive_modifiers.len());
                passive_modifiers.push((Key::Skill(id), effect_id, position));
            }
        }
        Ok(Fit { nodes, passive_modifiers, passive_by_attribute, sde, memo: Mutex::new(HashMap::new()), projection_revision: None })
    }

    /// Include a prospective fitted item with its persisted dogma overrides.
    pub(crate) async fn include_module(&mut self, db: &(impl crate::db::executor::Executor + ?Sized), item: i64, type_id: i32) -> Result<(), CallError> {
        if self.nodes.contains_key(&Key::Module(item)) { return Ok(()); }
        let mut overrides: Vec<_> = crate::dao::items::dogma::overrides_for(db, &[item]).await.map_err(db_err)?
            .into_iter().map(|r| (r.attribute_id, r.value)).collect();
        overrides.sort_by_key(|v| v.0);
        self.nodes.insert(Key::Module(item), Node {
            type_id, data_position: self.sde.types.position(type_id), location: Location::Ship,
            online: false, active: false, overloaded: false, projected_effect: None,
            charge_type: None, level: None, overrides, charge_values: Vec::new(),
        });
        self.memo.get_mut().unwrap().clear();
        Ok(())
    }

    /// Change a prospective module state and invalidate all dependent values.
    pub(crate) fn set_module_online(&mut self, item: i64, online: bool) -> bool {
        let Some(node) = self.nodes.get_mut(&Key::Module(item)) else { return false };
        if node.online != online {
            node.online = online;
            if !online { node.active = false; }
            self.memo.get_mut().unwrap().clear();
        }
        true
    }

    /// Load the selected character ship and one owned drone, including launched drones.
    pub(crate) async fn drone(db: &Db, sde: Arc<Sde>, char_id: i64, drone_item: i64) -> Result<Fit, CallError> {
        use crate::dao::items::loading::{load_items, ItemQuery};
        let item = load_items(db, ItemQuery::Id(drone_item)).await.map_err(db_err)?
            .into_iter().find(|item| item.owner_id == char_id && sde.types.get(item.type_id).and_then(|d| d.category_id) == Some(CATEGORY_DRONE))
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("owned drone not found")))?;
        let ship = crate::dao::characters::selection::ship_id(db, char_id).await.map_err(db_err)?
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("character has no selected ship")))?;
        let mut fit = Self::load(db, sde, char_id, ship).await?;
        let mut overrides: Vec<_> = crate::dao::items::dogma::overrides_for(db, &[drone_item]).await.map_err(db_err)?
            .into_iter().map(|r| (r.attribute_id, r.value)).collect();
        overrides.sort_by_key(|v| v.0);
        fit.nodes.insert(Key::Drone(drone_item), Node {
            type_id: item.type_id, data_position: fit.sde.types.position(item.type_id),
            location: Location::Space, online: true, active: false, overloaded: false,
            projected_effect: None, charge_type: None, level: None, overrides, charge_values: Vec::new(),
        });
        fit.memo.get_mut().unwrap().clear();
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
                        if effect.effect_category.unwrap_or(0) == EFFECT_CATEGORY_PASSIVE {
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
            .is_some_and(|position| attr == crate::consts::ATTR_CAPACITY || self.sde.types.at_position(position).attribute(attr).is_some())
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
            let category = self.nodes.get(&Key::Ship).and_then(|hull| hull.data_position)
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
        modifier_applies(source, m, target, t, group, required(m.skill))
    }

    /// The effective value of `attr` on `key`.
    pub fn value(&self, key: Key, attr: i32) -> f64 {
        self.value_guarded(key, attr, &mut HashSet::new())
    }

    fn value_guarded(&self, key: Key, attr: i32, visiting: &mut HashSet<(Key, i32)>) -> f64 {
        if let Some(value) = self.memo.lock().unwrap().get(&(key, attr)).copied() {
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
                        if !effect_running(source, node, effect.effect_id, effect.effect_category.unwrap_or(0), &m.domain) { continue; }
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
        self.memo.lock().unwrap().insert((key, attr), value);
        value
    }
}

fn stacking_exempt(category: i32) -> bool {
    matches!(category, CATEGORY_SHIP | CATEGORY_CHARGE | CATEGORY_SKILL | CATEGORY_IMPLANT | CATEGORY_SUBSYSTEM)
}

fn projected_value(node: &Node, data: Option<&TypeData>, meta: Option<&Attribute>, attr: i32) -> f64 {
    node.base(data, meta, attr)
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

fn modifier_applies(source: Key, m: &Modifier, target: Key, node: &Node, group: Option<i32>, required: bool) -> bool {
    let domain = match m.domain.as_str() {
        "itemID" => Some(source),
        "shipID" => Some(Key::Ship),
        "charID" => Some(Key::Char),
        "targetID" if matches!(source, Key::Projected(_)) => Some(Key::Ship),
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

#[cfg(test)] mod tests;
