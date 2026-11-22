//! Tactical stance protocol table: the hulls that expose stances and the
//! hidden mode item type each stance key selects.
//! Mirrors client `shipmode/data.py` hull -> (stance key, mode type) rows; item
//! persistence and switching live in `services::ship_stance` and `dao::items`.
//! All mode attributes, effects and the stance switch time stay in the SDE:
//! `validate` proves the deployed `Types`/`Dogma` cover the whole table at
//! startup. Strategic cruiser subsystem modes are not part of this table.
use anyhow::{anyhow, bail};
use infinity_core::consts::dogma::ATTR_STANCE_SWITCH_TIME;
use super::{dogma::Dogma, types::Types};

/// One supported hull and its (stance key, mode type) pairs.
pub(crate) struct HullStances {
    pub(crate) hull_type: i32,
    pub(crate) modes: [(i64, i32); 3],
}

// shipmode/data.py stance keys: every hull offers keys 1, 2 and 3, and
// `get_default_stance` answers key 1 for any supported hull.
/// shipmode/data.py `get_default_stance` (`shipStanceDefense`, key 1).
pub(crate) const DEFAULT_STANCE: i64 = 1;
/// shipmode/data.py `shipStanceSpeed` (key 2).
const SHIP_STANCE_SPEED: i64 = 2;
/// shipmode/data.py `shipStanceSniper` (key 3).
const SHIP_STANCE_SNIPER: i64 = 3;

/// Client dogma/const.py `effectCategoryPassive`: mode items run offline
/// passives, so every mode effect must be a passive with modifiers.
const PASSIVE_EFFECT_CATEGORY: i32 = 0;

// shipmode/data.py hull type IDs and per-hull mode item type IDs. The numeric
// keys are the client's protocol keys, not gameplay names: the SDE names of
// Anhinga's modes (Primary/Secondary/Tertiary) differ from the tactical
// destroyer icons, so the constants use key suffixes, not stance meanings.
/// shipmode/data.py: Confessor hull type.
const HULL_CONFESSOR: i32 = 34317;
/// shipmode/data.py: Confessor key 1 mode item type.
const MODE_CONFESSOR_1: i32 = 34319;
/// shipmode/data.py: Confessor key 2 mode item type.
const MODE_CONFESSOR_2: i32 = 34323;
/// shipmode/data.py: Confessor key 3 mode item type.
const MODE_CONFESSOR_3: i32 = 34321;
/// shipmode/data.py: Svipul hull type.
const HULL_SVIPUL: i32 = 34562;
/// shipmode/data.py: Svipul key 1 mode item type.
const MODE_SVIPUL_1: i32 = 34564;
/// shipmode/data.py: Svipul key 2 mode item type.
const MODE_SVIPUL_2: i32 = 34566;
/// shipmode/data.py: Svipul key 3 mode item type.
const MODE_SVIPUL_3: i32 = 34570;
/// shipmode/data.py: Jackdaw hull type.
const HULL_JACKDAW: i32 = 34828;
/// shipmode/data.py: Jackdaw key 1 mode item type.
const MODE_JACKDAW_1: i32 = 35676;
/// shipmode/data.py: Jackdaw key 2 mode item type.
const MODE_JACKDAW_2: i32 = 35677;
/// shipmode/data.py: Jackdaw key 3 mode item type.
const MODE_JACKDAW_3: i32 = 35678;
/// shipmode/data.py: Hecate hull type.
const HULL_HECATE: i32 = 35683;
/// shipmode/data.py: Hecate key 1 mode item type.
const MODE_HECATE_1: i32 = 35686;
/// shipmode/data.py: Hecate key 2 mode item type.
const MODE_HECATE_2: i32 = 35687;
/// shipmode/data.py: Hecate key 3 mode item type.
const MODE_HECATE_3: i32 = 35688;
/// shipmode/data.py: Skua hull type.
const HULL_SKUA: i32 = 89808;
/// shipmode/data.py: Skua key 1 mode item type.
const MODE_SKUA_1: i32 = 90060;
/// shipmode/data.py: Skua key 2 mode item type.
const MODE_SKUA_2: i32 = 90062;
/// shipmode/data.py: Skua key 3 mode item type.
const MODE_SKUA_3: i32 = 90064;
/// shipmode/data.py: Anhinga hull type.
const HULL_ANHINGA: i32 = 89807;
/// shipmode/data.py: Anhinga key 1 mode item type.
const MODE_ANHINGA_1: i32 = 90061;
/// shipmode/data.py: Anhinga key 2 mode item type.
const MODE_ANHINGA_2: i32 = 90065;
/// shipmode/data.py: Anhinga key 3 mode item type.
const MODE_ANHINGA_3: i32 = 90063;

