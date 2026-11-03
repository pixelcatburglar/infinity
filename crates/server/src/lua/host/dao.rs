//! Script access to existing game data and service operations. Observers read
//! items and station-local market orders without a context; writes use the
//! invocation's client pump and service policies. No arbitrary SQL is exposed.

use mlua::{Lua, Result, Table};
use crate::{dao::{characters::{crimewatch, routing, selection, social, standings}, items::loading::ItemQuery, skills::levels}, db::Db, sde::Sde};
use std::sync::Arc;
use super::client::{lua_error, mutate, Mutation};

fn positive(id: i64) -> Result<i64> {
    if id > 0 { Ok(id) } else { Err(mlua::Error::runtime("identifier must be positive")) }
}

fn amount(value: f64) -> Result<f64> {
    if value.is_finite() && value > 0.0 { Ok(value) } else { Err(mlua::Error::runtime("amount must be finite and positive")) }
}

fn item_rows(lua: &Lua, items: Vec<crate::dao::items::loading::Item>) -> Result<Table> {
    let rows = lua.create_table()?;
    for (index, item) in items.into_iter().enumerate() {
        let row = lua.create_table()?;
        row.set("item_id", item.item_id)?;
        row.set("type_id", item.type_id)?;
        row.set("owner_id", item.owner_id)?;
        row.set("location_id", item.location_id)?;
        row.set("flag_id", item.flag_id)?;
        row.set("quantity", item.quantity)?;
        row.set("singleton", item.singleton)?;
        row.set("group_id", item.group_id)?;
        row.set("category_id", item.category_id)?;
        row.set("custom_info", item.custom_info)?;
        rows.set(index + 1, row)?;
    }
    Ok(rows)
}

fn ids(table: Table) -> Result<Vec<i64>> {
    let mut entries = Vec::new();
    for entry in table.pairs::<mlua::Value, i64>() {
        let (key, value) = entry?;
        let mlua::Value::Integer(index) = key else { return Err(mlua::Error::runtime("expected contiguous recipient list")); };
        entries.push((index, positive(value)?));
    }
    entries.sort_unstable_by_key(|entry| entry.0);
    if entries.iter().enumerate().any(|(i, (index, _))| *index != (i + 1) as i64) {
        return Err(mlua::Error::runtime("expected contiguous recipient list"));
    }
    Ok(entries.into_iter().map(|(_, id)| id).collect())
}

