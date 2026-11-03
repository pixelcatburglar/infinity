//! Fleet watchlist damage uses real ballpark snapshots, never a health cache.
//! michelle.py:875-879,1594-1599 consumes the targeted ballpark function.
use super::*;
use std::sync::Arc;

/// Called outside the space actor with the damaged ball's captured visibility.
pub(crate) async fn route(world: Arc<World>, damaged: i64, source_system: i64, ship: i64, cloak: u8, health: Value, only: Option<i64>) {
    let watchers = {
        let st = state();
        let Some(&fleet_id) = st.of.get(&damaged) else { return };
        if !st.is_member(fleet_id, damaged) { return; }
        let Some(fleet) = st.fleets.get(&fleet_id) else { return };
        fleet.members.iter().filter_map(|(&id, member)| {
            (id != damaged && only.is_none_or(|wanted| wanted == id) && st.is_member(fleet_id, id)
                && member.damage_updates && member.watchlist.contains(&damaged)).then_some(id)
        }).collect::<Vec<_>>()
    };
    for watcher in watchers {
        let Some(system) = world.online_session(watcher).and_then(|session| session.system_id) else { continue };
        let damage = health.clone();
        world.post(system, move |actor| actor.fleet_damage_update(watcher, source_system, ship, cloak, damage));
    }
}

/// Registration obtains the current damage from each target's actual actor.
pub(super) async fn initial(world: Arc<World>, watcher: i64, targets: Vec<i64>) {
    for target in targets {
        let Some(system) = world.online_session(target).and_then(|session| session.system_id) else { continue };
        let Some(Some((ship, cloak, health))) = world.with(system, move |actor| actor.fleet_damage_snapshot(target)).await else { continue };
        route(world.clone(), target, system, ship, cloak, health, Some(watcher)).await;
    }
}
