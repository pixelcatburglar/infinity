//! Effect notifications consumed by client dogma and fxSequencer.
//! Payloads use runtime effect and item data; EWAR slots follow client
//! dogma/effects/GetEwarTypeByEffectID and buffBarConst. Jam start times are
//! FILETIME and durations are milliseconds. Scheduling and delivery are elsewhere.
//! Scan result stack quantities are projected to the client display limit;
//! scanned inventory stays exact.

use crate::client::filetime_now;
use crate::services::combat::Ctx;
use crate::space::event;
use marshal::Value;

use super::environment;

/// Michelle.OnDroneStateChange's authoritative seven positional arguments.
pub(crate) fn drone_state(drone: i64, owner: Option<i64>, controller: Option<i64>, state: i32, type_id: i32, controller_owner: Option<i64>, target: Option<i64>) -> Vec<Value> {
    vec![Value::Int(drone), owner.map_or(Value::None, Value::Int), controller.map_or(Value::None, Value::Int),
        Value::Int(i64::from(state)), Value::Int(i64::from(type_id)), controller_owner.map_or(Value::None, Value::Int), target.map_or(Value::None, Value::Int)]
}

/// Michelle.OnDroneActivityChange carries assist/guard metadata.
pub(crate) fn drone_activity(drone: i64, character: Option<i64>, activity: Option<&str>) -> Vec<Value> {
    vec![Value::Int(drone), character.map_or(Value::None, Value::Int), activity.map_or(Value::None, Value::str)]
}

/// Client dogma.OnDroneControlLost unloads the drone's dogma item.
pub(crate) fn drone_control_lost(drone: i64) -> Vec<Value> { vec![Value::Int(drone)] }

/// Client dogma/effects/GetEwarTypeByEffectID, including chance-flag precedence.
pub(crate) fn ewar_slot(effect: &super::registry::Effect) -> Option<&'static str> {
    use super::registry::EffectFlag;
    if effect.has(EffectFlag::ElectronicChance) { return Some("electronic"); }
    if effect.has(EffectFlag::PropulsionChance) { return Some("webify"); }
    ewar_slot_by_id(i32::try_from(effect.id).ok()?)
}

/// Client dogma/effects/BUFF_SLOTS_BY_EFFECT_ID for the supported effects.
fn ewar_slot_by_id(id: i32) -> Option<&'static str> {
    use infinity_core::consts::dogma::*;
    Some(match id {
        EFFECT_REMOTE_ECM_FALLOFF | EFFECT_ENTITY_ECM_FALLOFF | EFFECT_NPC_BEHAVIOR_ECM => "electronic",
        EFFECT_REMOTE_WEBIFIER_FALLOFF | EFFECT_REMOTE_WEBIFIER_ENTITY | EFFECT_NPC_BEHAVIOR_WEB => "webify",
        EFFECT_WARP_DISRUPT | EFFECT_NPC_BEHAVIOR_WARP_DISRUPT | EFFECT_NPC_WARP_SCRAMBLE => "warpScrambler",
        EFFECT_WARP_SCRAMBLE | EFFECT_NPC_BEHAVIOR_WARP_SCRAMBLE | EFFECT_NPC_WARP_SCRAMBLE_MWD => "warpScramblerMWD",
        EFFECT_REMOTE_TARGET_PAINT_FALLOFF | EFFECT_REMOTE_TARGET_PAINT_ENTITY | EFFECT_NPC_BEHAVIOR_PAINTER => "ewTargetPaint",
        EFFECT_REMOTE_SENSOR_DAMP_FALLOFF | EFFECT_REMOTE_SENSOR_DAMP_ENTITY | EFFECT_NPC_BEHAVIOR_DAMP => "ewRemoteSensorDamp",
        EFFECT_TRACKING_DISRUPTOR | EFFECT_NPC_WEAPON_DISRUPTOR | EFFECT_NPC_ENTITY_TRACKING_DISRUPTOR | EFFECT_NPC_BEHAVIOR_TRACKING_DISRUPTOR => "ewTrackingDisrupt",
        EFFECT_GUIDANCE_DISRUPTOR | EFFECT_NPC_BEHAVIOR_GUIDANCE_DISRUPTOR => "ewGuidanceDisrupt",
        EFFECT_REMOTE_SENSOR_BOOST => "sensorBooster",
        EFFECT_REMOTE_TRACKING_COMPUTER => "remoteTracking",
        EFFECT_ENERGY_NEUTRALIZER_FALLOFF | EFFECT_ENTITY_ENERGY_NEUTRALIZER_FALLOFF | EFFECT_NPC_BEHAVIOR_NEUTRALIZER => "ewEnergyNeut",
        EFFECT_ENERGY_NOSFERATU_FALLOFF | EFFECT_NPC_BEHAVIOR_NOSFERATU => "ewEnergyVampire",
        _ => return None,
    })
}

