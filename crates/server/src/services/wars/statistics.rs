//! Immutable war reports for `warStatisticMgr` and the client `warReport.py`.
//! Reads `game.wars`, `game.killmail_wars`, `game.killmails` and startup SDE types.
//! Old alliance snapshots remain unknown; types without an SDE group are omitted only
//! from group summaries and class-filtered reports, never from unfiltered totals.

use std::collections::BTreeMap;
use infinity_core::{consts::{CATEGORY_STARBASE, CATEGORY_STRUCTURE}, messages};
use crate::{client::Client, dao::{killmails::wars::IndexedKillmail, wars::{self, War}}};
use crate::services::{Call, CallError, CallResult, db_err, notify_error};
use crate::services::crimewatch::killmails::killmail_value;
use marshal::Value;
use super::wire;

/// Client `appConst.GROUP_CAPSULES`.
const CLASS_CAPSULES: i64 = 0;
/// Client `appConst.GROUP_FRIGATES`.
const CLASS_FRIGATES: i64 = 1;
/// Client `appConst.GROUP_DESTROYERS`.
const CLASS_DESTROYERS: i64 = 2;
/// Client `appConst.GROUP_CRUISERS`.
const CLASS_CRUISERS: i64 = 3;
/// Client `appConst.GROUP_BATTLESHIPS`.
const CLASS_BATTLESHIPS: i64 = 4;
/// Client `appConst.GROUP_BATTLECRUISERS`.
const CLASS_BATTLECRUISERS: i64 = 5;
/// Client `appConst.GROUP_CAPITALSHIPS`.
const CLASS_CAPITALSHIPS: i64 = 6;
/// Client `appConst.GROUP_INDUSTRIALS`.
const CLASS_INDUSTRIALS: i64 = 7;
/// Client `appConst.GROUP_POS`.
const CLASS_POS: i64 = 8;
/// Client `appConst.GROUP_STRUCTURES`.
const CLASS_STRUCTURES: i64 = 9;
/// Client `inventorycommon.const.groupCapsule`.
const GROUP_CAPSULE: i32 = 29;
/// Client `inventorycommon.const.groupShuttle`.
const GROUP_SHUTTLE: i32 = 31;
/// Client `inventorycommon.const.groupRookieship`.
const GROUP_ROOKIESHIP: i32 = 237;
/// Client `inventorycommon.const.groupFrigate`.
const GROUP_FRIGATE: i32 = 25;
/// Client `inventorycommon.const.groupAssaultFrigate`.
const GROUP_ASSAULT_FRIGATE: i32 = 324;
/// Client `inventorycommon.const.groupCovertOps`.
const GROUP_COVERT_OPS: i32 = 830;
/// Client `inventorycommon.const.groupElectronicAttackShips`.
const GROUP_ELECTRONIC_ATTACK_SHIPS: i32 = 893;
/// Client `inventorycommon.const.groupPrototypeExplorationShip`.
const GROUP_PROTOTYPE_EXPLORATION_SHIP: i32 = 1022;
/// Client `inventorycommon.const.groupInterceptor`.
const GROUP_INTERCEPTOR: i32 = 831;
/// Client `inventorycommon.const.groupStealthBomber`.
const GROUP_STEALTH_BOMBER: i32 = 834;
/// Client `inventorycommon.const.groupExpeditionFrigate`.
const GROUP_EXPEDITION_FRIGATE: i32 = 1283;
/// Client `inventorycommon.const.groupLogisticsFrigate`.
const GROUP_LOGISTICS_FRIGATE: i32 = 1527;
/// Client `inventorycommon.const.groupDestroyer`.
const GROUP_DESTROYER: i32 = 420;
/// Client `inventorycommon.const.groupInterdictor`.
const GROUP_INTERDICTOR: i32 = 541;
/// Client `inventorycommon.const.groupTacticalDestroyer`.
const GROUP_TACTICAL_DESTROYER: i32 = 1305;
/// Client `inventorycommon.const.groupCommandDestroyer`.
const GROUP_COMMAND_DESTROYER: i32 = 1534;
/// Client `inventorycommon.const.groupCruiser`.
const GROUP_CRUISER: i32 = 26;
/// Client `inventorycommon.const.groupCombatReconShip`.
const GROUP_COMBAT_RECON_SHIP: i32 = 906;
/// Client `inventorycommon.const.groupForceReconShip`.
const GROUP_FORCE_RECON_SHIP: i32 = 833;
/// Client `inventorycommon.const.groupHeavyAssaultCruiser`.
const GROUP_HEAVY_ASSAULT_CRUISER: i32 = 358;
/// Client `inventorycommon.const.groupHeavyInterdictors`.
const GROUP_HEAVY_INTERDICTORS: i32 = 894;
/// Client `inventorycommon.const.groupLogistics`.
const GROUP_LOGISTICS: i32 = 832;
/// Client `inventorycommon.const.groupFlagCruiser`.
const GROUP_FLAG_CRUISER: i32 = 1972;
/// Client `inventorycommon.const.groupSpecialEditionYacht`.
const GROUP_SPECIAL_EDITION_YACHT: i32 = 5087;
/// Client `inventorycommon.const.groupBattlecruiser`.
const GROUP_BATTLECRUISER: i32 = 419;
/// Client `inventorycommon.const.groupAttackBattlecruiser`.
const GROUP_ATTACK_BATTLECRUISER: i32 = 1201;
/// Client `inventorycommon.const.groupCommandShip`.
const GROUP_COMMAND_SHIP: i32 = 540;
/// Client `inventorycommon.const.groupExpeditionCommandShip`.
const GROUP_EXPEDITION_COMMAND_SHIP: i32 = 4902;
/// Client `inventorycommon.const.groupBattleship`.
const GROUP_BATTLESHIP: i32 = 27;
/// Client `inventorycommon.const.groupBlackOps`.
const GROUP_BLACK_OPS: i32 = 898;
/// Client `inventorycommon.const.groupMarauders`.
const GROUP_MARAUDERS: i32 = 900;
/// Client `inventorycommon.const.groupCarrier`.
const GROUP_CARRIER: i32 = 547;
/// Client `inventorycommon.const.groupCommandCarrier`.
const GROUP_COMMAND_CARRIER: i32 = 5120;
/// Client `inventorycommon.const.groupCapitalIndustrialShip`.
const GROUP_CAPITAL_INDUSTRIAL_SHIP: i32 = 883;
/// Client `inventorycommon.const.groupDreadnought`.
const GROUP_DREADNOUGHT: i32 = 485;
/// Client `inventorycommon.const.groupTitan`.
const GROUP_TITAN: i32 = 30;
/// Client `inventorycommon.const.groupSupercarrier`.
const GROUP_SUPERCARRIER: i32 = 659;
/// Client `inventorycommon.const.groupForceAux`.
const GROUP_FORCE_AUX: i32 = 1538;
/// Client `inventorycommon.const.groupLancerDreadnought`.
const GROUP_LANCER_DREADNOUGHT: i32 = 4594;
/// Client `inventorycommon.const.groupBlockadeRunner`.
const GROUP_BLOCKADE_RUNNER: i32 = 1202;
/// Client `inventorycommon.const.groupExhumer`.
const GROUP_EXHUMER: i32 = 543;
/// Client `inventorycommon.const.groupFreighter`.
const GROUP_FREIGHTER: i32 = 513;
/// Client `inventorycommon.const.groupIndustrial`.
const GROUP_INDUSTRIAL: i32 = 28;
/// Client `inventorycommon.const.groupIndustrialCommandShip`.
const GROUP_INDUSTRIAL_COMMAND_SHIP: i32 = 941;
/// Client `inventorycommon.const.groupJumpFreighter`.
const GROUP_JUMP_FREIGHTER: i32 = 902;
/// Client `inventorycommon.const.groupMiningBarge`.
const GROUP_MINING_BARGE: i32 = 463;
/// Client `inventorycommon.const.groupTransportShip`.
const GROUP_TRANSPORT_SHIP: i32 = 380;
/// Client `eveCfg.GetShipGroupByClassType`: inventory-group membership.
const SHIP_CLASSES: &[(i64, &[i32])] = &[
    (CLASS_CAPSULES, &[GROUP_CAPSULE]),
    (CLASS_FRIGATES, &[GROUP_SHUTTLE, GROUP_ROOKIESHIP, GROUP_FRIGATE, GROUP_ASSAULT_FRIGATE, GROUP_COVERT_OPS, GROUP_ELECTRONIC_ATTACK_SHIPS, GROUP_PROTOTYPE_EXPLORATION_SHIP, GROUP_INTERCEPTOR, GROUP_STEALTH_BOMBER, GROUP_EXPEDITION_FRIGATE, GROUP_LOGISTICS_FRIGATE]),
    (CLASS_DESTROYERS, &[GROUP_DESTROYER, GROUP_INTERDICTOR, GROUP_TACTICAL_DESTROYER, GROUP_COMMAND_DESTROYER]),
    (CLASS_CRUISERS, &[GROUP_CRUISER, infinity_core::consts::GROUP_STRATEGIC_CRUISER as i32, GROUP_COMBAT_RECON_SHIP, GROUP_FORCE_RECON_SHIP, GROUP_HEAVY_ASSAULT_CRUISER, GROUP_HEAVY_INTERDICTORS, GROUP_LOGISTICS, GROUP_FLAG_CRUISER, GROUP_SPECIAL_EDITION_YACHT]),
    (CLASS_BATTLECRUISERS, &[GROUP_BATTLECRUISER, GROUP_ATTACK_BATTLECRUISER, GROUP_COMMAND_SHIP, GROUP_EXPEDITION_COMMAND_SHIP]),
    (CLASS_BATTLESHIPS, &[GROUP_BATTLESHIP, GROUP_BLACK_OPS, GROUP_MARAUDERS]),
    (CLASS_CAPITALSHIPS, &[GROUP_CARRIER, GROUP_COMMAND_CARRIER, GROUP_CAPITAL_INDUSTRIAL_SHIP, GROUP_DREADNOUGHT, GROUP_TITAN, GROUP_SUPERCARRIER, GROUP_FORCE_AUX, GROUP_LANCER_DREADNOUGHT]),
    (CLASS_INDUSTRIALS, &[GROUP_BLOCKADE_RUNNER, GROUP_EXHUMER, GROUP_FREIGHTER, GROUP_INDUSTRIAL, GROUP_INDUSTRIAL_COMMAND_SHIP, GROUP_JUMP_FREIGHTER, GROUP_MINING_BARGE, GROUP_TRANSPORT_SHIP]),
];

