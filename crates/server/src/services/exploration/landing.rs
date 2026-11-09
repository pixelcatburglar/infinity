//! Actual-exit site spawning for beyonce scan warps. Site state and persistence
//! are described in the parent module; pending arrivals are transient. Mining
//! reservations remain retryable until the site and its committed rocks are saved.

use super::*;

fn reserve(slot: &mut Option<Vec<i64>>) -> bool {
    if slot.is_some() { return false; }
    *slot = Some(Vec::new());
    true
}

fn release(slot: &mut Option<Vec<i64>>) {
    if slot.as_ref().is_some_and(Vec::is_empty) { *slot = None; }
}

pub(super) async fn arrive(db: &Db, world: &Arc<World>, user: i64, system: i64, site_id: i64) -> Result<(), CallError> {
    let (site, containers) = {
        let mut st = state();
        let Some(site) = st.sites.get_mut(&site_id).filter(|site| site.system == system && site.wormhole.is_none()) else { return Ok(()) };
        let containers = site.mining_field().is_some() || site.is_hack_site();
        if !containers && site.dungeon_id.is_none() { return Ok(()); }
        let slot = if containers { &mut site.containers } else { &mut site.npcs };
        if !reserve(slot) { return Ok(()); }
        (site.clone(), containers)
    };
    let result = if let Some(field) = site.mining_field() {
        async {
            let info = system_info(world.sde(), system)?;
            let jspace = (1..=6).contains(&info.class);
            let rocks = super::super::mining::grow_field(db, world, system, site.id, site.pos, field, info.security, jspace).await?;
            let snapshot = state().sites.get(&site.id).cloned().map(|mut s| {
                s.containers = Some(rocks.clone());
                s
            });
            if let Some(s) = snapshot {
                save_site(db, &s).await?;
                if let Some(s) = state().sites.get_mut(&site.id).filter(|s| s.system == system) {
                    s.containers = Some(rocks.clone());
                }
            }
            track_site_clearance(db.clone(), world.clone(), site.id, system, rocks);
            Ok(())
        }.await
    } else if containers {
        spawn_containers(db, world, user, &site).await
    } else {
        spawn_site_npcs(db, world, user, &site).await
    };
    if result.is_err() {
        if let Some(site) = state().sites.get_mut(&site_id).filter(|site| site.system == system) {
            release(if containers { &mut site.containers } else { &mut site.npcs });
        }
    }
    result
}

#[cfg(test)] mod tests;
