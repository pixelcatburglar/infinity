//! Probe scanning inputs read from a pilot's resident fit: the client's probe
//! scanner (probescanning/scanBonusController.py) shows the loaded probe's
//! effective baseSensorStrength and baseMaxScanDeviation and the launcher's
//! effective duration; the server reads the same values here, so skills, hull
//! and subsystem bonuses, scan arrays, implants and command bursts apply
//! through the shared dogma evaluator. Mirrors probescanning/const.py and
//! eve/client/script/parklife/scanSvc.py:476-485 (the launcher is the first
//! online scan probe launcher). Data: Sde and the actor's resident Fit only.
//! Not supported: per-probe launcher memory; the launch-time module and hull
//! breakdown is a display aid, not a scan input.

use super::*;
use crate::services::fit::{Fit, Key, SourceScope};
use infinity_core::consts::dogma::ATTR_DURATION;

/// Client inventorycommon/const.py `groupScanProbeLauncher`.
pub(super) const GROUP_SCAN_PROBE_LAUNCHER: i32 = 481;
/// Client probescanning/const.py SCAN_STRENGTH_ATTRIBUTE_ID (dogma baseSensorStrength).
pub(super) const ATTR_BASE_SENSOR_STRENGTH: i32 = 1371;
/// Client probescanning/const.py SCAN_DEVIATION_ATTRIBUTE_ID (dogma baseMaxScanDeviation, AU).
const ATTR_BASE_MAX_SCAN_DEVIATION: i32 = 1372;
/// Client dogma/const.py attributeBaseScanRange (AU), the range the deviation is quoted at.
const ATTR_BASE_SCAN_RANGE: i32 = 1370;
/// Client scanBonusController.py:115-122 reads scanBonuses[bonus][origin] for these keys.
const SCAN_BONUS_STRENGTH: &str = "strength";
const SCAN_BONUS_DEVIATION: &str = "deviation";
const SCAN_ORIGIN_MODULES: &str = "modules";
const SCAN_ORIGIN_SHIP: &str = "ship";

/// Module and hull percentages of a probe's strength and deviation at launch,
/// sent on each probe for the client's empty-launcher bonus breakdown.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(super) struct ProbeBonuses {
    pub strength_modules: f64,
    pub strength_ship: f64,
    pub deviation_modules: f64,
    pub deviation_ship: f64,
}

/// One probe type's scan-time inputs.
pub(super) struct ProbeTypeInputs {
    pub type_id: i32,
    pub strength: f64,
    pub deviation_per_range: f64,
}

/// Everything a scan takes from the resident fit.
pub(super) struct ScanInputs {
    pub scan_ms: f64,
    pub types: Vec<ProbeTypeInputs>,
}

/// One probe taking part in a scan: position, scan range (m), effective
/// strength and max deviation per metre of range.
pub(super) struct ScanningProbe {
    pub pos: V3,
    pub range: f64,
    pub strength: f64,
    pub deviation_per_range: f64,
}

/// The first online scan probe launcher by item id, the client's GetProbeLauncher rule.
pub(super) fn probe_launcher(fit: &Fit) -> Option<i64> {
    fit.fitted_modules()
        .filter(|(item, type_id)| {
            fit.sde().types.get(*type_id).is_some_and(|t| t.group_id == Some(GROUP_SCAN_PROBE_LAUNCHER))
                && fit.module_state(*item).is_some_and(|(online, _)| online)
        })
        .map(|(item, _)| item)
        .min()
}

/// Scan time (the first online launcher's effective duration) and, per probe
/// type, the effective strength and deviation per metre of range (effective
/// baseMaxScanDeviation over the type's baseScanRange; 0 when the type has no
/// base range). Refused without an online launcher: the client cannot analyse
/// a scan then (ScnNoLauncher).
pub(super) fn scan_inputs(fit: &Fit, types: &[i32]) -> Result<ScanInputs, CallError> {
    let launcher = probe_launcher(fit).ok_or_else(|| CallError::User {
        msg: messages::MSG_SCN_NO_LAUNCHER.into(),
        dict: vec![],
    })?;
    let scan_ms = fit.value(Key::Module(launcher), ATTR_DURATION);
    let types = types
        .iter()
        .map(|&type_id| {
            let strength = fit.charge_value(launcher, type_id, ATTR_BASE_SENSOR_STRENGTH, SourceScope::All);
            let deviation_per_range = match fit.sde().types.attribute(type_id, ATTR_BASE_SCAN_RANGE) {
                Some(base_range) if base_range > 0.0 => {
                    fit.charge_value(launcher, type_id, ATTR_BASE_MAX_SCAN_DEVIATION, SourceScope::All) / base_range
                }
                _ => 0.0,
            };
            ProbeTypeInputs { type_id, strength, deviation_per_range }
        })
        .collect();
    Ok(ScanInputs { scan_ms, types })
}

/// Launch-time module and hull percentages for the client's empty-launcher breakdown.
pub(super) fn probe_bonuses(fit: &Fit, launcher: i64, type_id: i32) -> ProbeBonuses {
    let pct = |attr: i32, scope: SourceScope| match fit.sde().types.attribute(type_id, attr) {
        Some(base) if base > 0.0 => (fit.charge_value(launcher, type_id, attr, scope) / base - 1.0) * PERCENT_SCALE,
        _ => 0.0,
    };
    ProbeBonuses {
        strength_modules: pct(ATTR_BASE_SENSOR_STRENGTH, SourceScope::Modules),
        strength_ship: pct(ATTR_BASE_SENSOR_STRENGTH, SourceScope::Hull),
        deviation_modules: pct(ATTR_BASE_MAX_SCAN_DEVIATION, SourceScope::Modules),
        deviation_ship: pct(ATTR_BASE_MAX_SCAN_DEVIATION, SourceScope::Hull),
    }
}

/// The scanBonuses dict of a probe KeyVal.
pub(super) fn scan_bonuses_value(b: &ProbeBonuses) -> Value {
    let origins = |modules: f64, ship: f64| {
        Value::Dict(vec![
            (Value::str(SCAN_ORIGIN_MODULES), Value::Float(modules)),
            (Value::str(SCAN_ORIGIN_SHIP), Value::Float(ship)),
        ])
    };
    Value::Dict(vec![
        (Value::str(SCAN_BONUS_STRENGTH), origins(b.strength_modules, b.strength_ship)),
        (Value::str(SCAN_BONUS_DEVIATION), origins(b.deviation_modules, b.deviation_ship)),
    ])
}
