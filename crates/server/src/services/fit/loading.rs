//! Cold construction of a `Fit` from persisted hull, character and projection
//! rows. Ship, module, charge, drone and character inputs are read once here;
//! later evaluation and committed patches reuse the nodes this loader built.
//! Mirrors the same dogma sources as the parent fit module. Activation,
//! overload and projection lifetime stay with their callers: this loader only
//! snapshots the rows and runtime sets it is given.
//! Hidden flag156 rows are tactical-stance candidates: the exact hull's single
//! recognized mode loads as an offline `Key::Module` source whose passive
//! effects run; every other hidden row gets no node, charge slot or loaded
//! seed, and multiple recognized modes are an integrity error.

use super::{character, fit_dao, runtime::RuntimeModifierSource, CallError, Fit, Key, Location, Node, CATEGORY_DRONE, CATEGORY_SHIP};
use crate::services::{combat, db_err, effects, skills};
use crate::sde::{ship_stances, Sde};
use infinity_core::consts::FLAG_HIDDEN_MODIFIERS;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

impl Fit {
    pub(super) async fn load_at(
        db: &(impl crate::db::executor::Executor + ?Sized),
        sde: Arc<Sde>,
        char_id: Option<i64>,
        ship: i64,
        projections: Vec<effects::projection::Projection>,
        at: i64,
        supplied_skills: Option<&[crate::dao::skills::training::Skill]>,
        supplied_installations: Option<&[crate::dao::skills::expert_systems::ExpertSystem]>,
        runtime_sources: Vec<RuntimeModifierSource>,
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
            let mut character: HashMap<i32, f64> = skills::attributes::base_character_attributes(db, &sde, char_id).await?
                .into_iter().filter_map(|(attr, value)| i32::try_from(attr).ok().map(|attr| (attr, value))).collect();
            character.extend(overrides.remove(&char_id).unwrap_or_default());
            let mut character: Vec<_> = character.into_iter().collect();
            character.sort_by_key(|v| v.0);
            Some(character)
        } else { None };
        let active = if char_id.is_some() { combat::active_modules() } else { HashSet::new() };
        let overloaded = if char_id.is_some() { effects::heat::overloaded_modules() } else { HashSet::new() };
        let mut nodes = HashMap::new();
        let node = |type_id, location, online, active, level, overrides| Node {
            type_id, data_position: sde.types.position(type_id), location, online, active,
            overloaded: false, projected_effect: None, projection_resistance_attribute: None, receiver: None, charge_type: None, level, overrides, cargo_unlimited: false, charge_values: Vec::new(),
        };

        let mut ship_node = node(ship_type, Location::Space, true, false, None, overrides.remove(&ship).unwrap_or_default());
        ship_node.cargo_unlimited = ship_cargo_unlimited;
        nodes.insert(Key::Ship, ship_node);

        let mut module_by_flag = HashMap::new();
        let mut stance = None;
        for r in &modules {
            if r.flag_id == FLAG_HIDDEN_MODIFIERS {
                if ship_stances::stance_for_mode(ship_type, r.type_id).is_some() {
                    if stance.replace(r).is_some() {
                        return Err(CallError::Internal(anyhow::anyhow!("ship stance integrity violation: multiple recognized mode items")));
                    }
                }
                continue;
            }
            module_by_flag.insert(r.flag_id, r.item_id);
            let mut module = node(r.type_id, Location::Ship, r.online, char_id.is_some() && r.online && active.contains(&r.item_id), None, overrides.remove(&r.item_id).unwrap_or_default());
            module.overloaded = char_id.is_some() && r.online && overloaded.contains(&r.item_id);
            nodes.insert(Key::Module(r.item_id), module);
        }
        if let Some(mode) = stance {
            nodes.insert(Key::Module(mode.item_id), node(mode.type_id, Location::Ship, false, false, None, overrides.remove(&mode.item_id).unwrap_or_default()));
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
            if m.flag_id == FLAG_HIDDEN_MODIFIERS { return None; }
            let charge = charged.get(&m.flag_id);
            (charge.is_some() || pendings.contains(&m.flag_id)).then_some(crate::space::combat::loaded::LoadedSeed {
                module: m.item_id, flag: m.flag_id, type_id: charge.map(|c| c.type_id),
                quantity: charge.map_or(0, |c| c.quantity), pending: pendings.contains(&m.flag_id) })
        }).collect();
        let mut fit = Fit { nodes, selected_effects: HashMap::new(), passive_modifiers: Vec::new(), passive_by_attribute: HashMap::new(), sde, memo: Mutex::new(HashMap::new()), ship_id, projection_revision: None, drone_projection_revision: None, character, character_at: at, charge_incarnations, loaded_seeds, runtime_sources: Vec::new(), reactive_armor: None };
        fit.rebuild_character_domain(at);
        fit.set_runtime_sources_sorted(runtime_sources);
        Ok(fit)
    }
}
