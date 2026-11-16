//! Typed decode and admission of the client's `fleetFormationSettings` dictionary
//! (evefleet/const.py, sent by eve/client/script/remote/michelle.py). Choices gate on the
//! commander's trained Fleet Formations and Fleet Coordination levels in `game.skills`;
//! effective levels, per-member checks and persisted preferences are out of scope.
use super::*;

/// Client evefleet.const skill gating formation type choice.
const FORMATION_SKILL_TYPE: i32 = 57317;
/// Client evefleet.const skill gating spacing and size tiers.
const COORDINATION_SKILL_TYPE: i32 = 57318;
/// Client evefleet.const spacing choices in metres; the index is the required coordination level.
const SPACING_M: [f64; 6] = [1_000.0, 2_000.0, 3_000.0, 4_000.0, 5_000.0, 10_000.0];
/// Client evefleet.const size choices in metres; the index is the required coordination level.
const SIZE_M: [f64; 6] = [10_000.0, 20_000.0, 30_000.0, 40_000.0, 50_000.0, 100_000.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FormationKind { Point, Sphere, Plane, Wall, Arrow, Relative }
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FormationSettings {
    pub(super) kind: FormationKind,
    pub(super) spacing_m: f64,
    pub(super) size_m: f64,
}
impl Default for FormationSettings {
    fn default() -> Self { Self { kind: FormationKind::Point, spacing_m: SPACING_M[0], size_m: SIZE_M[0] } }
}

/// Strict dict decode: exactly the three text keys, each once, each an integer in range.
fn decode_dict(wire: &Value) -> Option<FormationSettings> {
    let Value::Dict(entries) = wire else { return None };
    let mut fields = [("formationType", None), ("formationSpacing", None), ("formationSize", None)];
    for (key, value) in entries {
        let text = key.as_text()?;
        let Some(slot) = fields.iter_mut().find(|(name, _)| *name == text.as_ref()) else { return None };
        if slot.1.is_some() { return None; }
        let Value::Int(n) = value else { return None };
        slot.1 = Some(*n);
    }
    let [(_, kind), (_, spacing), (_, size)] = fields;
    let kind = match kind? {
        0 => FormationKind::Point, 1 => FormationKind::Sphere, 2 => FormationKind::Plane,
        3 => FormationKind::Wall, 4 => FormationKind::Arrow, 5 => FormationKind::Relative,
        _ => return None,
    };
    let (spacing, size) = (spacing?, size?);
    let spacing_m = SPACING_M.iter().copied().find(|v| *v == spacing as f64)?;
    let size_m = SIZE_M.iter().copied().find(|v| *v == size as f64)?;
    Some(FormationSettings { kind, spacing_m, size_m })
}
/// Decode the wire value; `None` means the whole dictionary was absent and defaults to Point.
fn decode(wire: Option<&Value>) -> Result<FormationSettings, CallError> {
    let Some(wire) = wire else { return Ok(FormationSettings::default()) };
    decode_dict(wire).ok_or_else(|| notify_error(messages::fleets::INVALID_REQUEST))
}
/// Required (formation, coordination) trained levels for a decoded choice.
fn requirements(s: &FormationSettings) -> (i32, i32) {
    let coordination = SPACING_M.iter().position(|v| *v == s.spacing_m).unwrap_or(0)
        .max(SIZE_M.iter().position(|v| *v == s.size_m).unwrap_or(0)) as i32;
    (s.kind as i32, coordination)
}
/// Validate a decoded choice against the commander's trained skills with one focused read;
/// zero-level requirements skip the database entirely.
pub(super) async fn load(db: &crate::db::Db, character: i64, wire: Option<&Value>) -> Result<FormationSettings, CallError> {
    let settings = decode(wire)?;
    let (formation, coordination) = requirements(&settings);
    if formation == 0 && coordination == 0 { return Ok(settings); }
    let levels = crate::dao::skills::formations::trained_levels(db, character, FORMATION_SKILL_TYPE, COORDINATION_SKILL_TYPE).await?;
    if levels.formation < formation || levels.coordination < coordination {
        return Err(notify_error(messages::fleets::INVALID_REQUEST));
    }
    Ok(settings)
}

#[cfg(all(test, feature = "tests-social"))]
#[path = "settings/tests.rs"]
mod tests;
