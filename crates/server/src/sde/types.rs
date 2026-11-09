//! Immutable inventory type, group and category data loaded from typed `sde` tables at startup.
//! The dataset has no direct client mirror; an attribute consumer is
//! `eve/common/script/dogma/baseDogmaLocation.py:1960-1967`.
//! Presentation fields and other inventory tables are outside this dataset.
use std::collections::{HashMap, HashSet};
use infinity_core::{consts::{dogma::ATTR_CAPACITY, dogma::ATTR_MASS, dogma::ATTR_RADIUS, dogma::ATTR_VOLUME, dogma::EFFECT_SKILL}};
use crate::{dao::sde::types as dao, db::Db};

pub(crate) struct TypeData {
    pub(crate) type_id: i32,
    pub(crate) group_id: Option<i32>, pub(crate) category_id: Option<i32>,
    pub(crate) name: Option<String>,
    pub(crate) lower_name: Option<String>,
    pub(crate) mass: Option<f64>, pub(crate) capacity: Option<f64>,
    pub(crate) volume: Option<f64>, pub(crate) radius: Option<f64>,
    pub(crate) portion_size: Option<i32>,
    pub(crate) base_price: Option<f64>,
    pub(crate) published: Option<bool>,
    pub(crate) market_group_id: Option<i32>,
    pub(crate) race_id: Option<i32>,
    pub(crate) icon_id: Option<i32>,
    attributes: Vec<(i32, f64)>, effects: Vec<TypeEffect>,
    modifiers: HashMap<i32, Vec<(usize, usize)>>,
    materials: Vec<TypeMaterial>,
}
pub(crate) struct TypeEffect {
    pub(crate) effect_id: i32,
    pub(crate) is_default: bool,
}
pub(crate) struct TypeMaterial { pub(crate) material_type_id: i32, pub(crate) quantity: Option<i64> }
pub(crate) struct Group {
    pub(crate) category_id: Option<i32>,
    pub(crate) name: Option<String>,
    pub(crate) lower_name: Option<String>,
}
pub(crate) struct Category { pub(crate) name: Option<String> }

