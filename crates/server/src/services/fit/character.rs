//! Character-domain inputs of a fit: the character attribute row, trained
//! skills, expert-system installations and brain items with their real
//! deadlines. These rows are captured once by the cold loader and carried by
//! the live fit so its actor can replace the whole domain on a committed
//! publish or let timed boosters and expert grants expire in place, without
//! another database read. Ship, module, charge, condition, runtime and
//! projection state is not part of this domain. Expiry is the in-actor
//! equivalent of the loader's as-of filtering: it never deletes items, grants
//! skills or rerolls booster chance effects, and a late publish can never
//! resurrect a deadline that already passed. `OwnerInputs` packages the whole
//! owner domain (ship, module, charge and ship-bound projection nodes plus
//! the raw character rows at their watermark) for the cold drone handoff; a
//! deadline already past inside a package expires at the receiving gate's
//! captured FILETIME instead of resurrecting until a timer fires.

use super::{chance_enabled, Fit, Key, Location, Node, EFFECT_CATEGORY_PASSIVE};
use crate::dao::items::brain::BrainItem;
use crate::dao::skills::{expert_systems::ExpertSystem as Installation, training::Skill};
use infinity_core::consts::{dogma::ATTR_SKILL_POINTS, dogma::ATTR_SKILL_TIME_CONSTANT, dogma::EFFECT_SKILL,
    FLAG_BOOSTER, FLAG_IMPLANT, TYPE_CHARACTER};

/// One brain item row and its persisted dogma overrides, kept for re-evaluation.
#[derive(Clone)]
pub(crate) struct BrainInput {
    pub(crate) item: BrainItem,
    pub(crate) overrides: Vec<(i32, f64)>,
}

/// The complete character domain of a fit, captured at one FILETIME.
///
/// A full publish replaces every `Char`/`Skill`/`Implant`/`Booster` node,
/// selected chance effects and the passive modifier index; items absent here
/// are removed. `Default` is the empty domain used to detach an owner.
#[derive(Clone, Default)]
pub(crate) struct CharacterInputs {
    /// FILETIME the rows were read and projected at.
    pub(crate) at: i64,
    /// Sorted base/override attribute rows of the character node.
    pub(crate) attributes: Option<Vec<(i32, f64)>>,
    pub(crate) trained: Vec<Skill>,
    pub(crate) installations: Vec<Installation>,
    pub(crate) brain: Vec<BrainInput>,
}

impl CharacterInputs {
    /// Deadlines that can still change this domain after `at`.
    fn deadlines(&self, at: i64) -> impl Iterator<Item = i64> + '_ {
        self.brain.iter()
            .flat_map(|brain| [brain.item.activated_at, brain.item.expires_at])
            .chain(self.installations.iter().flat_map(|i| [Some(i.installed_at), Some(i.expires_at)]))
            .flatten()
            .filter(move |deadline| *deadline > at)
    }
}

impl Fit {
    /// The retained raw character rows of this fit, or the empty domain.
    pub(crate) fn character_inputs(&self) -> CharacterInputs {
        self.character.clone().unwrap_or_default()
    }

    /// Replace the complete character domain, keeping the evaluation watermark
    /// monotone so stale publishes cannot resurrect expired deadlines.
    pub(crate) fn apply_character_inputs(&mut self, inputs: &CharacterInputs) {
        self.character_at = self.character_at.max(inputs.at);
        self.character = Some(inputs.clone());
        let at = self.character_at;
        self.rebuild_character_domain(at);
    }

    /// Earliest pending booster or expert-system deadline after the watermark.
    pub(crate) fn next_character_expiry(&self) -> Option<i64> {
        self.character.as_ref()?.deadlines(self.character_at).min()
    }

    /// Re-evaluate the character domain at `at`; true when a deadline crossed
    /// and the domain actually changed. Watermark still advances on misses so
    /// stale writes never resurrect expired rows.
    pub(crate) fn expire_character_inputs(&mut self, at: i64) -> bool {
        if self.character.is_none() || at <= self.character_at { return false; }
        if self.next_character_expiry().is_none_or(|deadline| deadline > at) {
            self.character_at = at;
            return false;
        }
        self.character_at = at;
        self.rebuild_character_domain(at);
        true
    }