pub(super) fn register(lua: &Lua, env: &Table, db: &Db, sde: &Arc<Sde>) -> Result<()> {
    let dao = lua.create_table()?;
    let wallets = lua.create_table()?;
    let handle = db.clone();
    wallets.set("balance", lua.create_async_function(move |_, character: i64| {
        let handle = handle.clone();
        async move {
            let character = positive(character)?;
            crate::services::wallets::balance(&handle, character).await.map_err(lua_error)
        }
    })?)?;
    for (name, credit) in [("credit", true), ("debit", false)] {
        wallets.set(name, lua.create_async_function(move |_, (ctx, character, value): (mlua::AnyUserData, i64, f64)| async move {
            mutate(ctx, Mutation::Wallet { character: positive(character)?, amount: amount(value)?, credit }).await?;
            Ok(())
        })?)?;
    }
    dao.set("wallets", wallets)?;
    let items = lua.create_table()?;
    for (name, owner) in [("by_owner", true), ("by_location", false)] {
        let handle = db.clone();
        let sde = sde.clone();
        items.set(name, lua.create_async_function(move |lua, id: i64| {
            let handle = handle.clone();
            let sde = sde.clone();
            async move {
                let id = positive(id)?;
                let query = if owner { ItemQuery::Owner(id) } else { ItemQuery::Location(id) };
                let rows = crate::services::items::load_items(&handle, &sde, query).await.map_err(mlua::Error::external)?;
                item_rows(&lua, rows)
            }
        })?)?;
    }
    let handle = db.clone();
    let sde = sde.clone();
    items.set("in_hangar", lua.create_async_function(move |lua, (character, station): (i64, i64)| {
        let handle = handle.clone();
        let sde = sde.clone();
        async move {
            let query = ItemQuery::OwnerLocationFlag(positive(character)?, positive(station)?, crate::consts::FLAG_HANGAR);
            let rows = crate::services::items::load_items(&handle, &sde, query).await.map_err(mlua::Error::external)?;
            item_rows(&lua, rows)
        }
    })?)?;
    items.set("create", lua.create_async_function(|_, (ctx, owner, location, flag, type_id, quantity): (mlua::AnyUserData, i64, i64, i32, i32, i32)| async move {
        positive(owner)?; positive(location)?; positive(i64::from(type_id))?; positive(i64::from(quantity))?;
        mutate(ctx, Mutation::Create { owner, location, flag, type_id, quantity }).await?;
        Ok(())
    })?)?;
    items.set("move", lua.create_async_function(|_, (ctx, item, container, flag, quantity): (mlua::AnyUserData, i64, i64, Option<i32>, Option<i64>)| async move {
        positive(item)?; positive(container)?;
        if let Some(q) = quantity {
            positive(q)?;
            i32::try_from(q).map_err(|_| mlua::Error::runtime("quantity exceeds inventory limit"))?;
        }
        mutate(ctx, Mutation::Move { container: crate::services::inventory::access::Container::Item(container), item, flag, quantity }).await
    })?)?;
    items.set("delete", lua.create_async_function(|_, (ctx, item): (mlua::AnyUserData, i64)| async move {
        mutate(ctx, Mutation::Delete { item: positive(item)? }).await?;
        Ok(())
    })?)?;
    dao.set("items", items)?;
    let market = lua.create_table()?;
    for (name, bid) in [("bids", true), ("asks", false)] {
        let handle = db.clone();
        market.set(name, lua.create_async_function(move |lua, (station, type_id): (i64, i64)| {
            let handle = handle.clone();
            async move {
                let station = positive(station)?;
                let type_id = i32::try_from(positive(type_id)?).map_err(|_| mlua::Error::runtime("type identifier exceeds limit"))?;
                let orders = crate::dao::market::listings::orders(&handle, crate::client::filetime_now(), type_id, crate::dao::market::listings::Area::Station, station, Some(bid)).await.map_err(mlua::Error::external)?;
                let rows = lua.create_table()?;
                for (index, order) in orders.into_iter().enumerate() {
                    let row = lua.create_table()?;
                    row.set("order_id", order.order_id)?;
                    row.set("price", order.price)?;
                    row.set("volume_remaining", order.vol_remaining)?;
                    row.set("min_volume", order.min_volume)?;
                    row.set("range", order.range)?;
                    rows.set(index + 1, row)?;
                }
                Ok(rows)
            }
        })?)?;
    }
    dao.set("market", market)?;
    let mail = lua.create_table()?;
    mail.set("send", lua.create_async_function(|_, (ctx, recipients, title, body, to_list, to_corp, reply_to, forwarded_from): (mlua::AnyUserData, Table, String, String, Option<i64>, Option<i64>, Option<i64>, Option<i64>)| async move {
        for id in [to_list, to_corp, reply_to, forwarded_from].into_iter().flatten() { positive(id)?; }
        let input = crate::services::mail::delivery::SendMail { to_chars: ids(recipients)?, to_list, to_corp, title, body, reply_to: reply_to.unwrap_or(0), forwarded_from: forwarded_from.unwrap_or(0) };
        mutate(ctx, Mutation::Mail(input)).await
    })?)?;
    dao.set("mail", mail)?;

    let characters = lua.create_table()?;
    let handle = db.clone();
    characters.set("get", lua.create_async_function(move |lua, id: i64| {
        let handle = handle.clone();
        async move {
            let row = social::public_info(&handle, id, crate::consts::FILETIME_SECOND, crate::consts::FILETIME_UNIX_EPOCH).await.map_err(mlua::Error::external)?;
            row.map(|row| {
                let result = lua.create_table()?;
                result.set("name", row.name)?;
                result.set("corporation_id", row.corporation_id)?;
                result.set("race_id", row.race_id)?;
                result.set("bloodline_id", row.bloodline_id)?;
                result.set("ancestry_id", row.ancestry_id)?;
                result.set("school_id", row.school_id)?;
                result.set("gender_id", row.gender_id)?;
                result.set("created", row.created)?;
                Ok(result)
            }).transpose()
        }
    })?)?;
    let handle = db.clone();
    characters.set("name", lua.create_async_function(move |_, id: i64| {
        let handle = handle.clone();
        async move { routing::character_name(&handle, id).await.map_err(mlua::Error::external) }
    })?)?;
    let handle = db.clone();
    characters.set("ship", lua.create_async_function(move |_, id: i64| {
        let handle = handle.clone();
        async move { selection::ship_id(&handle, id).await.map_err(mlua::Error::external) }
    })?)?;
    dao.set("characters", characters)?;
    let skills = lua.create_table()?;
    let handle = db.clone();
    skills.set("levels", lua.create_async_function(move |lua, id: i64| {
        let handle = handle.clone();
        async move {
            let rows = levels::levels(&handle, id).await.map_err(mlua::Error::external)?;
            let result = lua.create_table()?;
            for row in rows { result.set(row.type_id, row.level)?; }
            Ok(result)
        }
    })?)?;
    dao.set("skills", skills)?;
    let corps = lua.create_table()?;
    let handle = db.clone();
    corps.set("of", lua.create_async_function(move |_, id: i64| {
        let handle = handle.clone();
        async move { crimewatch::corporation(&handle, id).await.map_err(mlua::Error::external) }
    })?)?;
    dao.set("corps", corps)?;
    let standing = lua.create_table()?;
    let handle = db.clone();
    standing.set("get", lua.create_async_function(move |_, (id, from): (i64, i64)| {
        let handle = handle.clone();
        async move { standings::standing(&handle, id, from).await.map_err(mlua::Error::external) }
    })?)?;
    standing.set("set", lua.create_async_function(|_, (ctx, character, from, value): (mlua::AnyUserData, i64, i64, f64)| async move {
        positive(character)?; positive(from)?;
        if !value.is_finite() { return Err(mlua::Error::runtime("standing must be finite")); }
        mutate(ctx, Mutation::Standing { character, from, value }).await?;
        Ok(())
    })?)?;
    dao.set("standings", standing)?;
    env.set("dao", dao)
}

#[cfg(test)]
mod tests;
