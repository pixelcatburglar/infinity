//! Service-life damage reads and the pristine guard shared with repackaging
//! and mutation. `repairableShipMgr.GetServiceLifeDamage` answers the true
//! persisted damage for an owned, escrowed or legitimately viewable item;
//! `invInventory.ListServiceLifeDamage` and the bound charMgr global inventory
//! batch it per listed item. Recovery-band business IDs are refused before
//! any inventory API sees them.
//! Loss damage uses the SDE maximum reduction and the seed frozen with the
//! pending loss; each deterministic draw adds to existing service-life damage.

use std::collections::BTreeMap;
use infinity_core::messages::repairable_ship as msg;
use marshal::Value;
use crate::dao::items::loading::{Item, ItemQuery};
use crate::dao::items::service_life;
use crate::dao::repairable_ship::ItemId;
use crate::db::executor::Executor;
use crate::sde::Sde;
use crate::services::{db_err, items::load_items, notify_error, CallError, CallResult};
use crate::services::identity::Identity;
use crate::services::inventory::access::custody_allowed;

fn damage_map(rows: BTreeMap<ItemId, f64>) -> Value {
    Value::Dict(rows.into_iter().map(|(id, damage)| (Value::Int(id.get()), Value::Float(damage))).collect())
}

fn item_ids(items: &[Item]) -> Result<Vec<ItemId>, CallError> {
    items.iter().map(|item| ItemId::new(item.item_id)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("listed item {} is not a physical item", item.item_id)))).collect()
}

/// `{itemID: damage}` for the listed items; missing rows stay absent
/// (pristine), mirroring `repairableShipSvc._RequestServiceLifeDamageInv`.
pub(crate) async fn for_items(ex: &(impl Executor + ?Sized), items: &[Item]) -> Result<Value, CallError> {
    let ids = item_ids(items)?;
    Ok(damage_map(service_life::get_many(ex, &ids).await.map_err(db_err)?))
}

/// `GetServiceLifeDamage(itemID)`: the stored damage, `0` when no row exists.
/// Reads are allowed for the owner, the recovery beneficiary while the item
/// sits in escrow, or an existing corporate-custody viewer.
pub(crate) async fn single(ex: &(impl Executor + ?Sized), sde: &Sde, identity: &Identity, item: ItemId) -> CallResult {
    let id = item.get();
    let loaded = load_items(ex, sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| notify_error(msg::SELECTION_INVALID))?;
    let me = identity.char_id;
    let allowed = loaded.owner_id == me
        || crate::dao::repairable_ship::escrow_beneficiary(ex, item).await.map_err(db_err)? == Some(me)
        || custody_allowed(ex, sde, id, me, identity.station_id, false).await? == Some(true);
    if !allowed { return Err(notify_error(msg::SELECTION_INVALID)); }
    let damage = service_life::get_many(ex, &[item]).await.map_err(db_err)?.get(&item).copied().unwrap_or(0.0);
    Ok(Value::Float(damage))
}

/// `charMgr.ListServiceLifeDamage` on the bound global inventory: every item
/// the character still owns, wherever it sits.
pub(crate) async fn global(ex: &(impl Executor + ?Sized), sde: &Sde, character: i64) -> CallResult {
    let items = load_items(ex, sde, ItemQuery::OwnerExceptSelf(character)).await.map_err(db_err)?;
    for_items(ex, &items).await
}

/// Repackaging and mutation refuse damaged service-life inputs; an absent row
/// or a stored zero is pristine.
pub(crate) async fn require_pristine(ex: &(impl Executor + ?Sized), ids: &[ItemId]) -> Result<(), CallError> {
    if service_life::get_many(ex, ids).await.map_err(db_err)?.values().any(|damage| *damage > 0.0) {
        return Err(notify_error(msg::DAMAGED_OPERATION));
    }
    Ok(())
}

/// The Macho `{itemID: damage}` dict consumed by
/// `repairableShipSvc.OnItemServiceLifeDamageChanged`; keys stay integer
/// inventory IDs, never recovery IDs.
pub(crate) fn damage_dict(changes: &BTreeMap<ItemId, f64>) -> Value {
    Value::Dict(changes.iter().map(|(&id, &damage)| (Value::Int(id.get()), Value::Float(damage))).collect())
}
/// `repairableShip/common/methods.py:CalDamageLife` uses the deduction from
/// `repairableShip/common/const.py:REDUCE_SERVICE_LIFE_DEDUCTED`.
const REDUCE_DEDUCTION: f64 = 20.0;
/// The lower and upper random bounds and quadratic factor in `CalDamageLife`.
const LOW_LIFE_RANDOM_MAX: f64 = 100.0;
const LOW_LIFE_QUADRATIC: f64 = 0.002;

/// Damage is deterministic for one pending loss and item, including retries.
pub(super) fn loss_damage(seed: u64, item: ItemId, prior: f64, max_life: f64, max_reduce: f64) -> f64 {
    let mut x = seed ^ (item.get() as u64);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    let uniform = (x >> 11) as f64 / ((1_u64 << 53) as f64);
    let increment = if max_life - prior > max_reduce {
        (max_reduce - REDUCE_DEDUCTION + REDUCE_DEDUCTION * uniform).trunc()
    } else {
        let value = (1.0 + (LOW_LIFE_RANDOM_MAX - 1.0) * uniform).trunc();
        max_reduce - (value * value * LOW_LIFE_QUADRATIC).trunc()
    };
    (prior + increment.clamp(0.0, max_life)).clamp(0.0, max_life)
}


#[cfg(all(test, feature = "tests-services"))] mod tests;
