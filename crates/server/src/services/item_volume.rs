//! Item volumes for packaged stacks and assembled items, mirroring
//! `inventorycommon/util.py:30-59` and its `inventorycommon/const.py` overrides.
//! Type, group and raw volume come from the loaded SDE. Plastic wraps use their
//! stored encoded quantity; creating or repairing that encoding is not supported here.

mod overrides;

use crate::{consts::TYPE_PLASTIC_WRAP, sde::Sde};
use super::CallError;

/// Client `GetItemVolume`: hundredths of a cubic metre encoded in wrap quantity.
const WRAP_VOLUME_SCALE: f64 = 100.0;
/// Client `GetItemVolume`: volume unavailable sentinel is not multiplied.
const UNAVAILABLE_VOLUME: f64 = -1.0;

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
    let count = qty.unwrap_or(if singleton { 1 } else { quantity });
    Ok(volume * if count < 0 { 1 } else { count } as f64)
}

#[cfg(test)] mod tests;
