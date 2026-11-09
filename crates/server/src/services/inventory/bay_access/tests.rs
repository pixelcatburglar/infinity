use super::*;

async fn grant(db: &crate::db::Db, ship_id: i64, owner: i64) {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        assert!(crate::dao::items::ship::lock_reload_ship(tx, ship_id).await?);
        assert!(ship_configuration::patch(tx, ship_id, owner, &ship_configuration::Patch { fleet_hangar_corp: Some(true), ..Default::default() }).await?);
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
}

#[test]
fn independent_bays_and_private_flags() {
    let settings = ship_configuration::Settings { owner_id: 1, fleet_hangar_fleet: true, maintenance_corp: true, ..Default::default() };
    let fleet = permissions_from(2, 1, Some(&settings), Affiliation { fleet: true, corporation: false });
    assert!(fleet.allows(FLAG_FLEET_HANGAR));
    assert!(!fleet.allows(FLAG_SHIP_HANGAR));
    assert!(!fleet.allows(0));
    let corp = permissions_from(2, 1, Some(&settings), Affiliation { fleet: false, corporation: true });
    assert!(!corp.allows(FLAG_FLEET_HANGAR));
    assert!(corp.allows(FLAG_SHIP_HANGAR));
    assert!(!corp.allows(0));
    let neither = permissions_from(2, 1, Some(&settings), Affiliation::default());
    assert!(!neither.allows(FLAG_FLEET_HANGAR));
    assert!(!neither.allows(FLAG_SHIP_HANGAR));
    assert!(permissions_from(1, 1, None, Affiliation::default()).allows(0));
    assert!(!permissions_from(0, 0, Some(&settings), Affiliation { fleet: true, corporation: true }).allows(FLAG_FLEET_HANGAR));
    assert!(!permissions_from(2, 3, Some(&settings), Affiliation { fleet: true, corporation: true }).allows(FLAG_FLEET_HANGAR));
    assert!(!permissions_from(2, 1, None, Affiliation { fleet: true, corporation: true }).allows(FLAG_FLEET_HANGAR));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn foreign_docked_hull_remains_inaccessible_even_with_corporation_grant() {
    use crate::services::inventory::reachability::{Endpoint, Reachability};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
    let station_id = station.station_id;
    let system = station.solar_system_id.unwrap();
    let hull = client.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let ship = crate::dao::items::creation::rookie_ship(&db, hull, owner.character, station_id, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    grant(&db, ship, owner.character).await;
    let identity = crate::services::identity::Identity { char_id: actor.character, ship_id: None, corp_id: 1, station_id: Some(station_id), system_id: Some(system) };
    let endpoint = Endpoint { location: ship, flag: FLAG_FLEET_HANGAR };
    let proof = Reachability::capture(&identity, &db, &client.world, None, &[endpoint]).await.unwrap();
    assert!(proof.check(&db, &client.sde, endpoint).await.is_err());
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn batch_grants_follow_fresh_membership_for_multiple_owners() {
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let first = fixture::create(&db).await.unwrap();
    let second = fixture::create(&db).await.unwrap();
    let a = crate::dao::items::creation::rookie_ship(&db, 1, first.character, first.character, 0).await.unwrap();
    let b = crate::dao::items::creation::rookie_ship(&db, 1, second.character, second.character, 0).await.unwrap();
    grant(&db, a, first.character).await;
    grant(&db, b, second.character).await;
    let hulls = [(a, first.character), (b, second.character)];
    let grants = permissions_many(&db, actor.character, &hulls).await.unwrap();
    assert!(grants[&hulls[0]].allows(FLAG_FLEET_HANGAR));
    assert!(grants[&hulls[1]].allows(FLAG_FLEET_HANGAR));
    let corp = fixture::create_corporation(&db, second.character, 1).await.unwrap();
    fixture::assign_corporation(&db, second.character, corp, 0).await.unwrap();
    let grants = permissions_many(&db, actor.character, &hulls).await.unwrap();
    assert!(grants[&hulls[0]].allows(FLAG_FLEET_HANGAR));
    assert!(!grants[&hulls[1]].allows(FLAG_FLEET_HANGAR));
    crate::dao::characters::selection::prepare_delete(&db, first.character, 0).await.unwrap();
    crate::dao::characters::selection::delete(&db, first.character, 1).await.unwrap();
    let grants = permissions_many(&db, actor.character, &hulls).await.unwrap();
    assert!(!grants[&hulls[0]].allows(FLAG_FLEET_HANGAR));
    fixture::cleanup_corporation(&db, corp, &[second.character], 0).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, first).await.unwrap();
    fixture::cleanup(&db, second).await.unwrap();
}