    /// Rebuild every character node, chance selection and passive index from the
    /// retained rows, evaluated as of `at`. This is the same construction the
    /// cold loader used; only the evaluation time differs.
    pub(super) fn rebuild_character_domain(&mut self, at: i64) {
        self.nodes.retain(|key, _| !matches!(key, Key::Char | Key::Skill(_) | Key::Implant(_) | Key::Booster(_)));
        self.selected_effects.clear();
        self.passive_modifiers.clear();
        self.passive_by_attribute.clear();
        let Some(inputs) = self.character.clone() else {
            crate::poison::lock_or_exit(self.memo.get_mut()).clear();
            return;
        };
        let node = |type_id, level, overrides| Node {
            type_id, data_position: self.sde.types.position(type_id), location: Location::Char,
            online: true, active: false, overloaded: false, projected_effect: None, projection_resistance_attribute: None, receiver: None,
            charge_type: None, level, overrides, cargo_unlimited: false, charge_values: Vec::new(),
        };
        if let Some(attributes) = inputs.attributes {
            self.nodes.insert(Key::Char, node(TYPE_CHARACTER, None, attributes));
        }
        let snapshot = crate::services::skills::effective::project(&inputs.trained, &inputs.installations, &self.sde, at);
        let mut skill_effects = Vec::new();
        for (type_id, entry) in snapshot.iter() {
            let rank = self.sde.types.get(type_id).and_then(|d| d.fit_attribute(ATTR_SKILL_TIME_CONSTANT)).unwrap_or(0.0);
            let trained_points = entry.trained.as_ref().map_or(0, |skill| skill.skill_points);
            let trained_level = crate::services::skills::formulas::level_from_points(trained_points, rank);
            let level = entry.effective_level();
            let points = if entry.virtual_level.is_some_and(|virtual_level| virtual_level > trained_level) {
                trained_points.max(crate::services::skills::formulas::sp_for_level(rank, i64::from(level)))
            } else {
                trained_points
            };
            self.nodes.insert(Key::Skill(type_id), node(type_id, Some(level), vec![(ATTR_SKILL_POINTS, points as f64)]));
            if level <= 0 { continue; }
            if let Some(data) = self.sde.types.get(type_id) {
                for effect_entry in data.effects() {
                    if effect_entry.effect_id == EFFECT_SKILL { continue; }
                    if self.sde.dogma.effect(effect_entry.effect_id).is_some_and(|effect|
                        effect.effect_category.unwrap_or(0) == EFFECT_CATEGORY_PASSIVE
                        && chance_enabled(&self.selected_effects, Key::Skill(type_id), data, effect)) {
                        skill_effects.push((type_id, effect_entry.effect_id));
                    }
                }
            }
        }
        for brain in inputs.brain {
            // The same half-open window the loader's DAO query applies.
            if brain.item.flag_id == FLAG_BOOSTER
                && !(brain.item.activated_at.is_none_or(|activated| activated <= at)
                    && brain.item.expires_at.is_none_or(|expires| expires > at)) {
                continue;
            }
            let key = if brain.item.flag_id == FLAG_IMPLANT { Key::Implant(brain.item.item_id) } else { Key::Booster(brain.item.item_id) };
            self.nodes.insert(key, node(brain.item.type_id, None, brain.overrides));
            if let Some(mut ids) = brain.item.effect_ids {
                ids.sort_unstable();
                self.selected_effects.insert(brain.item.item_id, ids);
            }
        }
        skill_effects.sort_unstable();
        for (id, effect_id) in skill_effects {
            let Some(effect) = self.sde.dogma.effect(effect_id) else { continue };
            for (position, modifier) in effect.modifiers().iter().enumerate() {
                self.passive_by_attribute.entry(modifier.modified).or_default().push(self.passive_modifiers.len());
                self.passive_modifiers.push((Key::Skill(id), effect_id, position));
            }
        }
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
    }

