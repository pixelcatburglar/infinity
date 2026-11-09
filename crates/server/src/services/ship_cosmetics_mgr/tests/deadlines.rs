use super::*;

#[test]
fn duration_and_skin_boundaries_are_rejected_without_narrowing() {
    for value in [i64::MAX, i64::from(i32::MAX) + 1, 0, -1] {
        let args = [Value::Int(value)];
        let kwargs = Value::dict([( "duration", Value::Int(1) )]);
        assert!(gm_args(&call("GiveSkin", &args, &kwargs), 1).is_err());
    }
    let args = [Value::Int(1)];
    for value in [0, -1] {
        let kwargs = Value::dict([( "duration", Value::Int(value) )]);
        assert!(gm_args(&call("GiveSkin", &args, &kwargs), 1).is_err());
    }
}

#[test]
fn duplicate_and_malformed_kwargs_are_rejected() {
    let args = [Value::Int(1)];
    for kwargs in [
        Value::Bool(false),
        Value::list([]),
        Value::dict([( "isSingleUse", Value::Int(0) )]),
        Value::dict([( "duration", Value::Int(1) ), ("isSingleUse", Value::Bool(false)), ("extra", Value::None)]),
    ] {
        assert!(gm_args(&call("GiveSkin", &args, &kwargs), 1).is_err());
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn expiry_plus_one_overflow_is_rejected() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (c, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let skin = c.sde.ship_skins.licenses().find_map(|license| {
        let skin = c.sde.ship_skins.skin(license.skin_id)?;
        (!skin.is_structure_skin && skin.visible(&c.cfg.server.boot_region)).then_some(skin.skin_id)
    }).expect("visible ordinary SKIN");
    let now = i64::MAX - infinity_core::consts::FILETIME_DAY;
    assert!(licenses::grant_tx(&db, &c.sde, &c.cfg.server.boot_region, fixture.character, skin, Some(1), now).await.is_err());
    assert!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().is_none());
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn query_and_authorized_selection_converge_overdue_licenses() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db.clone(), character).await;
        let station = c.sde.map.stations().find(|s| {
            s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()
        }).expect("SDE station with complete location");
        let (station_id, system_id, constellation_id, region_id) = (
            station.station_id, station.solar_system_id.unwrap(),
            station.constellation_id.unwrap(), station.region_id.unwrap(),
        );
        test_db.execute(
            "UPDATE game.characters SET station_id=$2, solar_system_id=$3, constellation_id=$4, region_id=$5 WHERE character_id=$1",
            &[&character, &station_id, &system_id, &constellation_id, &region_id],
        ).await.unwrap();
        let ship_type = c.sde.types.iter().find(|t| {
            t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
                && t.type_id != infinity_core::consts::TYPE_CAPSULE
        }).expect("published SDE ship").type_id;
        let ship = crate::dao::items::creation::rookie_ship(
            &test_db, ship_type, character, station_id, infinity_core::consts::FLAG_HANGAR,
        ).await.unwrap();
        crate::dao::characters::selection::set_ship(&test_db, character, ship).await.unwrap();
        let skin = c.sde.ship_skins.licenses().find_map(|license| {
            let skin = c.sde.ship_skins.skin(license.skin_id)?;
            (!skin.is_structure_skin && skin.visible(&c.cfg.server.boot_region)).then_some(skin.skin_id)
        }).unwrap();
        let overdue = filetime_now() - 1;
        assert!(ship_skins::insert_license(&test_db, character, skin, Some(overdue)).await.unwrap());
        expiry::schedule(&test_db, character).await.unwrap();
        assert_eq!(query(&c, "GetLicencedSkins", &[]).await.unwrap(), Value::list([]));
        assert!(ship_skins::get_stored_license(&test_db, character, skin).await.unwrap().is_none());
        assert!(ship_skins::insert_license(&test_db, character, skin, Some(overdue)).await.unwrap());
        expiry::schedule(&test_db, character).await.unwrap();
        let args = [Value::Int(character)];
        let kwargs = Value::None;
        let select = Call { service: "charUnboundMgr", method: "SelectCharacterID", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        c.login.user_id = fixture.account + 1;
        assert!(crate::services::charsel::dispatch(&mut c, &select).await.is_err());
        assert!(ship_skins::get_stored_license(&test_db, character, skin).await.unwrap().is_some());
        c.login.user_id = fixture.account;
        crate::services::charsel::dispatch(&mut c, &select).await.unwrap();
        assert!(ship_skins::get_stored_license(&test_db, character, skin).await.unwrap().is_none());
        assert!(crate::dao::jobs::lock_job(&test_db, &format!("{}:{}", expiry::KIND, character)).await.unwrap().is_none());
    }).await;
    crate::scheduler::cancel(&db, &format!("{}:{}", expiry::KIND, character)).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&character]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