fn invalid() -> CallError { notify_error(messages::wars::INVALID) }
fn positive(value: &Value) -> Result<i64, CallError> {
    match value { Value::Int(id) if *id > 0 => Ok(*id), _ => Err(invalid()) }
}
fn bound_id(call: &Call<'_>) -> Result<i64, CallError> {
    if !call.bound { return Err(invalid()); }
    let value = call.bind_params.ok_or_else(invalid)?;
    match value {
        Value::Tuple(items) if items.len() == 1 => positive(&items[0]),
        Value::Int(_) => positive(value),
        _ => Err(invalid()),
    }
}
fn filters(args: &[Value], war: &War) -> Result<(Option<i64>, Option<i64>), CallError> {
    if args.len() != 2 { return Err(invalid()); }
    let entity = match &args[0] { Value::None => None, value => Some(positive(value)?) };
    if entity.is_some_and(|id| id != war.declared_by && id != war.against) { return Err(invalid()); }
    let class = match &args[1] {
        Value::None => None,
        Value::Int(id) if SHIP_CLASSES.iter().any(|(class, _)| class == id) || *id == CLASS_POS || *id == CLASS_STRUCTURES => Some(*id),
        _ => return Err(invalid()),
    };
    Ok((entity, class))
}
fn ship_class(group: i32, category: Option<i32>) -> Option<i64> {
    SHIP_CLASSES.iter().find_map(|(class, groups)| groups.contains(&group).then_some(*class))
        .or_else(|| match category { Some(CATEGORY_STARBASE) => Some(CLASS_POS), Some(CATEGORY_STRUCTURE) => Some(CLASS_STRUCTURES), _ => None })
}
fn matches_filter(row: &IndexedKillmail, entity: Option<i64>, class: Option<i64>, group: Option<(i32, Option<i32>)>) -> bool {
    entity.is_none_or(|id| row.projection.attacker_side == id)
        && class.is_none_or(|id| group.and_then(|(group, category)| ship_class(group, category)) == Some(id))
}
fn group_for(c: &Client, row: &IndexedKillmail) -> Option<(i32, Option<i32>)> {
    let ty = c.sde.types.get(row.killmail.victim_ship_type)?;
    Some((ty.group_id?, ty.category_id))
}

