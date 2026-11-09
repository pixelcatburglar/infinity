//! Map history and personal arrival visits RPCs for
//! `eve/client/script/ui/shared/mapView/mapViewColorHandler.py` and
//! `eve/client/script/ui/shared/mapView/filters/mapFilterVisited.py`.
//! Reads `game.map_events`, `game.map_visits`, and committed sovereignty through DAOs.
//! Topology, cynos and faction warfare history are not supported here.
use infinity_core::{consts::FILETIME_HOUR, messages};
use crate::{client::{Client, filetime_now}, dao::map_telemetry::{self as dao, History, HistoryStat, Visit}};
use crate::services::{Call, CallError, CallResult, notify_error, user_error, rowset, keyval};
use marshal::Value;

/// `eve/common/lib/appConst.py`: `mapHistoryStatJumps`.
const HISTORY_STAT_JUMPS: i64 = 1;
/// `eve/common/lib/appConst.py`: `mapHistoryStatKills`.
const HISTORY_STAT_KILLS: i64 = 3;
/// `eve/common/lib/appConst.py`: `mapHistoryStatFacWarKills`.
const HISTORY_STAT_FAC_WAR_KILLS: i64 = 5;
/// Hour window used by `mapViewColorHandler.py` and
/// `filters/mapFilterJumps.py` and `filters/mapFilterKills.py`.
const HISTORY_WINDOW_HOUR: i64 = 1;
/// Day window in hours used by `mapViewColorHandler.py` and
/// `filters/mapFilterJumps.py` and `filters/mapFilterKills.py`.
const HISTORY_WINDOW_DAY_HOURS: i64 = 24;

const HEADER: &[&str] = &["solarSystemID", "value1", "value2", "value3"];
fn invalid() -> CallError { notify_error(messages::map::INVALID_MAP_REQUEST) }
fn empty_kwargs(kwargs: &Value) -> bool { matches!(kwargs, Value::None) || matches!(kwargs, Value::Dict(entries) if entries.is_empty()) }

pub(super) fn history_request(call: &Call<'_>) -> Result<(Option<HistoryStat>, i64), CallError> {
    if !empty_kwargs(call.kwargs) { return Err(invalid()); }
    let [Value::Int(stat), Value::Int(hours)] = call.args else { return Err(invalid()); };
    let stat = match *stat { HISTORY_STAT_JUMPS => Some(HistoryStat::Jumps), HISTORY_STAT_KILLS => Some(HistoryStat::Kills), HISTORY_STAT_FAC_WAR_KILLS => None, _ => return Err(invalid()) };
    if !matches!(*hours, HISTORY_WINDOW_HOUR | HISTORY_WINDOW_DAY_HOURS) { return Err(invalid()); }
    Ok((stat, *hours))
}

pub(super) fn visits_request(call: &Call<'_>) -> Result<(), CallError> {
    if call.args.is_empty() && empty_kwargs(call.kwargs) { Ok(()) } else { Err(invalid()) }
}

pub(super) fn history_value(rows: Vec<History>) -> Value {
    rowset(HEADER, rows.into_iter().map(|r| vec![Value::Int(r.solar_system_id), Value::Int(r.value1), Value::Int(r.value2), Value::Int(r.value3)]).collect())
}

pub(super) fn visits_value(rows: Vec<Visit>) -> Value {
    Value::list(rows.into_iter().map(|r| keyval([
        ("lastDateTime", Value::Int(r.last_date_time)),
        ("solarSystemID", Value::Int(r.solar_system_id)),
        ("visits", Value::Int(r.visits)),
    ])))
}

pub(super) fn selected(character: Option<i64>) -> Result<i64, CallError> {
    character.filter(|id| *id > 0).ok_or_else(|| user_error(messages::MSG_CHARACTER_NOT_FOUND, vec![]))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "map" || !matches!(call.method, "GetHistory" | "GetSolarSystemVisits" | "GetCurrentSovData" | "GetRecentSovActivity") { return None; }
    Some(handle(c, call).await)
}

async fn handle(c: &Client, call: &Call<'_>) -> CallResult {
    use crate::services::sovereignty::queries::{self, Query};
    if matches!(call.method, "GetHistory" | "GetSolarSystemVisits") {
        selected(c.session.int("charid"))?;
    }
    match call.method {
        "GetCurrentSovData" => {
            if !empty_kwargs(call.kwargs) { return Err(invalid()); }
            let [Value::Int(location_id)] = call.args else { return Err(invalid()); };
            if *location_id <= 0 { return Err(invalid()); }
            queries::read(&c.db, &c.sde, Query::CurrentSovData { location_id: *location_id }).await
        }
        "GetRecentSovActivity" => {
            visits_request(call)?;
            queries::read(&c.db, &c.sde, Query::RecentSovActivity).await
        }
        "GetHistory" => {
            let (stat, hours) = history_request(call)?;
            let Some(stat) = stat else { return Ok(history_value(vec![])); };
            let now = filetime_now();
            let rows = dao::history(&c.db, stat, now.saturating_sub(hours * FILETIME_HOUR), now).await?;
            Ok(history_value(rows))
        }
        "GetSolarSystemVisits" => {
            visits_request(call)?;
            Ok(visits_value(dao::visits(&c.db, selected(c.session.int("charid"))?).await?))
        }
        _ => unreachable!(),
    }
}