/// Tactical.OnEwarStart/End(sourceBallID, moduleID, targetBallID, jammingType).
pub(crate) fn ewar(source: i64, module: i64, target: i64, slot: &'static str, start: bool) -> (&'static str, Vec<Value>) {
    (if start { "OnEwarStart" } else { "OnEwarEnd" },
        vec![Value::Int(source), Value::Int(module), Value::Int(target), Value::str(slot)])
}

/// godma.OnJamStart/End identifies each jam by source, module, target and type.
pub(crate) fn jam(source: i64, module: i64, target: i64, start: bool, started: i64, duration_ms: f64) -> (&'static str, Vec<Value>) {
    let mut args = vec![Value::Int(source), Value::Int(module), Value::Int(target), Value::str("electronic")];
    if start { args.extend([Value::Int(started), Value::Float(duration_ms)]); }
    (if start { "OnJamStart" } else { "OnJamEnd" }, args)
}

/// OnGodmaShipEffect(itemID, effectID, time, start, active, environment, startTime,
/// duration, repeat, error, actualStopTime).
#[allow(clippy::too_many_arguments)]
pub(in crate::services) fn ship_effect(cx: &Ctx, module: i64, effect_id: i64, target: Option<i64>, other: Option<Value>, start: bool, started: i64, duration_ms: f64, repeat: i64) {
    let now = filetime_now();
    cx.notify("OnGodmaShipEffect", ship_effect_args(module, effect_id, environment::build(module, cx.char_id, cx.ship_id, target, other, effect_id), start, started, duration_ms, repeat, now));
}

#[allow(clippy::too_many_arguments)]
pub(in crate::services) fn ship_effect_args(module: i64, effect_id: i64, environment: Value, start: bool, started: i64, duration_ms: f64, repeat: i64, now: i64) -> Vec<Value> {
    vec![
        Value::Int(module),
        Value::Int(effect_id),
        Value::Int(now),
        Value::Int(start as i64),
        Value::Int(start as i64),
        environment,
        Value::Int(started),
        Value::Float(duration_ms),
        Value::Int(repeat),
        Value::None,
        if start { Value::None } else { Value::Int(now) },
    ]
}

fn scan_rows(rows: Vec<(i32, i64)>) -> Value {
    Value::list(rows.into_iter().map(|(type_id, quantity)| Value::tuple([Value::Int(i64::from(type_id)), Value::Int(crate::services::dogma::info::cap_client_quantity(quantity))])))
}

fn cargo_scan_args(target: i64, rows: Vec<(i32, i64)>) -> Vec<Value> {
    vec![Value::Int(target), scan_rows(rows)]
}

fn ship_scan_args(target: i64, charge: f64, capacity: f64, rows: Vec<(i32, i64)>) -> Vec<Value> {
    vec![Value::Int(target), Value::Float(charge), Value::Float(capacity), scan_rows(rows)]
}

