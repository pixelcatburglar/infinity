//! Station jump-clone installation and access policy for clonejumpsvc.py.
//! Uses effective dogma, station services, standings and character wallet data.
//! Pirate militia enrollment is not modelled; the Fulcrum is therefore unavailable.

use std::sync::Arc;
use marshal::Value;
use infinity_core::{consts::{dogma::{ATTR_CLONE_JUMP_COOLDOWN, ATTR_MAX_JUMP_CLONES}, UE_LOCID}, messages};
use crate::{client::filetime_ticks_from_hours, dao::characters::clones as clone_dao, db::executor::Executor, sde::Sde, services::{db_err, notify_error, user_error, CallError, fit::{Fit, Key}, identity::Identity, standings, wallets}};

/// appConst.py stationServiceCloning and stationServiceJumpCloneFacility.
/// Server NPC installation accepts either service; access checks still use the jump-clone service.
pub(super) const CLONING_SERVICE: i32 = 10;
pub(super) const JUMP_CLONE_FACILITY: i32 = 24;
/// inventorycommon.const.stationFulcrumZarzakh.
const FULCRUM_ZARZAKH: i64 = 60015187;

pub(super) fn service_fee() -> f64 { crate::config::game().clones.service_fee_isk }

/// Decimal precision used by evestations/standingsrestriction.py.
const STANDING_SCALE: u128 = 100;
/// IEEE 754 binary64 fraction width and exponent bias.
const F64_FRACTION_BITS: u32 = 52;
const F64_EXPONENT_BIAS: i32 = 1023;
const F64_EXPONENT_MASK: u64 = 0x7ff;

fn rounded_standing(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 { return value; }
    let bits = value.abs().to_bits();
    let exponent = ((bits >> F64_FRACTION_BITS) & F64_EXPONENT_MASK) as i32;
    let fraction = bits & ((1_u64 << F64_FRACTION_BITS) - 1);
    let (mantissa, power) = if exponent == 0 {
        (fraction, 1 - F64_EXPONENT_BIAS - F64_FRACTION_BITS as i32)
    } else {
        (fraction | (1_u64 << F64_FRACTION_BITS), exponent - F64_EXPONENT_BIAS - F64_FRACTION_BITS as i32)
    };
    if power >= 0 { return value; }
    let shift = (-power) as u32;
    let scaled = u128::from(mantissa) * STANDING_SCALE;
    let rounded = if shift >= u128::BITS {
        0
    } else {
        let quotient = scaled >> shift;
        let remainder = scaled & ((1_u128 << shift) - 1);
        quotient + u128::from(remainder >= (1_u128 << (shift - 1)))
    };
    ((rounded as f64) / STANDING_SCALE as f64).copysign(value)
}

pub(super) async fn check_station_access_tx(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, owner: i64, station: i64) -> Result<(), CallError> {
    let station_data = sde.map.station(station).ok_or_else(|| notify_error(messages::JUMP_CLONE_NOT_FOUND))?;
    if station == FULCRUM_ZARZAKH {
        return Err(user_error(messages::clones::MSG_NOT_IN_RIGHT_MILITIA_UNABLE_TO_USE_SERVICE, vec![("stationName".into(), Value::tuple([Value::Int(UE_LOCID), Value::Int(station)]))]));
    }
    let faction = station_data.solar_system_id.and_then(|system| sde.map.system(system)).and_then(|system| system.faction_id);
    for from in [station_data.owner_id, faction].into_iter().flatten() {
        if let Some(required) = sde.map.required_station_standing(from, JUMP_CLONE_FACILITY) {
            let current = standings::standing(ex, owner, from).await?;
            if !current.is_finite() || !required.is_finite() {
                return Err(db_err(anyhow::anyhow!("invalid station standing")));
            }
            let current = rounded_standing(current);
            let required = rounded_standing(required);
            if current < required {
                return Err(user_error(messages::clones::MSG_STANDINGS_RESTRICTION_GENERIC, vec![("from_id".into(), Value::Int(from)), ("to_id".into(), Value::Int(owner)), ("current_standing".into(), Value::Float(current)), ("required_standing".into(), Value::Float(required))]));
            }
        }
    }
    Ok(())
}

fn cooldown_ready(now: i64, last: Option<i64>, hours: f64) -> Result<bool, CallError> {
    if !hours.is_finite() || hours < 0.0 { return Err(db_err(anyhow::anyhow!("invalid effective clone cooldown"))); }
    let ticks = filetime_ticks_from_hours(hours.round()).ok_or_else(|| db_err(anyhow::anyhow!("effective clone cooldown overflows")))?;
    Ok(last.is_none_or(|last| i128::from(now) >= i128::from(last) + i128::from(ticks)))
}

