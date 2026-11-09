//! Cold fitting field patches and effective-attribute comparisons. These use the
//! same SDE and dogma evaluator as `fit`, mirroring dogma modifier evaluation;
//! database writes, runtime condition and notification delivery are not handled.

use super::{Fit, Key, Node};
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_CAPACITY, ATTR_CHARGE, ATTR_HEAT_HI,
    ATTR_HEAT_LOW, ATTR_HEAT_MED, ATTR_HULL_DAMAGE, ATTR_IS_ONLINE, ATTR_MASS,
    ATTR_MAX_VELOCITY, ATTR_QUANTITY, ATTR_RADIUS, ATTR_SHIELD_CHARGE, ATTR_VOLUME};
use std::collections::BTreeSet;

/// An owned database-field delta and the keys needed to evaluate its effects.
pub(crate) struct CommittedFitting {
    changes: Vec<(Key, Change)>,
    candidates: Vec<(Key, i32)>,
}

enum Change {
    Remove,
    Insert(Node),
    Update { online: Option<bool>, overrides: Vec<(i32, Option<f64>)> },
}

fn generic(attr: i32) -> bool {
    !matches!(attr, ATTR_IS_ONLINE | ATTR_QUANTITY | ATTR_SHIELD_CHARGE | ATTR_CHARGE
        | ATTR_ARMOR_DAMAGE | ATTR_HULL_DAMAGE | ATTR_HEAT_LOW | ATTR_HEAT_MED | ATTR_HEAT_HI)
}

impl Fit {
    /// Enumerate deterministic surviving receiver/attribute pairs from both fits.
    pub(crate) fn fitting_candidates(&self, after: &Fit) -> Vec<(Key, i32)> {
        let mut targets = BTreeSet::new();
        targets.insert(ATTR_MAX_VELOCITY);
        for fit in [Some(self), (!std::ptr::eq(self, after)).then_some(after)].into_iter().flatten() {
            targets.extend(fit.passive_by_attribute.keys().copied());
            for node in fit.nodes.values() {
                for type_id in [Some(node.type_id), node.charge_type].into_iter().flatten() {
                    for entry in fit.sde.types.effects(type_id) {
                        if let Some(effect) = fit.sde.dogma.effect(entry.effect_id) {
                            targets.extend(effect.modifiers().iter().map(|modifier| modifier.modified));
                        }
                    }
                }
                if let Some(effect) = node.projected_effect.and_then(|id| fit.sde.dogma.effect(id)) {
                    targets.extend(effect.modifiers().iter().map(|modifier| modifier.modified));
                }
            }
        }
        let mut candidates = BTreeSet::new();
        for (&key, final_node) in &after.nodes {
            if !matches!(key, Key::Ship | Key::Module(_)) { continue; }
            candidates.extend(targets.iter().copied().filter(|attr| generic(*attr)).map(|attr| (key, attr)));
            for (fit, node) in [(self, self.nodes.get(&key)), (after, (!std::ptr::eq(self, after)).then_some(final_node))] {
                let Some(node) = node else { continue };
                candidates.extend(fit.sde.types.attributes(node.type_id).map(|entry| entry.0)
                    .chain(node.overrides.iter().map(|entry| entry.0))
                    .filter(|attr| generic(*attr)).map(|attr| (key, attr)));
                if let Some(data) = fit.sde.types.get(node.type_id) {
                    for attr in [ATTR_MASS, ATTR_CAPACITY, ATTR_VOLUME, ATTR_RADIUS] {
                        if data.fit_attribute(attr).is_some() { candidates.insert((key, attr)); }
                    }
                }
            }
        }
        candidates.into_iter().collect()
    }

    /// Snapshot effective values in candidate order, retaining the evaluator memo.
    pub(crate) fn fitting_values(&self, candidates: &[(Key, i32)]) -> Vec<(Key, i32, f64)> {
        candidates.iter().map(|&(key, attr)| (key, attr, self.value(key, attr))).collect()
    }

    /// Compare an earlier snapshot with this fit, omitting unchanged/non-finite values.
    pub(crate) fn fitting_changes(&self, before: &[(Key, i32, f64)]) -> Vec<(Key, i32, f64, f64)> {
        before.iter().filter_map(|&(key, attr, old)| {
            if !self.nodes.contains_key(&key) { return None; }
            let new = self.value(key, attr);
            (old.is_finite() && new.is_finite() && old != new).then_some((key, attr, old, new))
        }).collect()
    }

    /// Consume the final fit, moving only committed node additions and field deltas.
    pub(crate) fn into_committed_fitting(mut self, before: &Fit) -> CommittedFitting {
        let candidates = before.fitting_candidates(&self);
        let mut keys = BTreeSet::new();
        keys.extend(before.nodes.keys().chain(self.nodes.keys()).copied()
            .filter(|key| matches!(key, Key::Ship | Key::Module(_) | Key::Charge(_))));
        let mut changes = Vec::new();
        for key in keys {
            match (before.nodes.get(&key), self.nodes.remove(&key)) {
                (Some(_), None) => changes.push((key, Change::Remove)),
                (None, Some(mut node)) => {
                    node.active = false;
                    node.overloaded = false;
                    changes.push((key, Change::Insert(node)));
                }
                (Some(old), Some(node)) if old.type_id != node.type_id => {
                    let mut node = node;
                    node.active = false;
                    node.overloaded = false;
                    changes.push((key, Change::Insert(node)));
                }
                (Some(old), Some(node)) => {
                    let online = (old.online != node.online || (matches!(key, Key::Module(_)) && !node.online))
                        .then_some(node.online);
                    let mut ids = BTreeSet::new();
                    ids.extend(old.overrides.iter().chain(&node.overrides).map(|entry| entry.0));
                    let overrides = ids.into_iter().filter_map(|id| {
                        let prior = old.overrides.binary_search_by_key(&id, |entry| entry.0).ok().map(|i| old.overrides[i].1);
                        let final_value = node.overrides.binary_search_by_key(&id, |entry| entry.0).ok().map(|i| node.overrides[i].1);
                        (prior != final_value).then_some((id, final_value))
                    }).collect::<Vec<_>>();
                    if online.is_some() || !overrides.is_empty() {
                        changes.push((key, Change::Update { online, overrides }));
                    }
                }
                (None, None) => unreachable!(),
            }
        }
        CommittedFitting { changes, candidates }
    }

    /// Apply only fields changed by the committed operation to the current fit.
    pub(crate) fn apply_committed_fitting(&mut self, change: CommittedFitting) {
        for (key, change) in change.changes {
            match change {
                Change::Remove => { self.nodes.remove(&key); }
                Change::Insert(node) => { self.nodes.insert(key, node); }
                Change::Update { online, overrides } => {
                    if let Some(node) = self.nodes.get_mut(&key) {
                        if let Some(online) = online {
                            node.online = online;
                            if !online { node.active = false; node.overloaded = false; }
                        }
                        for (id, value) in overrides {
                            match (node.overrides.binary_search_by_key(&id, |entry| entry.0), value) {
                                (Ok(i), Some(value)) => node.overrides[i].1 = value,
                                (Ok(i), None) => { node.overrides.remove(i); }
                                (Err(i), Some(value)) => node.overrides.insert(i, (id, value)),
                                (Err(_), None) => {}
                            }
                        }
                    }
                }
            }
        }
        self.memo.get_mut().unwrap().clear();
    }
}

impl CommittedFitting {
    pub(crate) fn candidates(&self) -> &[(Key, i32)] { &self.candidates }
}

#[cfg(test)] pub(crate) mod tests;
