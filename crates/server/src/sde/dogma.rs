//! Immutable dogma attributes, effects and ordered modifiers loaded from typed `sde` at startup.
//! The dataset has no direct client mirror; fitting consumers include
//! `dogma/effects/modifiers.py:8-140` and `dogma/const.py:7-51`.
//! Python-only modifier rows are supplemented at load time; runtime calculations are elsewhere.
use std::collections::HashMap;
use crate::{dao::sde::dogma as dao, db::Db};
pub(crate) mod python_effects;

pub(crate) struct Attribute {
    pub(crate) attribute_id: i32,
    pub(crate) default_value: Option<f64>, pub(crate) stackable: Option<bool>
}
pub(crate) struct Modifier { pub(crate) func: String, pub(crate) domain: String, pub(crate) op: i32, pub(crate) modified: i32, pub(crate) modifying: i32, pub(crate) group: Option<i32>, pub(crate) skill: Option<i32>, pub(crate) target_category: Option<i32> }
pub(crate) struct Effect {
    pub(crate) effect_id: i32,
    pub(crate) name: Option<String>,
    pub(crate) guid: Option<String>,
    pub(crate) effect_category: Option<i32>,
    pub(crate) duration_attribute_id: Option<i32>,
    pub(crate) fitting_usage_chance_attribute_id: Option<i32>,
    pub(crate) is_offensive: bool, pub(crate) is_assistance: bool,
    pub(crate) disallow_auto_repeat: bool, pub(crate) electronic_chance: bool, pub(crate) propulsion_chance: bool,
    pub(crate) discharge_attribute_id: Option<i32>, pub(crate) range_attribute_id: Option<i32>,
    pub(crate) falloff_attribute_id: Option<i32>, pub(crate) tracking_attribute_id: Option<i32>,
    modifiers: Vec<Modifier>
}
impl Effect {
    pub(crate) fn modifiers(&self) -> &[Modifier] { &self.modifiers }
}
#[cfg_attr(test, derive(Default))]
pub(crate) struct Dogma { attributes: Vec<Attribute>, effects: Vec<Effect>, attribute_ids: HashMap<i32, usize>, effect_ids: HashMap<i32, usize>, attribute_names: HashMap<String, Vec<usize>>, effect_names: HashMap<String, Vec<usize>> }
impl Dogma {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let (attributes, effects, modifiers) = tokio::try_join!(dao::attributes(db), dao::effects(db), dao::modifiers(db))?;
        let mut result = Self { attributes: Vec::with_capacity(attributes.len()), effects: Vec::with_capacity(effects.len()), attribute_ids: HashMap::new(), effect_ids: HashMap::new(), attribute_names: HashMap::new(), effect_names: HashMap::new() };
        for r in attributes {
            let pos = result.attributes.len(); result.attribute_ids.insert(r.attribute_id, pos);
            if let Some(name) = &r.name { result.attribute_names.entry(name.clone()).or_default().push(pos); }
            result.attributes.push(Attribute { attribute_id: r.attribute_id, default_value: r.default_value, stackable: r.stackable });
        }
        for r in effects {
            let pos = result.effects.len(); result.effect_ids.insert(r.effect_id, pos);
            if let Some(name) = &r.name { result.effect_names.entry(name.clone()).or_default().push(pos); }
            result.effects.push(Effect { effect_id: r.effect_id, name: r.name, guid: r.guid, effect_category: r.effect_category, duration_attribute_id: r.duration_attribute_id, fitting_usage_chance_attribute_id: r.fitting_usage_chance_attribute_id, is_offensive: r.is_offensive, is_assistance: r.is_assistance, disallow_auto_repeat: r.disallow_auto_repeat, electronic_chance: r.electronic_chance, propulsion_chance: r.propulsion_chance, discharge_attribute_id: r.discharge_attribute_id, range_attribute_id: r.range_attribute_id, falloff_attribute_id: r.falloff_attribute_id, tracking_attribute_id: r.tracking_attribute_id, modifiers: Vec::new() });
        }
        for r in modifiers {
            if let Some(&i) = result.effect_ids.get(&r.effect_id) {
                result.effects[i].modifiers.push(Modifier { func: r.func, domain: r.domain, op: r.op, modified: r.modified, modifying: r.modifying, group: r.group, skill: r.skill, target_category: None });
            }
        }
        for effect in &mut result.effects {
            python_effects::supplement(effect);
        }
        Ok(result)
    }
    pub(crate) fn attribute(&self, id: i32) -> Option<&Attribute> { self.attribute_ids.get(&id).map(|&i| &self.attributes[i]) }
    pub(crate) fn effect(&self, id: i32) -> Option<&Effect> { self.effect_ids.get(&id).map(|&i| &self.effects[i]) }
    pub(super) fn indexed_modifiers(&self, id: i32) -> impl Iterator<Item = (i32, usize, usize)> + '_ {
        self.effect_ids.get(&id).into_iter().flat_map(|&effect| {
            self.effects[effect].modifiers.iter().enumerate().map(move |(modifier, m)| (m.modified, effect, modifier))
        })
    }
    pub(crate) fn indexed_modifier(&self, effect: usize, modifier: usize) -> (&Effect, &Modifier) {
        let effect = &self.effects[effect];
        (effect, &effect.modifiers[modifier])
    }
    pub(crate) fn attributes_by_name<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a Attribute> { self.attribute_names.get(name).into_iter().flatten().map(|&i| &self.attributes[i]) }
    pub(crate) fn effects_by_name<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a Effect> { self.effect_names.get(name).into_iter().flatten().map(|&i| &self.effects[i]) }
    pub(crate) fn attribute_count(&self) -> usize { self.attributes.len() }
    pub(crate) fn effect_count(&self) -> usize { self.effects.len() }
}

#[cfg(test)] pub(crate) mod tests;