    /// Detach the owner domain of an owned drone, keeping its own node,
    /// overrides and incoming projections. Used when control or ownership ends.
    pub(crate) fn strip_to_own_drone(&mut self, drone: i64) {
        let own = Key::Drone(drone);
        self.nodes.retain(|key, node| *key == own
            || matches!(key, Key::Projected(_) if node.receiver == Some(own)));
        self.character = None;
        self.character_at = 0;
        self.charge_incarnations.clear();
        self.selected_effects.clear();
        self.passive_modifiers.clear();
        self.passive_by_attribute.clear();
        self.ship_id = None;
        self.projection_revision = None;
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
    }

    /// The owner domain of this fit's own ship: raw character rows at their
    /// watermark plus the ship, module, charge and incoming ship-projection
    /// nodes as evaluated at the source gate. Returns None when this fit does
    /// not own a ship node (drones, type-only and unpiloted snapshots).
    pub(crate) fn owner_inputs(&self) -> Option<OwnerInputs> {
        if !self.nodes.contains_key(&Key::Ship) { return None; }
        let mut nodes: Vec<(Key, Node)> = self.nodes.iter()
            .filter(|(key, node)| matches!(key, Key::Ship | Key::Module(_) | Key::Charge(_))
                || matches!(key, Key::Projected(_) if node.receiver == Some(Key::Ship)))
            .map(|(key, node)| (*key, node.clone()))
            .collect();
        nodes.sort_unstable_by_key(|(key, _)| *key);
        Some(OwnerInputs {
            ship: self.ship_id?,
            character: self.character.clone().unwrap_or_default(),
            character_at: self.character_at,
            nodes,
            projection_revision: self.projection_revision,
        })
    }

    /// Replace this fit's owner domain with a published package. The old
    /// ship's nodes and its incoming projections disappear entirely; the
    /// receiver's own drone node, overrides, drone-bound projections and
    /// drone projection revision are untouched. Charge nodes arrive with
    /// fresh incarnations: the source ship's tokens never leave it.
    pub(crate) fn apply_owner_inputs(&mut self, inputs: &OwnerInputs) {
        self.nodes.retain(|key, node| !matches!(key, Key::Ship | Key::Module(_) | Key::Charge(_))
            && !matches!(key, Key::Projected(_) if node.receiver == Some(Key::Ship)));
        for (key, node) in &inputs.nodes {
            if let Key::Charge(module) = key {
                self.charge_incarnations.insert(*module, crate::space::targeting::next_generation());
            }
            self.nodes.insert(*key, node.clone());
        }
        self.charge_incarnations.retain(|module, _| self.nodes.contains_key(&Key::Charge(*module)));
        self.ship_id = Some(inputs.ship);
        self.projection_revision = inputs.projection_revision;
        self.character = Some(inputs.character.clone());
        // The watermark stays monotone; a still-live caller expiry applies it
        // at the same captured FILETIME so a deadline in the package can never
        // resurrect into outputs before its timer.
        self.character_at = self.character_at.max(inputs.character_at);
        let at = self.character_at;
        self.rebuild_character_domain(at);
    }
}

/// One owner's complete ship and character domain, carried between systems.
///
/// The raw rows and nodes are the necessary per-recipient copies at this cold
/// handoff boundary, not a per-cycle whole-fit clone; a single Arc package can
/// serve every captured drone of the same owner.
pub(crate) struct OwnerInputs {
    /// The real ship item the package came from.
    pub(crate) ship: i64,
    character: CharacterInputs,
    character_at: i64,
    nodes: Vec<(Key, Node)>,
    projection_revision: Option<u64>,
}

impl OwnerInputs {
    /// The borrowed modifier-source view a receiving fit snapshots before the
    /// package lands: the raw character domain plus each node's
    /// `(type_id, charge_type, projected_effect)` descriptor. Nodes and
    /// topology stay private; which modifiers actually run remains the
    /// receiving evaluator's call.
    pub(crate) fn notification_sources(&self) -> (
        &CharacterInputs,
        impl Iterator<Item = (i32, Option<i32>, Option<i32>)> + '_,
    ) {
        (&self.character,
            self.nodes.iter().map(|(_, node)| (node.type_id, node.charge_type, node.projected_effect)))
    }
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
