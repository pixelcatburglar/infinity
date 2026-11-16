//! Home station lookup and clone self-destruction for the client's
//! `home_station` service, mirroring `homestation`. Reads character home IDs
//! from game data and resolves stations from the startup SDE; a missing
//! station row or required field is a data fault, never a fallback.
//! `self_destruct_clone` is the shared capsule medical path entered from this
//! service; its refusals use ordinary CustomNotify text.
use super::*;
use crate::services::notify_error;

pub(in crate::services) struct Station {
    pub station_id: i64,
    pub solar_system_id: i64,
    pub constellation_id: i64,
    pub region_id: i64,
}

/// Resolve a station row with the required location fields; a missing row or a
/// null required field is a data fault, never a fallback.
pub(in crate::services) fn resolve(sde: &crate::sde::Sde, id: i64) -> Result<Station, CallError> {
    let station = sde.map.station(id)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station required row not found")))?;
    let required = |value: Option<i64>| value.ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station required field is null")));
    Ok(Station {
        station_id: station.station_id,
        solar_system_id: required(station.solar_system_id)?,
        constellation_id: required(station.constellation_id)?,
        region_id: required(station.region_id)?,
    })
}

/// The character's ordinary home: the explicit home station, else the current station.
pub(in crate::services) async fn home(ex: &(impl crate::db::executor::Executor + ?Sized), sde: &crate::sde::Sde, me: i64) -> Result<Station, CallError> {
    let id = crate::dao::characters::ship::home_station(ex, me).await.map_err(db_err)?
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station required row not found")))?;
    resolve(sde, id)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = char_id(c);
    Some(match (call.service, call.method) {
        ("home_station", "get_home_station") => {
            async {
                let id = crate::dao::characters::social::home_station_id(&c.db, me).await.map_err(db_err)?;
                let Some(r) = id.and_then(|id| c.sde.map.station(id)) else { return Ok(Value::None) };
                let type_id = r.type_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station type_id is null")))?;
                let solar_system_id = r.solar_system_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("home station solar_system_id is null")))?;
                Ok(keyval([
                    ("id", Value::Int(r.station_id)),
                    ("type_id", Value::Int(type_id)),
                    ("solar_system_id", Value::Int(solar_system_id)),
                    ("is_fallback", Value::Bool(false)),
                ]))
            }.await
        }
        ("home_station", "self_destruct_clone") => {
            async {
                // `homestation/client/service.py` sends this unbound with no
                // positional arguments and no business keyword arguments.
                if call.bound || !call.args.is_empty()
                    || !(matches!(call.kwargs, Value::None)
                        || matches!(call.kwargs, Value::Dict(entries) if entries.is_empty())) {
                    return Err(notify_error(infinity_core::messages::clones::SELF_DESTRUCT_INVALID_ARGUMENTS));
                }
                let identity = crate::services::identity::Identity::of_client(c);
                let expected = infinity_core::session::OnlineSession::from_session(&c.session)
                    .filter(|expected| expected.user_id == Some(c.login.user_id))
                    .ok_or_else(|| notify_error(infinity_core::messages::clones::STALE_BODY))?;
                let (db, world, sde) = (c.db.clone(), c.world.clone(), c.sde.clone());
                crate::services::ship::loss::medical::self_destruct(
                    &identity, &expected, &db, &world, &sde,
                    move |change: crate::services::ship::SessionChange,
                          outbox: crate::db::outbox::Outbox<crate::services::effects::online::OnlineChanged>| {
                        c.change_session(change.updates);
                        crate::services::effects::online::dispatch_changed((), outbox, c)
                    },
                ).await?;
                Ok(Value::None)
            }.await
        }
        _ => return None,
    })
}

#[cfg(all(test, feature = "tests-services"))] mod tests;
