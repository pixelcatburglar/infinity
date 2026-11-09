use super::in_range;

#[test]
fn range_uses_missing_distance_fallback_and_inclusive_limit() {
    assert!(in_range(Some(2500.0), None));
    assert!(!in_range(Some(2500.0001), None));
    assert!(in_range(Some(0.0), Some(0.0)));
    assert!(!in_range(Some(0.0001), Some(0.0)));
    assert!(in_range(Some(42.0), Some(42.0)));
    assert!(!in_range(Some(42.0001), Some(42.0)));
    assert!(!in_range(None, None));
}

#[test]
fn range_rejects_nonfinite_geometry_and_limits() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!in_range(Some(value), None));
        assert!(!in_range(Some(0.0), Some(value)));
    }
}

async fn settings(db: &crate::db::Db, ship: i64, owner: i64, fleet: bool, maintenance: bool) {
    use crate::dao::items::{ship, ship_configuration::{self, Patch}};
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        assert!(ship::lock_reload_ship(tx, ship).await?);
        assert!(ship_configuration::patch(tx, ship, owner, &Patch { fleet_hangar_corp: Some(fleet), maintenance_corp: Some(maintenance), ..Patch::default() }).await?);
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
}

async fn change_item(db: &crate::db::Db, sde: &crate::sde::Sde, id: i64, owner: i64, quantity: i64) {
    use crate::dao::items::{inventory, loading::ItemQuery, quantities::{self, QuantityChange}};
    let item = crate::services::items::load_items(db, sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
    assert!(quantities::apply(db, &[(id, QuantityChange::new(&item, Some(quantity)))].into_iter().collect()).await.unwrap());
    let item = crate::services::items::load_items(db, sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
    assert!(inventory::move_item(db, &item, item.location_id, item.flag_id, owner, item.singleton).await.unwrap());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn batch_checks_independent_bays_refreshes_revocation_and_propagates_database_errors() {
    use super::{Endpoint, Reachability};
    use infinity_core::consts::{CATEGORY_SHIP, FLAG_FLEET_HANGAR, FLAG_SHIP_HANGAR};
    use infinity_core::consts::dogma::{ATTR_FLEET_HANGAR_CAPACITY, ATTR_SHIP_MAINTENANCE_BAY_CAPACITY};
    use crate::services::tests::fixture;
    use marshal::Value;
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let system = client.sde.map.stations().find_map(|s| s.solar_system_id).unwrap();
    let kind = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_FLEET_HANGAR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let pilot = crate::dao::items::creation::positioned(&db, kind, actor.character, system, [0.0; 3]).await.unwrap();
    let host = crate::dao::items::creation::positioned(&db, kind, owner.character, system, [0.0; 3]).await.unwrap();
    settings(&db, host, owner.character, true, false).await;
    client.session.apply(vec![("shipid", Value::Int(pilot)), ("solarsystemid", Value::Int(system)), ("stationid", Value::None)]);
    client.world.system(system).await.unwrap();
    let ball = |id, owner_id, char_id| crate::space::Ball {
        id, type_id: kind, owner_id, corp_id: 1, alliance_id: 0, char_id,
        mode: crate::space::MODE_STOP, radius: 50.0, signature_radius: None, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![],
    };
    let (pilot_ball, host_ball) = (ball(pilot, actor.character, Some(actor.character)), ball(host, owner.character, None));
    client.world.with(system, move |s| { s.insert(pilot_ball); s.insert(host_ball); }).await.unwrap();
    let endpoints = [Endpoint { location: host, flag: FLAG_FLEET_HANGAR }, Endpoint { location: host, flag: FLAG_SHIP_HANGAR }];
    let proof = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &db, &client.world, Some(system), &endpoints).await.unwrap();
    let inspected = proof.inspect_many(&db, &client.sde, &endpoints).await.unwrap();
    assert_eq!(inspected.admitted, [true, false]);
    assert_eq!(inspected.lineage(host).unwrap().get(&host).unwrap().owner_id, owner.character);
    assert!(inspected.lineage(pilot).is_none());
    change_item(&db, &client.sde, host, actor.character, 1).await;
    let stale = proof.inspect_many(&db, &client.sde, &endpoints).await.unwrap();
    assert_eq!(stale.admitted, [false, false]);
    assert!(stale.lineage(host).is_none());
    change_item(&db, &client.sde, host, owner.character, 1).await;
    assert_eq!(proof.check_many(&db, &client.sde, &endpoints).await.unwrap(), [true, false]);
    settings(&db, host, owner.character, false, true).await;
    assert_eq!(proof.check_many(&db, &client.sde, &endpoints).await.unwrap(), [false, true]);
    settings(&db, host, owner.character, false, false).await;
    assert_eq!(proof.check_many(&db, &client.sde, &endpoints).await.unwrap(), [false, false]);
    settings(&db, host, owner.character, false, true).await;
    let inner = crate::dao::items::creation::rookie_ship(&db, kind, owner.character, host, FLAG_SHIP_HANGAR).await.unwrap();
    settings(&db, inner, owner.character, true, false).await;
    let nested = Endpoint { location: inner, flag: FLAG_FLEET_HANGAR };
    let nested_proof = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &db, &client.world, Some(system), &[nested]).await.unwrap();
    assert!(nested_proof.check(&db, &client.sde, nested).await.is_ok());
    let private = Endpoint { location: inner, flag: infinity_core::consts::FLAG_CARGO };
    assert!(nested_proof.check(&db, &client.sde, private).await.is_err());
    settings(&db, host, owner.character, false, false).await;
    assert!(nested_proof.check(&db, &client.sde, nested).await.is_err());
    settings(&db, host, owner.character, false, true).await;
    for invalid in [pilot, host, inner] {
        let owner_id = if invalid == pilot { actor.character } else { owner.character };
        change_item(&db, &client.sde, invalid, owner_id, 2).await;
        let endpoint = if invalid == pilot { Endpoint { location: pilot, flag: infinity_core::consts::FLAG_CARGO } } else { nested };
        let current = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &db, &client.world, Some(system), &[endpoint]).await.unwrap();
        assert!(current.check(&db, &client.sde, endpoint).await.is_err());
        change_item(&db, &client.sde, invalid, owner_id, 1).await;
    }
    let (mut connection, task) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { task.await.unwrap(); });
    let tx = connection.transaction().await.unwrap();
    assert!(tx.query_one("SELECT 1/0", &[]).await.is_err());
    assert!(matches!(proof.check_many(&tx, &client.sde, &endpoints).await, Err(crate::services::CallError::Internal(_))));
    tx.rollback().await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
}