#[derive(Default)]
struct Totals { count: i64, isk: f64 }
#[derive(Default)]
struct GroupTotals { attacker: Totals, defender: Totals }
fn base_info(war: &War, rows: &[IndexedKillmail]) -> Value {
    let mut declared = Totals::default();
    let mut against = Totals::default();
    for row in rows {
        let total = if row.projection.attacker_side == war.declared_by { &mut declared } else if row.projection.attacker_side == war.against { &mut against } else { continue };
        total.count += 1;
        total.isk += row.killmail.isk_lost;
    }
    Value::tuple([
        wire::war(war),
        Value::list([Value::tuple([Value::Int(war.declared_by), Value::Int(declared.count)]), Value::tuple([Value::Int(war.against), Value::Int(against.count)])]),
        Value::list([Value::tuple([Value::Int(war.declared_by), Value::Float(declared.isk)]), Value::tuple([Value::Int(war.against), Value::Float(against.isk)])]),
        Value::Dict(vec![]),
    ])
}
fn grouped(war: &War, rows: &[IndexedKillmail], group: impl Fn(&IndexedKillmail) -> Option<i32>) -> Value {
    let mut groups: BTreeMap<i32, GroupTotals> = BTreeMap::new();
    for row in rows {
        let Some(group) = group(row) else { continue };
        let total = groups.entry(group).or_default();
        let side = if row.projection.defender_side == war.declared_by { &mut total.attacker } else if row.projection.defender_side == war.against { &mut total.defender } else { continue };
        side.count += 1;
        side.isk += row.killmail.isk_lost;
    }
    Value::Dict(groups.into_iter().map(|(group, total)| (Value::Int(i64::from(group)), Value::Dict(vec![
        (Value::str("defenderShipLoss"), Value::Int(total.defender.count)),
        (Value::str("attackerShipLoss"), Value::Int(total.attacker.count)),
        (Value::str("defenderIskLoss"), Value::Float(total.defender.isk)),
        (Value::str("attackerIskLoss"), Value::Float(total.attacker.isk)),
    ]))).collect())
}

pub(crate) async fn query(c: &Client, call: &Call<'_>) -> CallResult {
    let id = bound_id(call)?;
    let war = wars::get(&c.db, id).await.map_err(db_err)?.ok_or_else(invalid)?;
    let filter = if call.method == "GetKills" { Some(filters(call.args, &war)?) } else {
        if !call.args.is_empty() { return Err(invalid()); }
        None
    };
    let rows = crate::dao::killmails::wars::killmails(&c.db, id).await.map_err(db_err)?;
    match call.method {
        "GetBaseInfo" => Ok(base_info(&war, &rows)),
        "GetKillsByGroup" => Ok(grouped(&war, &rows, |row| group_for(c, row).map(|(group, _)| group))),
        "GetKills" => {
            let (entity, class) = filter.ok_or_else(invalid)?;
            Ok(Value::list(rows.into_iter().filter(|row| matches_filter(row, entity, class, if class.is_some() { group_for(c, row) } else { None })).map(|row| killmail_value(row.killmail, Some(id)))))
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