pub(super) async fn check_cooldown_tx(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, owner: i64, now: i64, last: Option<i64>, same_station: bool) -> Result<(), CallError> {
    if same_station || last.is_none() { return Ok(()); }
    let fit = Fit::load_character_at(ex, Arc::clone(sde), owner, now).await?;
    if cooldown_ready(now, last, fit.value(Key::Char, ATTR_CLONE_JUMP_COOLDOWN))? { Ok(()) } else { Err(notify_error(messages::clones::COOLDOWN_ACTIVE)) }
}

enum Issue { Docked, Bay, Capacity, Skill, Funds(crate::isk::IskAmount, crate::isk::IskAmount), Duplicate, Access(CallError, bool) }
impl Issue {
    fn label(&self) -> Value {
        let label = match self {
            Self::Docked => messages::clones::VALIDATION_DOCKED,
            Self::Bay => messages::clones::VALIDATION_BAY,
            Self::Capacity => messages::clones::VALIDATION_CAPACITY,
            Self::Skill => messages::clones::VALIDATION_SKILL,
            Self::Funds(_, _) => messages::clones::VALIDATION_FUNDS,
            Self::Duplicate => messages::clones::VALIDATION_DUPLICATE,
            Self::Access(_, true) => messages::clones::VALIDATION_OPPONENT,
            Self::Access(_, false) => messages::clones::VALIDATION_STANDINGS,
        };
        match self { Self::Funds(fee, _) => Value::tuple([Value::str(label), Value::dict([("cloneCost", Value::Float(fee.wire_isk()))])]), _ => Value::str(label) }
    }
    fn error(self) -> CallError {
        match self {
            Self::Docked => user_error(messages::MSG_NOT_AT_STATION, vec![]),
            Self::Bay => notify_error(messages::clones::NO_CLONE_BAY),
            Self::Capacity | Self::Skill => notify_error(messages::clones::CAPACITY_REACHED),
            Self::Funds(fee, balance) => wallets::not_enough_money(balance, fee),
            Self::Duplicate => notify_error(messages::JUMP_CLONE_ALREADY_AT_STATION),
            Self::Access(error, _) => error,
        }
    }
}

async fn issues(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, id: &Identity, now: i64) -> Result<Vec<Issue>, CallError> {
    let character = clone_dao::lock_character(ex, id.char_id).await.map_err(db_err)?.ok_or_else(|| notify_error(messages::JUMP_CLONE_NOT_FOUND))?;
    let clones = clone_dao::lock_clones(ex, id.char_id).await.map_err(db_err)?;
    let mut issues = Vec::new();
    let station = character.station_id.filter(|station| Some(*station) == id.station_id);
    if let Some(station) = station {
        match sde.map.station(station) {
            Some(data) if sde.industry.has_service(data.operation_id, CLONING_SERVICE) || sde.industry.has_service(data.operation_id, JUMP_CLONE_FACILITY) => {},
            _ => issues.push(Issue::Bay),
        }
        if clones.iter().any(|clone| clone.location_id == station) { issues.push(Issue::Duplicate); }
        if sde.map.station(station).is_some() {
            if let Err(error) = check_station_access_tx(ex, sde, id.char_id, station).await {
                match error { CallError::User { .. } => issues.push(Issue::Access(error, station == FULCRUM_ZARZAKH)), error => return Err(error) }
            }
        }
    } else { issues.push(Issue::Docked); }
    let fit = crate::services::skills::body_change::projected_fit_tx(ex, sde, id.char_id, now).await?;
    let capacity = fit.value(Key::Char, ATTR_MAX_JUMP_CLONES);
    if !capacity.is_finite() || capacity < 0.0 { return Err(db_err(anyhow::anyhow!("invalid effective clone capacity"))); }
    let capacity = capacity.trunc() as usize;
    if capacity == 0 { issues.push(Issue::Skill); }
    else if clones.len() >= capacity { issues.push(Issue::Capacity); }
    let fee = crate::isk::IskAmount::from_isk(service_fee())?;
    let balance = wallets::balance(ex, id.char_id).await?;
    if balance < fee { issues.push(Issue::Funds(fee, balance)); }
    Ok(issues)
}

pub(super) async fn validate_install_tx(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, id: &Identity, now: i64) -> Result<Vec<Value>, CallError> {
    Ok(issues(ex, sde, id, now).await?.iter().map(Issue::label).collect())
}
pub(super) async fn ensure_install_tx(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, id: &Identity, now: i64) -> Result<(), CallError> {
    if let Some(issue) = issues(ex, sde, id, now).await?.into_iter().next() { Err(issue.error()) } else { Ok(()) }
}

#[cfg(test)] mod tests;
