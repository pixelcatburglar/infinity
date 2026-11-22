//! Effective-value evaluation for a loaded fit: modifier selection and domain
//! checks, guarded memoized attribute reads, client stacking penalties and
//! dogma operation order, runtime modifier source operands folded into the
//! same application order, the running-propulsion speed multiplier, and the
//! resonance floor applied only at the public value boundary. Mirrors
//! `dogma/const.py`, `dogma/effects/modifiers.py`,
//! `dogma/attributes/stackingNurfedAttribute.py` and
//! `dogma/attributes/skillLevelAttribute.py`. Modifier rows, attribute
//! defaults and type data come from the SDE the parent fit holds; operand
//! reads and the memo keep raw guarded results, and absent nodes or
//! attributes stay zero. Projection lifetime and overload activation are not
//! managed here; callers provide the fit snapshot.

use super::*;
use infinity_core::consts::dogma::{
    ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE,
    ATTR_MAX_VELOCITY, ATTR_REQUIRED_SKILLS, ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, ATTR_SPEED_FACTOR,
    ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE,
    EFFECT_AFTERBURNER, EFFECT_MICROWARPDRIVE, EFFECT_ONLINE,
};
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_IMPLANT, CATEGORY_SKILL, CATEGORY_SUBSYSTEM};

/// SDE dogma effect category: active.
const EFFECT_CATEGORY_ACTIVE: i32 = 1;
/// SDE dogma effect category: online.
const EFFECT_CATEGORY_ONLINE: i32 = 4;
/// SDE dgmAttributeTypes: speedBoostFactor.
const ATTR_SPEED_BOOST_FACTOR: i32 = 567;
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

