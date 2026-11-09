//! Exploration client-service dispatch.
//! Shared exploration state and data sources are documented in the parent module.
//! GM operations reuse persisted exploration pairs and the normal collapse and overlay paths.
//! GM scope is the current system; universe-wide operations are not supported.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("wormholeMgr", "GetWormholePolarization") => {
            let (char_id, id) = (c.session.int("charid")?, call.args.first().and_then(|v| v.as_int()).unwrap_or(0));
            Some(Ok(polarization(char_id, id).map(|(end, duration)| Value::tuple([Value::Int(end), Value::Int(duration)])).unwrap_or(Value::None)))
        }
        ("wormholeMgr", "WormholeJump") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            Some(wormhole_jump(c, id).await)
        }
        _ => None,
    }
}

fn gm_system(c: &Client) -> Result<i64, CallError> {
    c.session.int("solarsystemid").ok_or_else(|| gm_error(messages::GM_UNDOCK_FIRST))
}

/// Read persisted connections without loading or generating exploration state.
pub(crate) async fn gm_status(c: &Client) -> Result<String, CallError> {
    let system = gm_system(c)?;
    let now = filetime_now();
    let mut rows: Vec<_> = crate::dao::exploration::sites(&c.db, system).await.map_err(db_err)?
        .into_iter().filter_map(|s| {
            let w = s.wormhole?;
            (now - w.born <= filetime_ticks_from_seconds(minutes_to_seconds(w.life_minutes) as i64)
                && w.mass_used < w.mass_max)
                .then(|| messages::gm::gm_wh_status_row(&s.target, w.type_id, w.dest_system, w.dest_class, w.mass_used, w.mass_max))
        }).collect();
    rows.sort();
    if rows.is_empty() { Ok(messages::gm::GM_WH_NONE.into()) } else { Ok(rows.join("\n")) }
}

/// Ensure at least one usable connection from the current system.
pub(crate) async fn gm_ensure(c: &Client) -> Result<String, CallError> {
    let system = gm_system(c)?;
    expire_wormholes(&c.db, &c.world).await?;
    ensure_loaded(&c.db, &c.world, system).await?;
    let _guard = load_lock().lock().await;
    if system_sites(system).iter().any(|s| s.wormhole.as_ref().is_some_and(|w| w.mass_used < w.mass_max)) {
        return Ok(messages::gm::GM_WH_ENSURED.into());
    }
    create_pairs(c, system, 1).await?;
    Ok(messages::gm::GM_WH_ENSURED.into())
}

/// Create additional random connections using the exploration generator.
pub(crate) async fn gm_random(c: &Client, count: usize) -> Result<String, CallError> {
    let system = gm_system(c)?;
    ensure_loaded(&c.db, &c.world, system).await?;
    let _guard = load_lock().lock().await;
    let created = create_pairs(c, system, count).await?;
    Ok(messages::gm::gm_wh_created(created))
}

async fn create_pairs(c: &Client, system: i64, count: usize) -> Result<usize, CallError> {
    let info = system_info(c.world.sde(), system)?;
    let targets: &[i64] = if (1..=6).contains(&info.class) { &[7, 8, 9] } else { &[1, 2, 3, 4, 5, 6] };
    let mut taken: HashSet<String> = system_sites(system).into_iter().map(|s| s.target).collect();
    let mut destination_targets = HashMap::new();
    let mut pairs = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(pair) = wormhole_pair(&c.db, c.world.sde(), system, info.class, targets, &mut taken, &mut destination_targets).await? else {
            return Err(gm_error(messages::GM_NO_WORMHOLES));
        };
        pairs.push(pair);
    }
    let world = &c.world;
    c.db.transaction(
        |tx, outbox: &mut Outbox<Vec<(Site, Site)>>| {
            let pairs = pairs.clone();
            Box::pin(async move {
                save_pairs(tx, outbox, pairs).await
            })
        },
        |(), outbox: Outbox<Vec<(Site, Site)>>| async move {
            for effect in outbox.into_entries() {
                match effect {
                    Effect::Notification(notification) => notification.dispatch(world, None)?,
                    Effect::Operation(pairs) => {
                        {
                            let mut st = state();
                            for (here, there) in &pairs {
                                st.sites.insert(here.id, here.clone());
                                st.sites.insert(there.id, there.clone());
                            }
                        }
                        for (here, there) in pairs {
                            broadcast_sites(world, here.system, &[here], &[]).await;
                            broadcast_sites(world, there.system, &[there], &[]).await;
                        }
                    }
                }
            }
            Ok(())
        },
    ).await?;
    Ok(count)
}

async fn save_pairs(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<Vec<(Site, Site)>>, pairs: Vec<(Site, Site)>) -> Result<(), CallError> {
    for (here, there) in &pairs {
        save_site(ex, here).await?;
        save_site(ex, there).await?;
    }
    outbox.operation(pairs);
    Ok(())
}

/// Collapse both endpoints, their persisted scan records and their runtime balls.
pub(crate) async fn gm_clear(c: &Client) -> Result<String, CallError> {
    let system = gm_system(c)?;
    ensure_loaded(&c.db, &c.world, system).await?;
    let ids: Vec<i64> = system_sites(system).into_iter().filter(|s| s.wormhole.is_some()).map(|s| s.id).collect();
    let count = ids.len();
    for id in ids { collapse(&c.db, &c.world, id).await?; }
    Ok(messages::gm::gm_wh_cleared(count))
}

#[cfg(test)]
mod tests;
