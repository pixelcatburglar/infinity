use super::*;
use std::future::Future;
use infinity_core::consts::{GROUP_ASTEROID_BELT, KNOWN_SPACE_SYSTEM_START, WORMHOLE_SYSTEM_END, WORMHOLE_SYSTEM_START};

/// Independent oracle: the shared client ranges, without the map predicate.
fn in_public_range(id: i64) -> bool {
    (KNOWN_SPACE_SYSTEM_START..=(WORMHOLE_SYSTEM_START - 1)).contains(&id)
        || (WORMHOLE_SYSTEM_START..=WORMHOLE_SYSTEM_END).contains(&id)
}

/// Same source-match rule the selector uses: a stargate record owned by `id`.
fn has_gate(map: &Map, id: i64) -> bool {
    map.celestials(id).filter_map(|c| map.stargate(c.item_id)).any(|g| g.solar_system_id == Some(id))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn preload_selects_static_systems_and_direct_gates() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let map = &sde.map;
    // The public set is exactly the map members inside the two client ranges.
    let public: Vec<i64> = map.systems().map(|s| s.solar_system_id).filter(|&id| in_public_range(id)).collect();
    assert_eq!(map.public_systems().map(|s| s.solar_system_id).collect::<Vec<_>>(), public);
    let all = targets(map, PreloadScope::All).unwrap();
    assert_eq!(all, public);
    assert!(all.windows(2).all(|w| w[0] < w[1]));
    // A real static row outside the ranges is not a preload target.
    let outside = map.systems().map(|s| s.solar_system_id).find(|&id| !in_public_range(id)).expect("static row outside public ranges");
    assert!(map.system(outside).is_some() && !map.is_public_system(outside) && !all.contains(&outside));
    assert!(targets(map, PreloadScope::CurrentAndNeighbors(outside)).is_none());
    // A real gateless wormhole member is public and selects only itself.
    let wormhole = map.systems().map(|s| s.solar_system_id)
        .find(|&id| (WORMHOLE_SYSTEM_START..=WORMHOLE_SYSTEM_END).contains(&id) && !has_gate(map, id))
        .expect("gateless wormhole system");
    assert!(map.is_public_system(wormhole) && all.contains(&wormhole));
    assert_eq!(targets(map, PreloadScope::CurrentAndNeighbors(wormhole)).unwrap(), vec![wormhole]);
    // A gated public system selects itself plus its public one-hop destinations.
    let gated = map.public_systems().map(|s| s.solar_system_id).find(|&id| has_gate(map, id)).expect("gated public system");
    let mut expected: std::collections::BTreeSet<i64> = [gated].into_iter().collect();
    expected.extend(map.celestials(gated).filter_map(|c| map.stargate(c.item_id))
        .filter(|g| g.solar_system_id == Some(gated)).filter_map(|g| g.dest_solar_system_id)
        .filter(|&d| in_public_range(d) && map.system(d).is_some()));
    assert_eq!(targets(map, PreloadScope::CurrentAndNeighbors(gated)).unwrap(), expected.into_iter().collect::<Vec<_>>());
    // An out-of-range gate destination is excluded when the data has one.
    if let Some((src, dest)) = map.public_systems().map(|s| s.solar_system_id).flat_map(|src| {
        map.celestials(src).filter_map(|c| map.stargate(c.item_id))
            .filter(move |g| g.solar_system_id == Some(src)).filter_map(|g| g.dest_solar_system_id)
            .filter(|&d| map.system(d).is_some() && !in_public_range(d)).map(move |d| (src, d))
    }).next() {
        assert!(!targets(map, PreloadScope::CurrentAndNeighbors(src)).unwrap().contains(&dest));
    }
    // A range ID the map lacks is refused: the range alone is not membership.
    let absent = (KNOWN_SPACE_SYSTEM_START..=WORMHOLE_SYSTEM_END).find(|&id| map.system(id).is_none()).expect("map-absent in-range ID");
    assert!(!map.is_public_system(absent));
    assert!(targets(map, PreloadScope::CurrentAndNeighbors(absent)).is_none());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn preload_snapshot_counts_concurrent_loads_once_per_target() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    // A gateless, beltless public system keeps the batch to one cheap target.
    let id = sde.map.public_systems().map(|s| s.solar_system_id)
        .find(|&id| !has_gate(&sde.map, id)
            && !sde.map.celestials(id).any(|c| c.group_id == Some(GROUP_ASTEROID_BELT as i64)))
        .expect("gateless beltless public system");
    let world = World::new(db.clone(), sde, 1);
    // With the market guard held, each future's first poll finishes the
    // batch-opening snapshot and then waits on the guard inside `World::system`.
    let market = crate::services::market::lock().await;
    let mut first = std::pin::pin!(world.preload_systems(PreloadScope::CurrentAndNeighbors(id)));
    let mut second = std::pin::pin!(world.preload_systems(PreloadScope::CurrentAndNeighbors(id)));
    for pending in [&mut first, &mut second] {
        std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        }).await;
    }
    drop(market);
    for report in [first.await.unwrap(), second.await.unwrap()] {
        assert_eq!(report, PreloadReport { total: 1, originally_loaded: 0, newly_loaded: 1, failed: 0, unprocessed: 0 });
    }
    let handle = world.systems.lock().unwrap().get(&id).cloned().expect("loaded actor");
    let repeat = world.preload_systems(PreloadScope::CurrentAndNeighbors(id)).await.unwrap();
    assert_eq!(repeat, PreloadReport { total: 1, originally_loaded: 1, newly_loaded: 0, failed: 0, unprocessed: 0 });
    let systems = world.systems.lock().unwrap();
    assert_eq!(systems.len(), 1);
    assert!(systems.get(&id).unwrap().tx.same_channel(&handle.tx));
}
