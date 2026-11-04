//! Lua inventory and market operations backed by the inventory and market services.
//! Mirrors invInventory and marketProxy; game items and orders are authoritative.
//! Inventory and market operations require an online pilot.
use mlua::{Lua, Table};
use crate::{dao::items::loading::ItemQuery, messages, services::{self, effects, inventory::{access::Container, movement}, market}};
use super::{lifecycle, lua_error, mutate, positive, space::actual_system, Resources};

pub(super) fn register(lua: &Lua, api: &Table, resources: &Resources) -> mlua::Result<()> {
    let r = resources.clone();
    api.set("move_item", lua.create_async_function(move |_, (id, item, container, flag, qty): (i64, i64, i64, Option<i64>, Option<i64>)| {
        let r = r.clone(); async move {
            positive(id)?; positive(item)?; positive(container)?;
            let flag = flag.map(|v| {
                if v < 0 { return Err(mlua::Error::runtime("invalid flag")); }
                i32::try_from(v).map_err(|_| mlua::Error::runtime("invalid flag"))
            }).transpose()?;
            if let Some(q) = qty { if q <= 0 || i32::try_from(q).is_err() { return Err(mlua::Error::runtime("invalid quantity")); } }
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                let pilot = state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                let destination = if identity.station_id == Some(container) { Container::Hangar { station_id: container, owner_id: id } } else { Container::Item(container) };
                movement::move_item(&identity, &r.db, &r.world, &r.sde, system, pilot.notify.clone(), &destination, item, None, flag, qty,
                    |n| effects::online::dispatch_notification(n, &r.world, &pilot.notify, Some(id))).await?;
                Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("sell", lua.create_async_function(move |_, (id, item, station, price, quantity, duration): (i64, i64, i64, f64, i64, i64)| {
        let r = r.clone(); async move {
            positive(id)?; positive(item)?; positive(station)?;
            if !price.is_finite() || price <= 0.0 || quantity <= 0 || i32::try_from(quantity).is_err() { return Err(mlua::Error::runtime("invalid price or quantity")); }
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                let _pilot = state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let item_row = services::items::load_items(&r.db, &r.sde, ItemQuery::Id(item)).await.map_err(services::db_err)?
                    .into_iter().next().ok_or_else(|| services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                market::place_sell(&identity, &r.db, &r.world, &r.sde, item, station, i64::from(item_row.type_id), price, quantity, duration).await
            }).await.map_err(lua_error)
        }
    })?)?;
    Ok(())
}
