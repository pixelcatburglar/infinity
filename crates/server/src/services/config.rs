//! `config` service: the server side of the client's cfg Recordsets
//! (eve/common/script/sys/eveCfg.py). Multi-fetchers return `(header, rows)`.
//! Owner, location, ticker and alliance records come from game tables and the startup SDE.
//! `GetStationSolarSystemsByOwner` returns distinct systems from SDE stations;
//! player-owned structures are not included.

use super::{db_err, value_ids, Call, CallError, CallResult};
use crate::client::{short, Client};
use infinity_core::consts::{TYPE_CHARACTER, TYPE_CORPORATION, TYPE_ALLIANCE, TYPE_FACTION};
use marshal::Value;
use tracing::warn;

/// Client cfg ticker row: remaining optional fields after owner ID and ticker.
const TICKER_OPTIONAL_FIELDS: usize = 6;


fn header(cols: &[&str]) -> Value {
    Value::list(cols.iter().map(|c| Value::str(*c)))
}

pub async fn owners(c: &Client, ids: &[i64]) -> Result<Value, CallError> {
    let rows = crate::dao::characters::social::owners(&c.db, ids, TYPE_CHARACTER as i64, TYPE_CORPORATION, TYPE_ALLIANCE).await.map_err(db_err)?;
    let keys: std::collections::HashSet<_> = ids.iter().copied().collect();
    let owner_row = |id, name: Option<&str>, type_id, gender: Option<i32>, name_id: Option<i64>| Value::list([
        Value::Int(id), Value::unicode(name.unwrap_or_default()), Value::Int(type_id),
        Value::Int(gender.unwrap_or(0) as i64), name_id.map(Value::Int).unwrap_or(Value::None),
    ]);
    let mut data = Vec::new();
    data.extend(rows.iter().filter(|r| r.type_id == TYPE_CHARACTER as i64).map(|r| owner_row(r.id, r.name.as_deref(), r.type_id, r.gender, r.name_id)));
    data.extend(c.sde.npc_corporations.iter().filter(|r| keys.contains(&r.corporation_id)).map(|r| owner_row(r.corporation_id, r.name.as_deref(), TYPE_CORPORATION, Some(0), None)));
    data.extend(rows.iter().filter(|r| r.type_id == TYPE_CORPORATION).map(|r| owner_row(r.id, r.name.as_deref(), r.type_id, r.gender, r.name_id)));
    data.extend(c.sde.characters.npc_characters().filter(|r| keys.contains(&r.character_id)).map(|r| owner_row(r.character_id, r.name.as_deref(), TYPE_CHARACTER as i64, r.gender, r.name_id)));
    data.extend(c.sde.factions.iter().filter(|r| keys.contains(&r.faction_id)).map(|r| owner_row(r.faction_id, r.name.as_deref(), TYPE_FACTION, Some(0), None)));
    data.extend(rows.iter().filter(|r| r.type_id == TYPE_ALLIANCE).map(|r| owner_row(r.id, r.name.as_deref(), r.type_id, r.gender, r.name_id)));
    Ok(Value::tuple([header(&["ownerID", "ownerName", "typeID", "gender", "ownerNameID"]), Value::List(data)]))
}

/// Build an item-backed `evelocations` row in the client's seven-column descriptor order.
///
/// The location cache update is emitted only after the guarded mutation commits.
pub(crate) fn location_row(id: i64, name: &str) -> Value {
    Value::list([
        Value::Int(id),
        Value::unicode(name),
        Value::None,
        Value::Float(0.0),
        Value::Float(0.0),
        Value::Float(0.0),
        Value::None,
    ])
}

async fn locations(c: &Client, ids: &[i64]) -> Result<Value, CallError> {
    let rows = crate::dao::characters::social::locations(&c.db, ids).await.map_err(db_err)?;
    let mut data: Vec<_> = rows.iter().map(|r| {
        let name = r.name.as_deref()
            .or_else(|| c.sde.types.get(r.type_id).and_then(|t| t.name.as_deref()))
            .unwrap_or_default();
        location_row(r.id, name)
    }).collect();
    let keys: std::collections::HashSet<_> = ids.iter().copied().collect();
    data.extend(c.sde.map.systems().filter(|r| keys.contains(&r.solar_system_id)).map(|r| {
        Value::list([
            Value::Int(r.solar_system_id),
            Value::unicode(r.name.as_deref().unwrap_or_default()),
            Value::Int(r.solar_system_id),
            Value::Float(r.x.expect("solar system x is NULL")),
            Value::Float(r.y.expect("solar system y is NULL")),
            Value::Float(r.z.expect("solar system z is NULL")),
            Value::None,
        ])
    }));
    Ok(Value::tuple([header(&["locationID", "locationName", "solarSystemID", "x", "y", "z", "locationNameID"]), Value::List(data)]))
}

async fn tickers(c: &Client, ids: &[i64]) -> Result<Value, CallError> {
    let rows = crate::dao::characters::social::tickers(&c.db, ids).await.map_err(db_err)?;
    let keys: std::collections::HashSet<_> = ids.iter().copied().collect();
    let ticker_row = |id, ticker: Option<&str>| {
        let mut row = vec![Value::Int(id), Value::str(ticker.unwrap_or_default())];
        row.extend(std::iter::repeat_n(Value::None, TICKER_OPTIONAL_FIELDS));
        Value::List(row)
    };
    let mut data: Vec<_> = c.sde.npc_corporations.iter().filter(|r| keys.contains(&r.corporation_id))
        .map(|r| ticker_row(r.corporation_id, r.ticker.as_deref())).collect();
    data.extend(rows.iter().map(|r| ticker_row(r.id, r.ticker.as_deref())));
    Ok(Value::tuple([
        header(&["corporationID", "tickerName", "shape1", "shape2", "shape3", "color1", "color2", "color3"]),
        Value::List(data),
    ]))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    let keys = value_ids(call.args.first());
    match call.method {
        "GetMultiOwnersEx" | "GetOwnersEx" => owners(c, &keys).await,
        "GetMultiLocationsEx" | "GetLocationsEx" => locations(c, &keys).await,
        "GetMultiCorpTickerNamesEx" | "GetCorpTickerNamesEx" => tickers(c, &keys).await,
        // [KeyVal(solarSystemID)] where the corporation owns a station (info window, map).
        "GetStationSolarSystemsByOwner" => {
            let owner = keys.first().copied().unwrap_or(0);
            let systems: std::collections::BTreeSet<_> = c.sde.map.stations_by_owner(owner)
                .map(|r| r.solar_system_id).collect();
            Ok(Value::list(systems.into_iter().map(|id| super::keyval([
                ("solarSystemID", Value::Int(id.expect("station solar system is NULL"))),
            ]))))
        }
        "GetMultiAllianceShortNamesEx" | "GetAllianceShortNamesEx" => {
            let rows = crate::dao::characters::social::alliances(&c.db, &keys).await.map_err(db_err)?;
            let data = rows.into_iter().map(|r| Value::list([Value::Int(r.id), Value::unicode(r.name)])).collect();
            Ok(Value::tuple([header(&["allianceID", "shortName"]), Value::List(data)]))
        }
        _ => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED config::{} args={} kwargs={}",
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}