/// shipUI.OnCargoScanComplete(shipID, cargoList).
pub(in crate::services) fn cargo_scan_complete(cx: &Ctx, target: i64, rows: Vec<(i32, i64)>) {
    cx.notify("OnCargoScanComplete", cargo_scan_args(target, rows));
}

/// shipUI.OnShipScanCompleted(shipID, charge, capacity, hardwareList).
pub(in crate::services) fn ship_scan_completed(cx: &Ctx, target: i64, charge: f64, capacity: f64, rows: Vec<(i32, i64)>) {
    cx.notify("OnShipScanCompleted", ship_scan_args(target, charge, capacity, rows));
}

/// A weapon firing animation (fxSequencer.OnSpecialFX) from `shooter`.
/// `is_offensive` precedes the start flag and comes from the source effect.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fire_fx(stamp: i64, shooter: i64, module: Option<(i64, i32)>, guid: &str, target: Option<i64>, charge: Option<i32>, is_offensive: bool, start: bool, duration_ms: f64, repeat: i64) -> Value {
    fire_fx_with_info(stamp, shooter, module, guid, target, charge, is_offensive, start, duration_ms, repeat, Value::None)
}

/// As `fire_fx`, with client-specific graphic information.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fire_fx_with_info(stamp: i64, shooter: i64, module: Option<(i64, i32)>, guid: &str, target: Option<i64>, charge: Option<i32>, is_offensive: bool, start: bool, duration_ms: f64, repeat: i64, graphic_info: Value) -> Value {
    event(
        stamp,
        "OnSpecialFX",
        vec![
            Value::Int(shooter),
            module.map(|m| Value::Int(m.0)).unwrap_or(Value::None),
            module.map(|m| Value::Int(m.1 as i64)).unwrap_or(Value::None),
            target.map(Value::Int).unwrap_or(Value::None),
            charge.map(|t| Value::Int(t as i64)).unwrap_or(Value::None),
            Value::str(guid),
            Value::Bool(is_offensive),
            Value::Int(start as i64),
            Value::Int(start as i64),
            Value::Float(duration_ms),
            Value::Int(repeat),
            Value::None,
            Value::Int(0),
            graphic_info,
        ],
    )
}

/// One `OnModuleAttributeChanges` entry.
pub(crate) fn attribute_change(owner: i64, item: Value, attribute: i32, new: Value, old: Value) -> Value {
    attribute_change_at(owner, item, attribute, new, old, filetime_now())
}

fn attribute_change_at(owner: i64, item: Value, attribute: i32, new: Value, old: Value, time: i64) -> Value {
    Value::tuple([
        Value::str("OnModuleAttributeChange"),
        Value::Int(owner),
        item,
        Value::Int(attribute as i64),
        Value::Int(time),
        new,
        old,
        Value::None,
    ])
}

/// Client dogma heat-source modifier connection event.
pub(crate) fn heat_connection(heat_id: i32, module: i64, start: bool) -> (&'static str, Vec<Value>) {
    (if start { "OnHeatAdded" } else { "OnHeatRemoved" }, vec![Value::Int(i64::from(heat_id)), Value::Int(module)])
}

/// Attribute change for an actor-owned rack.
pub(crate) fn heat_attribute(owner: i64, ship: i64, attribute: i32, old: f64, new: f64, time: i64) -> (&'static str, Vec<Value>) {
    ("OnModuleAttributeChanges", vec![Value::list([attribute_change_at(owner, Value::Int(ship), attribute, Value::Float(new), Value::Float(old), time)])])
}

/// Category-5 effect notification without a live service context.
pub(crate) fn overload_effect(owner: i64, ship: i64, module: i64, effect: i32, start: bool) -> (&'static str, Vec<Value>) {
    let now = filetime_now();
    let environment = environment::build(module, owner, ship, None, None, i64::from(effect));
    ("OnGodmaShipEffect", ship_effect_args(module, i64::from(effect), environment, start, now, -1.0, -1, now))
}

#[cfg(test)] mod tests;
