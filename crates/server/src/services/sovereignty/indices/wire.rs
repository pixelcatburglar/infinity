//! `devIndexManager` wire projections of computed development views.
//! `GetDevelopmentIndicesForSystem` mirrors the attribute-keyed dict consumed
//! by `eve/client/script/ui/services/sovSvc.py::GetLevelInfoForIndex`, and
//! `GetAllDevelopmentIndices` the map list of
//! `eve/client/script/ui/shared/mapView/filters/mapFilterDevIndex.py`; entries
//! are KeyVal records with Float points and a Bool trend flag.
use marshal::Value;
use crate::{lua::content::indices::Definitions, services::keyval};
use super::model::DevelopmentView;

/// `sovSvc.py::CLAIM_DAYS_TO_SECONDS`: `claimedFor` is reported in whole days'
/// worth of seconds, here fractional days of claim age.
const SECONDS_PER_CLAIMED_DAY: i64 = infinity_core::consts::FILETIME_DAY / infinity_core::consts::FILETIME_SECOND;

fn entry(points: f64, increasing: bool) -> Value {
    keyval([("points", Value::Float(points)), ("increasing", Value::Bool(increasing))])
}

/// One system's per-index record keyed by the compiled attribute identifiers;
/// strategic points are claim age in seconds.
pub(crate) fn development_system(view: &DevelopmentView, definitions: &Definitions) -> Value {
    Value::Dict(vec![
        (Value::Int(definitions.military_attribute), entry(view.military_points, view.military_increasing)),
        (Value::Int(definitions.industrial_attribute), entry(view.industrial_points, view.industrial_increasing)),
        (Value::Int(definitions.strategic_attribute), entry(view.strategic_seconds, view.strategic_increasing)),
    ])
}

/// The whole-map list in ascending system order; `claimedFor` is fractional
/// days, never truncated.
pub(crate) fn development_all(views: &[DevelopmentView]) -> Value {
    Value::List(views.iter().map(|view| keyval([
        ("solarSystemID", Value::Int(view.system_id)),
        ("militaryPoints", Value::Float(view.military_points)),
        ("industrialPoints", Value::Float(view.industrial_points)),
        ("claimedFor", Value::Float(view.strategic_seconds / SECONDS_PER_CLAIMED_DAY as f64)),
    ])).collect())
}

#[cfg(all(test, feature = "tests-content"))]
mod tests;
