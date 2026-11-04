//! Map, corporation and wallet-side services the neocom windows load from
//! (eve/client/script/ui/shared/maps, mapView, neocom/corporation, wallet).
//! Corporation rows come from the startup SDE dataset or `game.corporations`;
//! character corporation lookup uses `game.characters` and the startup NPC dataset.
//! Public corporation rows include `allowWar` (false for NPC corporations).
//! NPC corporation trading is not modelled: `GetCorpInfo` returns an empty list.
//! Many map, wallet and war queries likewise return empty fallback data.

use super::{db_err, row, Call, CallError, CallResult};
use crate::client::Client;
use crate::consts::{CORPORATION_TYPE_NPC, CORPORATION_UNLIMITED_MEMBER_LIMIT, CORP_DIVISION_NAME_COUNT, FACTION_AMARR, FACTION_CALDARI, FACTION_GALLENTE, FACTION_MINMATAR};
use marshal::Value;

/// Faction warfare warzones: (warzoneID, the two factions fighting over it).
pub(super) const WARZONES: &[(i64, [i64; 2])] = &[
    (1, [FACTION_AMARR, FACTION_MINMATAR]), // Amarr Empire / Minmatar Republic
    (2, [FACTION_CALDARI, FACTION_GALLENTE]), // Caldari State / Gallente Federation
];

/// Server default NPC corporation shares in public corporation information.
const NPC_CORPORATION_SHARES: i64 = 1000;
/// Client corporation row: optional logo shape, colour and typeface fields.
const CORP_LOGO_FIELDS: usize = 7;
/// Server default maximum dynamic resource output.
pub(super) const DYNAMIC_RESOURCE_MAX_OUTPUT: f64 = 2.0;

/// Public corporation row consumed by eve/client/script/ui/shared/neocom/corporation.
const CORP_HEADER: &[&str] = &[
    "corporationID",
    "corporationName",
    "description",
    "tickerName",
    "url",
    "taxRate",
    "loyaltyPointTaxRate",
    "minimumJoinStanding",
    "corporationType",
    "hasPlayerPersonnelManager",
    "sendCharTerminationMessage",
    "creatorID",
    "ceoID",
    "stationID",
    "raceID",
    "allianceID",
    "warFactionID",
    "shares",
    "memberCount",
    "memberLimit",
    "allowedMemberRaceIDs",
    "graphicID",
    "shape1",
    "shape2",
    "shape3",
    "color1",
    "color2",
    "color3",
    "typeface",
    "division1",
    "division2",
    "division3",
    "division4",
    "division5",
    "division6",
    "division7",
    "walletDivision1",
    "walletDivision2",
    "walletDivision3",
    "walletDivision4",
    "walletDivision5",
    "walletDivision6",
    "walletDivision7",
    "deleted",
    "isRecruiting",
    "friendlyFire",
    "aggressionSettings",
    "createDateTime",
    "chosenExecutorID",
    "allowWar",
];

/// A corporation row: NPC corporations from the SDE, player corporations from game.corporations.
async fn corporation(c: &Client, corp_id: i64) -> Result<Value, CallError> {
    let Some(r) = c.sde.npc_corporations.get(corp_id) else {
        return Ok(super::corps::corporation_line(&c.db, &c.sde, corp_id).await?.map(|line| row(Value::list(CORP_HEADER.iter().map(|h| Value::str(*h))), line)).unwrap_or(Value::None));
    };
    let member_count = crate::dao::characters::routing::npc_member_count(&c.db, corp_id).await.map_err(db_err)?;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    let s = |v: Option<&str>| Value::unicode(v.unwrap_or_default());
    let mut line = vec![
        Value::Int(corp_id),
        s(r.name.as_deref()),
        s(r.description.as_deref()),
        s(r.ticker.as_deref()),
        Value::unicode(""),
        Value::Float(r.tax_rate.unwrap_or(0.0)),
        Value::Float(0.0),
        Value::Float(0.0),
        Value::Int(CORPORATION_TYPE_NPC),
        Value::Bool(false),
        Value::Bool(true),
        opt(r.ceo_id),
        opt(r.ceo_id),
        opt(r.station_id),
        opt(r.race_id),
        Value::None,
        opt(r.faction_id),
        Value::Int(NPC_CORPORATION_SHARES),
        Value::Int(member_count),
        Value::Int(r.member_limit.unwrap_or(CORPORATION_UNLIMITED_MEMBER_LIMIT)),
        Value::None,
        Value::None,
    ];
    line.extend(std::iter::repeat_n(Value::None, CORP_LOGO_FIELDS)); // shapes, colours, typeface
    line.extend(std::iter::repeat_n(Value::None, CORP_DIVISION_NAME_COUNT)); // division names
    // ..., chosenExecutorID, allowWar (no wars can be declared on NPC corporations)
    line.extend([Value::Bool(false), Value::Bool(false), Value::Bool(false), Value::None, Value::None, Value::None, Value::Bool(false)]);
    Ok(row(Value::list(CORP_HEADER.iter().map(|h| Value::str(*h))), line))
}


mod map;
mod corp_registry;
mod corpmgr;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if let Some(reply) = super::fixed::universe(call) { return Some(reply); }
    match call.service {
        "map" => map::dispatch(c, call).await,
        "corpRegistry" => corp_registry::dispatch(c, call).await,
        "corpmgr" => corpmgr::dispatch(c, call).await,
        _ => None,
    }
}
