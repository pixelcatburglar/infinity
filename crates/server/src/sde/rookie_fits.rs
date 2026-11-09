//! Server-authored civilian rookie fits, embedded and validated at startup.
//! Character selection and station grants use the same catalogue; it is not a
//! client or live-server loadout, and contains no charges or retroactive refits.
//! Fitted entries are checked against static rack effects and hull slot counts,
//! not dynamic fitting resources or skill requirements.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Context};
use serde::Deserialize;
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FALLBACK_ROOKIE_SHIP, FLAG_DRONE_BAY};
use crate::services::{effects::fitting::{rack, RACK_WIDTH}, inventory::{holds, is_fitting_flag}};
use super::{characters::Characters, types::Types};

const CONTENT: &str = include_str!("../../content/rookie_fits.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RookieFit {
    pub(crate) ship: i32,
    pub(crate) items: Vec<(i32, i32, i32)>,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct RookieFits {
    fits: HashMap<i32, RookieFit>,
}

impl RookieFits {
    pub(crate) fn load(types: &Types, characters: &Characters) -> anyhow::Result<Self> {
        Self::parse(CONTENT, |id| types.get(id).map(|ty| (ty.category_id, ty.group_id)),
            |id| rack(types, id),
            |ship, attr| types.get(ship).and_then(|ty| ty.fit_attribute(attr)).unwrap_or(0.0),
            characters.races().filter_map(|race| race.ship_type_id).chain(std::iter::once(FALLBACK_ROOKIE_SHIP)))
            .context("loading rookie fit catalogue")
    }

    fn parse(
        content: &str,
        class: impl Fn(i32) -> Option<(Option<i32>, Option<i32>)>,
        rack_for: impl Fn(i32) -> Option<(i32, i32, i32)>,
        slots_for: impl Fn(i32, i32) -> f64,
        required: impl IntoIterator<Item = i32>,
    ) -> anyhow::Result<Self> {
        let definitions: Vec<RookieFit> = serde_json::from_str(content)?;
        let mut fits = HashMap::with_capacity(definitions.len());
        for fit in definitions {
            if class(fit.ship).and_then(|(category, _)| category) != Some(CATEGORY_SHIP) {
                bail!("unknown or non-ship rookie hull {}", fit.ship);
            }
            let mut slots = HashSet::new();
            if fit.items.is_empty() { bail!("empty rookie fit for {}", fit.ship); }
            for &(type_id, flag, quantity) in &fit.items {
                let (category, group) = class(type_id).ok_or_else(|| anyhow::anyhow!("unknown rookie item type {type_id}"))?;
                if quantity <= 0 { bail!("invalid rookie item quantity for {type_id}"); }
                if is_fitting_flag(flag) {
                    if quantity != 1 || !slots.insert(flag) { bail!("invalid or duplicate rookie fitting slot {flag}"); }
                    let (_, attr, first) = rack_for(type_id).ok_or_else(|| anyhow::anyhow!("invalid rookie module rack for {type_id}"))?;
                    let count = (slots_for(fit.ship, attr) as i32).clamp(0, RACK_WIDTH);
                    if category != Some(CATEGORY_MODULE) || flag < first || flag >= first + count {
                        bail!("invalid rookie module {type_id} in fitting slot {flag}");
                    }
                } else if flag == FLAG_DRONE_BAY {
                    let hold = holds::descriptor(flag).expect("drone bay descriptor");
                    if !holds::accepts_class(hold, category, group, type_id) { bail!("invalid rookie drone bay item {type_id}"); }
                } else {
                    bail!("invalid rookie item flag {flag}");
                }
            }
            let ship = fit.ship;
            if fits.insert(ship, fit).is_some() { bail!("duplicate rookie hull {ship}"); }
        }
        for ship in required {
            if !fits.contains_key(&ship) { bail!("missing rookie fit for ship {ship}"); }
        }
        Ok(Self { fits })
    }

    pub(crate) fn get(&self, ship_type: i32) -> Option<&RookieFit> { self.fits.get(&ship_type) }
}

#[cfg(test)]
mod tests;
