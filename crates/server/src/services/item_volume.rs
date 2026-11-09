//! Item volumes for packaged stacks and assembled items, mirroring
//! `inventorycommon/util.py:30-59` and its `inventorycommon/const.py` overrides.
//! Type, group and raw volume come from the loaded SDE. Plastic wrap quantity
//! encoding is supported here; persistence is the contract DAO and caller's concern.

mod overrides;

use infinity_core::consts::TYPE_PLASTIC_WRAP;
use crate::sde::Sde;
use super::CallError;

/// Client `GetItemVolume`: hundredths of a cubic metre encoded in wrap quantity.
const WRAP_VOLUME_SCALE: f64 = 100.0;
/// Client `GetItemVolume`: volume unavailable sentinel is not multiplied.
const UNAVAILABLE_VOLUME: f64 = -1.0;

/// Logical item count, independent of singleton wire quantity encoding.
pub(super) fn stack_size(singleton: bool, quantity: i64) -> i64 {
    if singleton { 1 } else { quantity }
}

/// Encode a plastic wrap volume for the client `inventorycommon/util.py:45-48`
/// decoder. Rounding up and the minimum magnitude are server representation choices;
/// the encoded magnitude remains limited to 21,474,836.47 m³ even though
/// the stored quantity is i64. Courier creation separately enforces the client's
/// 1,200,000 m³ limit.
pub(super) fn encode_wrap_quantity(volume: f64) -> Result<i64, super::CallError> {
    if !volume.is_finite() || volume < 0.0 {
        return Err(anyhow::anyhow!("invalid plastic wrap volume {volume}").into());
    }
    let magnitude = (volume * WRAP_VOLUME_SCALE).ceil().max(1.0);
    if magnitude > i32::MAX as f64 {
        return Err(anyhow::anyhow!("plastic wrap volume exceeds encoded range").into());
    }
    Ok(-(magnitude as i64))
}

pub(super) fn item_volume(sde: &Sde, type_id: i32, singleton: bool, quantity: i64, qty: Option<i64>) -> Result<f64, CallError> {
    let ty = sde.types.get(type_id).ok_or_else(|| anyhow::anyhow!("missing SDE type {type_id}"))?;
    let volume = ty.volume.ok_or_else(|| anyhow::anyhow!("missing SDE volume for type {type_id}"))?;
    calculate(type_id, ty.group_id, volume, singleton, quantity, qty)
}

fn calculate(type_id: i32, group_id: Option<i32>, raw_volume: f64, singleton: bool, quantity: i64, qty: Option<i64>) -> Result<f64, CallError> {
    let volume = if singleton {
        if type_id == TYPE_PLASTIC_WRAP {
            let volume = -(quantity as f64) / WRAP_VOLUME_SCALE;
            if volume <= 0.0 {
                return Err(anyhow::anyhow!("plastic wrap volume must be positive").into());
            }
            volume
        } else {
            raw_volume
        }
    } else {
        // Both override tables are strictly sorted by ID for binary search.
        overrides::TYPE_OVERRIDES.binary_search_by_key(&type_id, |(id, _)| *id).ok().map(|index| overrides::TYPE_OVERRIDES[index].1)
            .or_else(|| group_id.and_then(|group| overrides::GROUP_OVERRIDES.binary_search_by_key(&group, |(id, _)| *id).ok().map(|index| overrides::GROUP_OVERRIDES[index].1)))
            .unwrap_or(raw_volume)
    };
    if volume == UNAVAILABLE_VOLUME {
        return Ok(volume);
    }
    let count = qty.unwrap_or(stack_size(singleton, quantity));
    Ok(volume * if count < 0 { 1 } else { count } as f64)
}

#[cfg(test)] mod tests;
