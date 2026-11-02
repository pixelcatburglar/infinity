use super::*;

#[test]
fn boards_connect_entry_to_core() {
    for tier in 1..=4 {
        let mut id = 0;
        let (tiles, start) = board(tier, || {
            id += 1;
            id
        });
        let core = tiles.values().filter(|t| t.kind == TYPE_CORE).count();
        assert_eq!(core, 1);
        assert!(!tiles[&start].hidden);
        let g = Game {
            container: 0,
            system: 0,
            tier,
            tiles,
            coherence: 100,
            max_coherence: 100,
            strength: 30,
            inventory: vec![],
            next_id: id,
            skip_click: None,
        };
        assert!(g.distance_to_core(start).is_some_and(|d| d > 0));
        assert!(neighbours(start).iter().any(|n| g.flippable(*n)));
    }
}

#[test]
fn analyzer_skill_matches_postgres_integer_cast() {
    assert_eq!(analyzer_skill(2.5).ok(), Some(2));
    assert_eq!(analyzer_skill(3.5).ok(), Some(4));
    assert_eq!(analyzer_skill(-2.5).ok(), Some(-2));
    assert_eq!(analyzer_skill(i32::MIN as f64).ok(), Some(i32::MIN));
    assert_eq!(analyzer_skill(i32::MAX as f64).ok(), Some(i32::MAX));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, i32::MIN as f64 - 1.0, i32::MAX as f64 + 1.0] {
        assert!(matches!(analyzer_skill(value), Err(CallError::Internal(_))));
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn start_requires_space_actor_ship_and_target() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 0);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut identity = Identity { char_id: 1, ship_id: Some(2), corp_id: 0, station_id: None, system_id: Some(3) };
    let result = start(&identity, &db, &world, &sde, None, sender.clone(), 0, 4, None, |_| panic!("unexpected notification")).await.unwrap();
    assert!(matches!(result, Value::None));
    identity.ship_id = None;
    let result = start(&identity, &db, &world, &sde, Some(3), sender.clone(), 0, 4, None, |_| panic!("unexpected notification")).await.unwrap();
    assert!(matches!(result, Value::None));
    identity.ship_id = Some(2);
    identity.char_id = 0;
    let result = start(&identity, &db, &world, &sde, Some(3), sender.clone(), 0, 4, None, |_| panic!("unexpected notification")).await.unwrap();
    assert!(matches!(result, Value::None));
    identity.char_id = 1;
    let result = start(&identity, &db, &world, &sde, Some(3), sender, 0, 4, None, |_| panic!("unexpected notification")).await;
    assert!(matches!(result, Err(CallError::User { msg, .. }) if msg == messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT));
    assert!(receiver.try_recv().is_err());
}
