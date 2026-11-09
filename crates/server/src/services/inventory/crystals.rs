//! Batch crystal damage reads for `invbroker.GetDamageForCrystals`.
//! Mirrors `eve/client/script/environment/inventorysvc.py` and
//! `itemDamage.py`; reads item rows, SDE dogma bases and persisted dogma
//! overrides. Shared bays require current physical and owner-bound authorization;
//! corporate ancestry uses custody roles. Crystal mutation and loss are not implemented.

use std::collections::HashMap;

use crate::client::Client;
use infinity_core::consts::dogma::{ATTR_CRYSTAL_DAMAGE_ENABLED, ATTR_HULL_DAMAGE};
use crate::dao::items::{dogma, loading::ItemQuery};
use crate::services::{db_err, fit, items::load_items, value_id_list, Call, CallError};
use crate::services::inventory::access::{corporate_custody, may_view_corp_hangar};
use super::reachability::{Endpoint, Reachability};
use crate::services::identity::Identity;
use marshal::Value;

/// Return the current native damage ratio for visible, damageable crystals.
pub(super) async fn get_damage(c: &Client, call: &Call<'_>) -> Result<Value, CallError> {
    let ids = value_id_list(call.args.first());
    if ids.is_empty() {
        return Ok(Value::Dict(vec![]));
    }
    let rows = load_items(&c.db, &c.sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
    let me = c.session.int("charid").unwrap_or(0);
    let rows: Vec<_> = rows.into_iter().filter(|row| {
        let Some(ty) = c.sde.types.get(row.type_id) else { return false };
        let enabled = fit::base_attribute(Some(ty), c.sde.dogma.attribute(ATTR_CRYSTAL_DAMAGE_ENABLED), ATTR_CRYSTAL_DAMAGE_ENABLED);
        enabled.is_finite() && enabled > 0.0
    }).collect();
    let mut scopes = HashMap::new();
    let mut endpoints = Vec::new();
    for row in &rows {
        let key = (row.location_id, row.flag_id);
        if !scopes.contains_key(&key) {
            scopes.insert(key, endpoints.len());
            endpoints.push(Endpoint { location: key.0, flag: key.1 });
        }
    }
    let identity = Identity::of_client(c);
    let reach = Reachability::capture(&identity, &c.db, &c.world, identity.system_id, &endpoints).await?;
    let checked = reach.inspect_many(&c.db, &c.sde, &endpoints).await?;
    let mut visible = Vec::with_capacity(rows.len());
    for row in rows {
        let index = scopes[&(row.location_id, row.flag_id)];
        if !checked.admitted[index] { continue; }
        let endpoint = endpoints[index];
        let shared = checked.lineage(endpoint.location).is_some_and(|lineage|
            super::movement::shared_lineage(&c.sde, endpoint, lineage.len(), |id| lineage.get(&id)));
        let authorized = if shared && identity.station_id.is_none() {
            true
        } else if row.owner_id == me {
            true
        } else if let Some((corp, location, flag)) = corporate_custody(&c.db, Some(c.sde.as_ref()), row.item_id).await? {
            let station = super::access::custody_station(&c.db, location, flag).await?;
            if let Some(station) = station {
                identity.station_id == Some(station) && may_view_corp_hangar(&c.db, me, corp, station, flag).await?
            } else { false }
        } else {
            false
        };
        if authorized { visible.push(row); }
    }
    if visible.is_empty() {
        return Ok(Value::Dict(vec![]));
    }
    let selected: Vec<_> = visible.iter().map(|row| row.item_id).collect();
    let overrides = dogma::overrides_for(&c.db, &selected).await.map_err(db_err)?;
    let mut damage = HashMap::with_capacity(overrides.len());
    for value in overrides {
        if value.attribute_id == ATTR_HULL_DAMAGE { damage.insert(value.item_id, value.value); }
    }
    let mut result = Vec::with_capacity(visible.len());
    for row in visible {
        let base = fit::base_attribute(c.sde.types.get(row.type_id), c.sde.dogma.attribute(ATTR_HULL_DAMAGE), ATTR_HULL_DAMAGE);
        let value = damage.get(&row.item_id).copied().unwrap_or(base);
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(CallError::Internal(anyhow::anyhow!("invalid crystal damage for item {}", row.item_id)));
        }
        result.push((Value::Int(row.item_id), Value::Float(value)));
    }
    Ok(Value::Dict(result))
}

#[cfg(test)]
mod tests;
