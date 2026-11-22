//! Cold fitting field patches and effective-attribute comparisons. These use the
//! same SDE and dogma evaluator as `fit`, mirroring dogma modifier evaluation;
//! database writes, runtime condition and notification delivery are not handled.
//! Damage-only repair patches preserve every live module field except hull damage.
//! Patches cover ship, module, charge and drone nodes; a cold charge insertion
//! stamps a fresh incarnation so a final consumption deletes exactly that node.
//! Loaded charge quantities live in the fit's loaded seeds and never flow
//! through generic attribute overrides.
//! Committed override-only deltas admit set and remove writes on named keys
//! without touching topology, runtime flags or incarnations.
//! A terminal stance switch attaches its `StanceTransition` receipt to the
//! same patch; ordinary fittings always carry `None`.

use super::{Fit, Key, Node};
use crate::services::ship_stance::StanceTransition;
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_CAPACITY, ATTR_CHARGE, ATTR_HEAT_HI,
    ATTR_HEAT_LOW, ATTR_HEAT_MED, ATTR_HULL_DAMAGE, ATTR_IS_ONLINE, ATTR_MASS,
    ATTR_MAX_VELOCITY, ATTR_QUANTITY, ATTR_RADIUS, ATTR_SHIELD_CHARGE, ATTR_VOLUME};
use std::collections::{BTreeMap, BTreeSet};

/// An owned database-field delta and the keys needed to evaluate its effects.
pub(crate) struct CommittedFitting {
    changes: Vec<(Key, Change)>,
    candidates: Vec<(Key, i32)>,
    stance: Option<StanceTransition>,
}

enum Change {
    Remove,
    Insert(Node),
    Update { online: Option<bool>, overrides: Vec<(i32, Option<f64>)>, cargo_unlimited: Option<bool> },
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
        if self.cargo_unlimited() || after.cargo_unlimited() { targets.insert(ATTR_CAPACITY); }
        for fit in [Some(self), (!std::ptr::eq(self, after)).then_some(after)].into_iter().flatten() {
            targets.extend(fit.passive_by_attribute.keys().copied());
            for source in &fit.runtime_sources {
                if let Some(collection) = fit.sde.dbuffs.get(source.collection_id) {
                    targets.extend(collection.modifiers.iter().map(|m| m.modifier.modified));
                }
            }
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
            if !matches!(key, Key::Ship | Key::Module(_) | Key::Drone(_)) { continue; }
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
        candidates.iter().map(|&(key, attr)| (key, attr, self.wire_value(key, attr))).collect()
    }

    /// Compare an earlier snapshot with this fit, omitting unchanged/non-finite values.
    pub(crate) fn fitting_changes(&self, before: &[(Key, i32, f64)]) -> Vec<(Key, i32, f64, f64)> {
        before.iter().filter_map(|&(key, attr, old)| {
            if !self.nodes.contains_key(&key) { return None; }
            let new = self.wire_value(key, attr);
            (old.is_finite() && new.is_finite() && old != new).then_some((key, attr, old, new))
        }).collect()
    }

