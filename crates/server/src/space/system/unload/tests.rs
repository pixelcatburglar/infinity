use super::*;
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn static_scene_and_busy_priority() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let row = sde.map.all_celestials().find(|row| {
        row.solar_system_id.is_some_and(|id| sde.map.is_public_system(id)
            && !sde.map.celestials(id).any(|c| c.group_id == Some(infinity_core::consts::GROUP_ASTEROID_BELT as i64)))
            && row.type_id.and_then(|id| i32::try_from(id).ok())
                .is_some_and(|id| sde.types.get(id).is_some())
    }).unwrap();
    let (id, celestial) = (row.solar_system_id.unwrap(), row.item_id);
    let world = World::new(db, sde.clone(), 1);
    world.system(id).await.unwrap();
    let ball = world.with(id, move |s| s.ball(celestial).cloned()).await.unwrap().unwrap();
    let mut s = System::new(id, Arc::default(), 0);
    s.projection_sde = Some(sde); s.balls.insert(celestial, ball);
    assert_eq!(s.unload_blocked(), None);
    let (tx, _rx) = mpsc::unbounded_channel();
    s.pilots.insert(1, PilotState { ship_id: celestial, npc_ignored: false, binding: None });
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(s.observe(1, celestial, tx, completed));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(done.try_recv(), Ok(true));
    s.schedule(1000, |_| {});
    for reason in ["pilots", "observers", "timers"] {
        assert_eq!(s.unload_blocked(), Some(reason));
        match reason { "pilots" => s.pilots.clear(), "observers" => s.observers.clear(),
            _ => s.timers.clear() }
    }
    s.balls.get_mut(&celestial).unwrap().id = -1;
    assert_eq!(s.unload_blocked(), Some("dynamic_ball"));
}

#[test]
fn unload_occupied_counts_pilots_and_pending_observers() {
    let mut s = System::default();
    assert!(!s.unload_occupied());
    s.pilots.insert(1, PilotState { ship_id: 1, npc_ignored: false, binding: None });
    assert!(s.unload_occupied());
    s.pilots.clear();
    let (tx, rx) = mpsc::unbounded_channel();
    let (completed, _done) = tokio::sync::oneshot::channel();
    // The initial SetState is still owed, so the token check would pass while
    // the pending observer still occupies the idle window.
    assert!(s.observe(1, 2, tx, completed));
    assert!(s.unload_occupied());
    drop(rx);
    s.prune_observers();
    assert!(!s.unload_occupied());
}