pub(crate) static HULL_STANCES: [HullStances; 6] = [
    HullStances { hull_type: HULL_CONFESSOR, modes: [(DEFAULT_STANCE, MODE_CONFESSOR_1), (SHIP_STANCE_SPEED, MODE_CONFESSOR_2), (SHIP_STANCE_SNIPER, MODE_CONFESSOR_3)] },
    HullStances { hull_type: HULL_SVIPUL, modes: [(DEFAULT_STANCE, MODE_SVIPUL_1), (SHIP_STANCE_SPEED, MODE_SVIPUL_2), (SHIP_STANCE_SNIPER, MODE_SVIPUL_3)] },
    HullStances { hull_type: HULL_JACKDAW, modes: [(DEFAULT_STANCE, MODE_JACKDAW_1), (SHIP_STANCE_SPEED, MODE_JACKDAW_2), (SHIP_STANCE_SNIPER, MODE_JACKDAW_3)] },
    HullStances { hull_type: HULL_HECATE, modes: [(DEFAULT_STANCE, MODE_HECATE_1), (SHIP_STANCE_SPEED, MODE_HECATE_2), (SHIP_STANCE_SNIPER, MODE_HECATE_3)] },
    HullStances { hull_type: HULL_SKUA, modes: [(DEFAULT_STANCE, MODE_SKUA_1), (SHIP_STANCE_SPEED, MODE_SKUA_2), (SHIP_STANCE_SNIPER, MODE_SKUA_3)] },
    HullStances { hull_type: HULL_ANHINGA, modes: [(DEFAULT_STANCE, MODE_ANHINGA_1), (SHIP_STANCE_SPEED, MODE_ANHINGA_2), (SHIP_STANCE_SNIPER, MODE_ANHINGA_3)] },
];

/// The protocol entry of a supported hull, `None` for every other ship.
pub(crate) fn for_hull(type_id: i32) -> Option<&'static HullStances> {
    HULL_STANCES.iter().find(|stances| stances.hull_type == type_id)
}

/// The mode item type a stance key selects on a supported hull.
pub(crate) fn mode_type(hull_type: i32, key: i64) -> Option<i32> {
    for_hull(hull_type)?.modes.iter().find(|&&(k, _)| k == key).map(|&(_, mode)| mode)
}

/// The stance key a mode item type represents on a supported hull.
pub(crate) fn stance_for_mode(hull_type: i32, mode: i32) -> Option<i64> {
    for_hull(hull_type)?.modes.iter().find(|&&(_, m)| m == mode).map(|&(key, _)| key)
}

/// The hull's switch cooldown in milliseconds through the same metadata
/// semantics the fit uses: the type attribute, then the dogma default.
fn switch_time_ms(types: &Types, dogma: &Dogma, hull: i32) -> Option<f64> {
    types.attribute(hull, ATTR_STANCE_SWITCH_TIME)
        .or_else(|| dogma.attribute(ATTR_STANCE_SWITCH_TIME).and_then(|a| a.default_value))
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Startup data closure: every hull, mode type, effect and modifier attribute
/// the table names must exist in the deployed datasets, every mode effect must
/// be a passive with modifiers, and every hull must resolve a switch time.
pub(crate) fn validate(types: &Types, dogma: &Dogma) -> anyhow::Result<()> {
    if dogma.attribute(ATTR_STANCE_SWITCH_TIME).is_none() {
        bail!("ship stance switch attribute {ATTR_STANCE_SWITCH_TIME} missing from dogma");
    }
    for stances in HULL_STANCES.iter() {
        let hull = stances.hull_type;
        if types.get(hull).is_none() {
            bail!("ship stance hull type {hull} missing from SDE types");
        }
        if switch_time_ms(types, dogma, hull).is_none() {
            bail!("ship stance hull {hull} resolves no switch time");
        }
        for &(key, mode) in &stances.modes {
            let data = types.get(mode).ok_or_else(|| anyhow!("ship stance mode type {mode} missing from SDE types"))?;
            if data.effects().is_empty() {
                bail!("ship stance mode {mode} (hull {hull} key {key}) carries no effects");
            }
            for type_effect in data.effects() {
                let effect = dogma.effect(type_effect.effect_id)
                    .ok_or_else(|| anyhow!("ship stance mode {mode} effect {} missing from dogma", type_effect.effect_id))?;
                if effect.effect_category != Some(PASSIVE_EFFECT_CATEGORY) || effect.modifiers().is_empty() {
                    bail!("ship stance mode {mode} effect {} is not a passive with modifiers", effect.effect_id);
                }
                for modifier in effect.modifiers() {
                    if dogma.attribute(modifier.modified).is_none() {
                        bail!("ship stance mode {mode} effect {} references unknown attribute {}", effect.effect_id, modifier.modified);
                    }
                    // The mode's own source parameter resolves like the switch time:
                    // type attribute first, dogma default second; zero stays legal.
                    let source = types.attribute(mode, modifier.modifying)
                        .or_else(|| dogma.attribute(modifier.modifying).and_then(|a| a.default_value));
                    if source.is_none_or(|value| !value.is_finite()) {
                        bail!("ship stance mode {mode} effect {} resolves no finite source attribute {}", effect.effect_id, modifier.modifying);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "tests-sde"))]
mod tests;