impl Fit {
    /// Does modifier `m` from `source` apply to `target`, which is the node
    /// stored under it or a virtual node evaluated as if it were?
    fn applies_to(&self, source: Key, m: &Modifier, target: Key, t: &Node) -> bool {
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

    /// Effective `attr` of a charge of `type_id` as if it were loaded in
    /// `module`, whether or not one is loaded now: the same sources `value`
    /// reads, restricted to `scope`. Not memoised; a charge carries no
    /// overrides, so the virtual node equals a loaded one. `module` 0 names no
    /// fitted module.
    pub(crate) fn charge_value(&self, module: i64, type_id: i32, attr: i32, scope: SourceScope) -> f64 {
        let node = Node { type_id, data_position: self.sde.types.position(type_id), location: Location::Ship,
            online: true, active: false, overloaded: false, projected_effect: None, projection_resistance_attribute: None,
            receiver: None, charge_type: None, level: None, overrides: Vec::new(), cargo_unlimited: false, charge_values: Vec::new() };
        let key = Key::Charge(module);
        let base = base_attribute(node.data_position.map(|i| self.sde.types.at_position(i)), self.sde.dogma.attribute(attr), attr);
        let mut visiting = HashSet::from([(key, attr)]);
        let mut mods = Vec::new();
        self.collect_modifiers(key, &node, attr, &mut visiting, scope, &mut mods);
        apply(base, &mods)
    }

    fn value_guarded(&self, key: Key, attr: i32, visiting: &mut HashSet<(Key, i32)>) -> f64 {
        // The adaptive hardener's already-effective operand overlay wins
        // before the memo; it is never inserted into the memo itself.
        if let Some(operand) = self.reactive_armor_operand(key, attr) { return operand; }
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
        if let Some(node) = self.nodes.get(&key) {
            self.collect_modifiers(key, node, attr, visiting, SourceScope::All, &mut mods);
        }
        if key == Key::Ship {
            mods.extend(self.propulsion(attr, visiting));
        }
        visiting.remove(&(key, attr));
        let value = apply(base, &mods);
        crate::poison::lock_or_exit(self.memo.lock()).insert((key, attr), value);
        value
    }

    /// Collect every modifier of `attr` that `scope` admits onto `target`, which
    /// is the node stored under `key` or a virtual node evaluated as if it were.
    /// Passive skill and runtime source operands belong to `All` only.
    fn collect_modifiers(&self, key: Key, target: &Node, attr: i32,
        visiting: &mut HashSet<(Key, i32)>, scope: SourceScope, mods: &mut Vec<(i32, f64, bool)>) {
        if scope == SourceScope::All {
            self.visit_passive_modifiers(self.passive_by_attribute.get(&attr).into_iter().flatten().copied(), |source, m| {
                if self.applies_to(source, m, key, target) {
                    let v = self.value_guarded(source, m.modifying, visiting);
                    mods.push((m.op, v, false));
                }
            });
        }
        if self.sde.types.modifies_attribute(attr) || self.nodes.values().any(|node|
            node.projected_effect.and_then(|id| self.sde.dogma.effect(id))
                .is_some_and(|effect| effect.modifiers().iter().any(|m| m.modified == attr))) {
            let stackable = self.sde.dogma.attribute(attr).and_then(|m| m.stackable).unwrap_or(true);
            let mut sources: Vec<_> = self.nodes.keys().copied().collect();
            sources.sort_unstable();
            for source in sources {
                let node = &self.nodes[&source];
                let category = node.data_position.and_then(|i| self.sde.types.at_position(i).category_id);
                if !scope_admits(scope, source, category) { continue; }
                if matches!(source, Key::Skill(_)) { continue; }
                let Some(data) = node.data_position.map(|i| self.sde.types.at_position(i)) else { continue };
                if let Some(effect_id) = node.projected_effect {
                    if let Some(effect) = self.sde.dogma.effect(effect_id) {
                        for m in effect.modifiers().iter().filter(|m| m.modified == attr) {
                            if !effect_running(source, node, effect_id, effect.effect_category.unwrap_or(0), &m.domain)
                                || !self.applies_to(source, m, key, target) { continue; }
                            let value = projected_value(self, key, node, Some(data), self.sde.dogma.attribute(m.modifying), m.modifying, m.op, visiting);
                            mods.push((m.op, value, !stackable && !stacking_exempt(data.category_id.unwrap_or(0))));
                        }
                    }
                } else {
                    for (effect, m) in data.modifiers(&self.sde.dogma, attr) {
                        if !chance_enabled(&self.selected_effects, source, data, effect) || !effect_running(source, node, effect.effect_id, effect.effect_category.unwrap_or(0), &m.domain) { continue; }
                        if !self.applies_to(source, m, key, target) { continue; }
                        let value = self.value_guarded(source, m.modifying, visiting);
                        mods.push((m.op, value, !stackable && !stacking_exempt(data.category_id.unwrap_or(0))));
                    }
                }
                if let Some(charge) = node.charge_type.and_then(|t| self.sde.types.get(t)) {
                    for (_, m) in charge.modifiers(&self.sde.dogma, attr) {
                        if m.domain != "targetID" || !self.applies_to(source, m, key, target) { continue; }
                        let value = node.charge_base(Some(charge), self.sde.dogma.attribute(m.modifying), m.modifying);
                        mods.push((m.op, value, false));
                    }
                }
            }
        }
        if scope == SourceScope::All {
            self.runtime_modifiers(key, target, attr, mods);
        }
    }

    /// Runtime source operands on `target`: each `(receiver, collection)`
    /// group folds its sources with the collection's SDE aggregate into one
    /// operand; every selector row of that collection which modifies `attr`
    /// then passes through the one `applies_to` selector, anchored at the
    /// receiver's own node (the compiled rows use the `itemID` domain, so
    /// ItemModifier hits the anchor itself and the Location* rows hit items
    /// located on it). A runtime source is not an exempt-category item, so
    /// its operand is penalized exactly when the attribute is non-stackable.
    fn runtime_modifiers(&self, target: Key, target_node: &Node, attr: i32, mods: &mut Vec<(i32, f64, bool)>) {
        let mut sources = self.runtime_sources.as_slice();
        if sources.is_empty() { return; }
        let mut stackable = None;
        while let Some(first) = sources.first() {
            let run = sources.iter().take_while(|s| s.receiver == first.receiver && s.collection_id == first.collection_id).count();
            let (group, rest) = sources.split_at(run);
            sources = rest;
            let Some(collection) = self.sde.dbuffs.get(first.collection_id) else { continue };
            if !collection.modifiers.iter().any(|m| m.modifier.modified == attr) { continue };
            let Some(anchor) = self.runtime_anchor(first.receiver) else { continue };
            let Some(value) = super::runtime::fold_runtime_values(collection.aggregate, group.iter().map(|s| s.value)) else { continue };
            for m in collection.modifiers.iter().filter(|m| m.modifier.modified == attr && self.applies_to(anchor, &m.modifier, target, target_node)) {
                let penalized = *stackable.get_or_insert_with(|| !self.sde.dogma.attribute(attr).and_then(|a| a.stackable).unwrap_or(true));
                mods.push((m.modifier.op, value, penalized));
            }
        }
    }
}

fn stacking_exempt(category: i32) -> bool {
    matches!(category, CATEGORY_SHIP | CATEGORY_CHARGE | CATEGORY_SKILL | CATEGORY_IMPLANT | CATEGORY_SUBSYSTEM)
}

fn projected_value(fit: &Fit, target: Key, node: &Node, data: Option<&TypeData>, meta: Option<&Attribute>, attr: i32, op: i32, visiting: &mut HashSet<(Key, i32)>) -> f64 {
    let value = node.base(data, meta, attr);
    node.projection_resistance_attribute.map_or(value, |resistance| crate::services::effects::projected::scaled(value, op, fit.value_guarded(target, resistance, visiting)))
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