impl TypeData {
    pub(crate) fn attribute(&self, id: i32) -> Option<f64> { self.attributes.binary_search_by_key(&id, |a| a.0).ok().map(|i| self.attributes[i].1) }
    pub(crate) fn fit_attribute(&self, id: i32) -> Option<f64> {
        self.attribute(id).or_else(|| match id { ATTR_MASS => self.mass, ATTR_CAPACITY => self.capacity, ATTR_VOLUME => self.volume, ATTR_RADIUS => self.radius, _ => None })
    }
    pub(crate) fn effects(&self) -> &[TypeEffect] { &self.effects }
    pub(crate) fn modifiers<'a>(&'a self, dogma: &'a super::dogma::Dogma, attr: i32) -> impl Iterator<Item = (&'a super::dogma::Effect, &'a super::dogma::Modifier)> + 'a {
        self.modifiers.get(&attr).into_iter().flatten().map(move |&(effect, modifier)| dogma.indexed_modifier(effect, modifier))
    }
    pub(crate) fn materials(&self) -> &[TypeMaterial] { &self.materials }
}
#[derive(Default)]
struct Children { attributes: Vec<(i32, f64)>, effects: Vec<TypeEffect>, materials: Vec<TypeMaterial> }
#[cfg_attr(test, derive(Default))]
pub(crate) struct Types {
    types: Vec<TypeData>, by_id: HashMap<i32, usize>, groups: HashMap<i32, Group>,
    categories: HashMap<i32, Category>,
    group_types: HashMap<i32, Vec<i32>>, orphans: HashMap<i32, Children>,
    modified_attributes: HashSet<i32>,
}
impl Types {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let (types, groups, categories, attrs, effects, materials) = tokio::try_join!(dao::types(db), dao::groups(db), dao::categories(db), dao::attributes(db), dao::effects(db), dao::materials(db))?;
        let mut children: HashMap<i32, Children> = HashMap::new();
        for r in attrs { children.entry(r.type_id).or_default().attributes.push((r.attribute_id, r.value)); }
        for r in effects { children.entry(r.type_id).or_default().effects.push(TypeEffect { effect_id: r.effect_id, is_default: r.is_default }); }
        for r in materials { children.entry(r.type_id).or_default().materials.push(TypeMaterial { material_type_id: r.material_type_id, quantity: r.quantity }); }
        let mut result = Self { types: Vec::with_capacity(types.len()), by_id: HashMap::with_capacity(types.len()), groups: groups.into_iter().map(|r| (r.group_id, Group { category_id: r.category_id, name: r.name, lower_name: r.lower_name })).collect(), categories: categories.into_iter().map(|r| (r.category_id, Category { name: r.name })).collect(), group_types: HashMap::new(), orphans: HashMap::new(), modified_attributes: HashSet::new() };
        for r in types {
            result.by_id.insert(r.type_id, result.types.len());
            if let Some(id) = r.group_id { result.group_types.entry(id).or_default().push(r.type_id); }
            let c = children.remove(&r.type_id).unwrap_or_default();
            result.types.push(TypeData { type_id: r.type_id, group_id: r.group_id, category_id: r.category_id, name: r.name, lower_name: r.lower_name, mass: r.mass, capacity: r.capacity, volume: r.volume, radius: r.radius, portion_size: r.portion_size, base_price: r.base_price, published: r.published, market_group_id: r.market_group_id, race_id: r.race_id, icon_id: r.icon_id, attributes: c.attributes, effects: c.effects, modifiers: HashMap::new(), materials: c.materials });
        }
        result.orphans = children;
        Ok(result)
    }
    pub(super) fn index_modifiers(&mut self, dogma: &super::dogma::Dogma) {
        for data in &mut self.types {
            for effect in &data.effects {
                if effect.effect_id == EFFECT_SKILL { continue; }
                for (attr, effect, modifier) in dogma.indexed_modifiers(effect.effect_id) {
                    data.modifiers.entry(attr).or_default().push((effect, modifier));
                    self.modified_attributes.insert(attr);
                }
            }
        }
    }
    /// Whether any indexed type modifier can target this attribute.
    pub(crate) fn modifies_attribute(&self, attr: i32) -> bool { self.modified_attributes.contains(&attr) }
    pub(crate) fn get(&self, id: i32) -> Option<&TypeData> { self.by_id.get(&id).map(|&i| &self.types[i]) }
    pub(crate) fn position(&self, id: i32) -> Option<usize> { self.by_id.get(&id).copied() }
    pub(crate) fn at_position(&self, index: usize) -> &TypeData { &self.types[index] }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &TypeData> { self.types.iter() }
    pub(crate) fn len(&self) -> usize { self.types.len() }
    pub(crate) fn group(&self, id: i32) -> Option<&Group> { self.groups.get(&id) }
    pub(crate) fn category(&self, id: i32) -> Option<&Category> { self.categories.get(&id) }
    pub(crate) fn group_types(&self, id: i32) -> &[i32] { self.group_types.get(&id).map_or(&[], Vec::as_slice) }
    fn raw_attributes(&self, id: i32) -> &[(i32, f64)] { self.get(id).map(|t| t.attributes.as_slice()).or_else(|| self.orphans.get(&id).map(|c| c.attributes.as_slice())).unwrap_or(&[]) }
    pub(crate) fn attributes(&self, id: i32) -> impl Iterator<Item = (i32, f64)> + '_ { self.raw_attributes(id).iter().copied() }
    pub(crate) fn attribute(&self, id: i32, attribute: i32) -> Option<f64> { let attrs = self.raw_attributes(id); attrs.binary_search_by_key(&attribute, |a| a.0).ok().map(|i| attrs[i].1) }
    pub(crate) fn effects(&self, id: i32) -> &[TypeEffect] { self.get(id).map(|t| t.effects()).or_else(|| self.orphans.get(&id).map(|c| c.effects.as_slice())).unwrap_or(&[]) }
    pub(crate) fn materials(&self, id: i32) -> &[TypeMaterial] { self.get(id).map(|t| t.materials()).or_else(|| self.orphans.get(&id).map(|c| c.materials.as_slice())).unwrap_or(&[]) }
}

#[cfg(test)]
pub(crate) mod tests;
