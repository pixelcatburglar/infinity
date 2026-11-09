//! Lua pilot targeting and default module effects through the normal combat services.
//! Mirrors godma effect activation and targetMgr locking using game items and SDE dogma.
//! Lock completion and effect cycles are actor-timed; runtime state is not persisted.
use mlua::{Lua, Table};
use infinity_core::messages;
use crate::{dao::items::loading::ItemQuery, services::{self, CallError, combat::{self, targeting_charges}, effects}};
use super::{lifecycle, lua_error, mutate, positive, space::actual_system, Resources};

async fn default_effect(r: &Resources, item: i64) -> Result<&str, CallError> {
    let item = services::items::load_items(&r.db, &r.sde, ItemQuery::Id(item)).await.map_err(services::db_err)?;
    item.first().and_then(|item| r.sde.types.effects(item.type_id).iter().find(|effect| effect.is_default))
        .and_then(|effect| r.sde.dogma.effect(effect.effect_id))
        .and_then(|effect| effect.name.as_deref())
        .ok_or_else(|| services::notify_error(messages::lua::PILOT_NO_DEFAULT_EFFECT))
}

pub(super) fn register(lua: &Lua, api: &Table, resources: &Resources) -> mlua::Result<()> {
    let r = resources.clone();
    api.set("activate", lua.create_async_function(move |_, (id, item, target, repeat): (i64, i64, Option<i64>, Option<i64>)| {
        let r = r.clone(); async move {
            positive(id)?; positive(item)?; if let Some(target) = target { positive(target)?; }
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                let pilot = state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                if system.is_none() { return Err(services::user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![])); }
                let name = default_effect(&r, item).await?;
                effects::lifecycle::activate(&identity, &r.db, &r.world, &r.sde, system, pilot.notify.clone(), r.pilots.account, item, name, target, repeat.unwrap_or(0),
                    |n| effects::online::dispatch_notification(n, &r.world, &pilot.notify, Some(id))).await?;
                Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("deactivate", lua.create_async_function(move |_, (id, item): (i64, i64)| {
        let r = r.clone(); async move {
            positive(id)?; positive(item)?;
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                let pilot = state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                if system.is_none() { return Err(services::user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![])); }
                let name = default_effect(&r, item).await?;
                effects::lifecycle::deactivate(&identity, &r.db, &r.world, &r.sde, system, pilot.notify.clone(), r.pilots.account, item, name,
                    |n| effects::online::dispatch_notification(n, &r.world, &pilot.notify, Some(id))).await?;
                Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("lock_target", lua.create_async_function(move |_, (id, target): (i64, i64)| {
        let r = r.clone(); async move {
            positive(id)?; positive(target)?;
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                let pilot = state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                let cx = combat::Ctx::from_identity(&identity, &r.db, &r.world, system, pilot.notify.clone())
                    .ok_or_else(|| services::user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![]))?;
                targeting_charges::add_target_ctx(&cx, target, false).await?;
                Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("unlock_target", lua.create_async_function(move |_, (id, target): (i64, i64)| {
        let r = r.clone(); async move {
            positive(id)?; positive(target)?;
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                let pilot = state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                if system.is_none() { return Err(services::user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![])); }
                targeting_charges::drop_targets(&identity, &r.world, system, &[target], |event, args| {
                    let _ = pilot.notify.send((event.to_string(), args));
                });
                Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    Ok(())
}