    /// Consume the final fit, moving only committed node additions and field deltas.
    pub(crate) fn into_committed_fitting(mut self, before: &Fit) -> CommittedFitting {
        let candidates = before.fitting_candidates(&self);
        let mut keys = BTreeSet::new();
        keys.extend(before.nodes.keys().chain(self.nodes.keys()).copied()
            .filter(|key| matches!(key, Key::Ship | Key::Module(_) | Key::Charge(_) | Key::Drone(_))));
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
                    let cargo_unlimited = (key == Key::Ship && old.cargo_unlimited != node.cargo_unlimited).then_some(node.cargo_unlimited);
                    if online.is_some() || !overrides.is_empty() || cargo_unlimited.is_some() {
                        changes.push((key, Change::Update { online, overrides, cargo_unlimited }));
                    }
                }
                (None, None) => unreachable!(),
            }
        }
        CommittedFitting { changes, candidates, stance: None }
    }

    /// Apply only fields changed by the committed operation to the current
    /// fit; true when a node was materially changed.
    pub(crate) fn apply_committed_fitting(&mut self, change: &CommittedFitting) -> bool {
        self.apply_fitting(change, |_| true)
    }

    /// Apply only the ship-domain portion of a committed patch. An owned drone's
    /// resident fit shares the owner's ship domain but keeps its own drone and
    /// projected nodes out of it.
    pub(crate) fn apply_ship_fitting(&mut self, change: &CommittedFitting) -> bool {
        self.apply_fitting(change, |key| matches!(key, Key::Ship | Key::Module(_) | Key::Charge(_)))
    }

    fn apply_fitting(&mut self, change: &CommittedFitting, keep: impl Fn(Key) -> bool) -> bool {
        let mut applied = false;
        for (key, change) in &change.changes {
            if !keep(*key) { continue; }
            match change {
                Change::Remove => {
                    if self.nodes.remove(key).is_some() { applied = true; }
                    if let Key::Charge(module) = key { self.charge_incarnations.remove(module); }
                    if let Key::Module(module) = key { self.clear_reactive_armor(*module); }
                }
                Change::Insert(node) => {
                    if let Key::Charge(module) = key {
                        self.charge_incarnations.insert(*module, crate::space::targeting::next_generation());
                    }
                    if let Key::Module(module) = key { self.clear_reactive_armor(*module); }
                    self.nodes.insert(*key, node.clone());
                    applied = true;
                }
                Change::Update { online, overrides, cargo_unlimited } => {
                    if let Some(node) = self.nodes.get_mut(key) {
                        if let Some(online) = online {
                            applied |= node.online != *online;
                            node.online = *online;
                            if !node.online {
                                applied |= node.active || node.overloaded;
                                node.active = false; node.overloaded = false;
                            }
                        }
                        for (id, value) in overrides {
                            match (node.overrides.binary_search_by_key(id, |entry| entry.0), value) {
                                (Ok(i), Some(value)) => {
                                    applied |= node.overrides[i].1 != *value;
                                    node.overrides[i].1 = *value;
                                }
                                (Ok(i), None) => { node.overrides.remove(i); applied = true; }
                                (Err(i), Some(value)) => { node.overrides.insert(i, (*id, *value)); applied = true; }
                                (Err(_), None) => {}
                            }
                        }
                        if let Some(mode) = cargo_unlimited {
                            applied |= node.cargo_unlimited != *mode;
                            node.cargo_unlimited = *mode;
                        }
                    }
                    // A committed offline drops the module's adapted operand
                    // profile like a removal; ordinary field deltas keep it.
                    if let (Key::Module(module), Some(false)) = (*key, *online) {
                        self.clear_reactive_armor(module);
                    }
                }
            }
        }
        if applied { crate::poison::lock_or_exit(self.memo.get_mut()).clear(); }
        applied
    }

    /// Incarnation token of the charge currently resident on one module.
    ///
    /// Each cold insertion receives a fresh process generation, so a same-type
    /// reload can never alias the token a cycle captured before committing its
    /// final consumption.
    pub(crate) fn charge_incarnation(&self, module: i64) -> Option<(u64, i32)> {
        let incarnation = self.charge_incarnations.get(&module).copied()?;
        self.nodes.get(&Key::Charge(module)).map(|node| (incarnation, node.type_id))
    }

    /// Remove the resident charge only when its incarnation and type still
    /// match the captured token; a positive remaining quantity never reaches
    /// this removal and is never stored here.
    pub(crate) fn remove_charge(&mut self, module: i64, incarnation: u64, type_id: i32) -> bool {
        let current = self.charge_incarnations.get(&module).copied();
        let matches = current == Some(incarnation)
            && self.nodes.get(&Key::Charge(module)).is_some_and(|node| node.type_id == type_id);
        if !matches { return false; }
        self.nodes.remove(&Key::Charge(module));
        self.charge_incarnations.remove(&module);
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        true
    }

    /// Drop one module's resident charge node when its type still matches,
    /// clearing its own incarnation with it. Propagates an owner-approved
    /// removal into a controlled drone's ship-domain copy, whose incarnation
    /// token is independent and never matched.
    pub(crate) fn remove_charge_node(&mut self, module: i64, type_id: i32) -> bool {
        if !self.nodes.get(&Key::Charge(module)).is_some_and(|node| node.type_id == type_id) { return false; }
        self.nodes.remove(&Key::Charge(module));
        self.charge_incarnations.remove(&module);
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        true
    }

    /// Loaded slots this cold fit seeds a combat record with: exact i64
    /// quantity plus the pending flag, keyed by module item id.
    pub(crate) fn loaded_seeds(&self) -> &[crate::space::combat::loaded::LoadedSeed] { &self.loaded_seeds }

    /// Publish one business-committed absolute seed into the cold-fit view so
    /// a later residency install reflects the last durable state.
    pub(crate) fn set_loaded_seed(&mut self, seed: crate::space::combat::loaded::LoadedSeed) {
        match self.loaded_seeds.iter_mut().find(|s| s.module == seed.module) {
            Some(existing) => *existing = seed,
            None => self.loaded_seeds.push(seed),
        }
    }
    /// Drop one module's loaded seed when the module itself is gone; a stale
    /// seed must not fabricate a slot on the next residency install.
    pub(crate) fn remove_loaded_seed(&mut self, module: i64) -> bool {
        let Some(index) = self.loaded_seeds.iter().position(|s| s.module == module) else { return false };
        self.loaded_seeds.remove(index);
        true
    }
}

impl CommittedFitting {
    /// Build a damage-only patch for every fitted module ID, including zero damage.
    pub(crate) fn repaired_modules(modules: impl Iterator<Item = i64>) -> Self {
        let changes = modules.map(|id| (
            Key::Module(id),
            Change::Update {
                online: None,
                cargo_unlimited: None,
                overrides: vec![(ATTR_HULL_DAMAGE, Some(0.0))],
            },
        )).collect();
        Self { changes, candidates: Vec::new(), stance: None }
    }

    /// A committed override-only field delta: `Some` sets the final persisted
    /// value, `None` removes the override and restores the type default.
    pub(crate) fn overridden_attributes(values: Vec<(Key, i32, Option<f64>)>) -> Self {
        let mut grouped: BTreeMap<Key, Vec<(i32, Option<f64>)>> = BTreeMap::new();
        for (key, attr, value) in values {
            grouped.entry(key).or_default().push((attr, value));
        }
        let mut candidates = Vec::new();
        let changes = grouped.into_iter().map(|(key, mut overrides)| {
            overrides.sort_unstable_by_key(|entry| entry.0);
            overrides.dedup_by_key(|entry| entry.0);
            candidates.extend(overrides.iter().filter_map(|&(attr, _)| generic(attr).then_some((key, attr))));
            (key, Change::Update { online: None, overrides, cargo_unlimited: None })
        }).collect();
        Self { changes, candidates, stance: None }
    }

    /// True when the patch carries override-only updates confined to `key`:
    /// nothing else may be admitted as that item's own field delta.
    pub(crate) fn overrides_only_on(&self, key: Key) -> bool {
        !self.changes.is_empty()
            && self.changes.iter().all(|(change_key, change)|
                *change_key == key
                && matches!(change, Change::Update { online: None, cargo_unlimited: None, .. }))
            && self.candidates.iter().all(|(candidate_key, _)| *candidate_key == key)
    }

    pub(crate) fn candidates(&self) -> &[(Key, i32)] { &self.candidates }

    /// The committed stance transition this patch publishes, set only by the
    /// terminal fitting of an accepted switch; ordinary fittings return `None`.
    pub(crate) fn ship_stance(&self) -> Option<StanceTransition> { self.stance }

    /// Attach the committed switch receipt to this patch.
    pub(crate) fn set_ship_stance(&mut self, stance: StanceTransition) { self.stance = Some(stance); }
}

#[cfg(all(test, any(feature = "tests-combat", feature = "tests-effects", feature = "tests-ships", feature = "tests-space")))] pub(crate) mod tests;
