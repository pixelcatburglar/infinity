//! Repackaging a stance hull deletes its hidden mode item and timing; the next
//! assembly initializes the default mode again without a cooldown.
use super::*;
use infinity_core::consts::{CATEGORY_MODULE, FLAG_HIDDEN_MODIFIERS};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn repackage_drops_mode_and_timing_then_assembly_initializes_default() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let stances = &crate::sde::ship_stances::HULL_STANCES[0];
    let rig = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
    let f = create(&db, sde.map.stations().next().unwrap().station_id, stances.hull_type, rig).await;
    let (mut client, _peer) = crate::services::tests::client_with_sde(db.clone(), f.owner, sde.clone()).await;
    client.session.apply(vec![("stationid", Value::Int(f.station))]);
    db.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true)",
        &[&stances.modes[0].1, &f.owner, &f.hull, &FLAG_HIDDEN_MODIFIERS]).await.unwrap();
    db.execute("INSERT INTO game.ship_stances (ship_id,switched_at,ready_at) VALUES ($1,0,0)", &[&f.hull]).await.unwrap();
    request(&mut client, f, Some(Value::Bool(true))).await.unwrap();
    assert!(db.query_opt("SELECT 1 FROM game.items WHERE location_id=$1 AND flag_id=$2", &[&f.hull, &FLAG_HIDDEN_MODIFIERS]).await.unwrap().is_none()
        && crate::dao::items::ship_stance::timing(&db, f.hull).await.unwrap().is_none());
    assert_eq!(crate::services::fitting::ship::assemble_ships(&mut client, vec![f.hull], None).await.unwrap(), vec![f.hull]);
    assert_eq!((db.query_one("SELECT type_id FROM game.items WHERE location_id=$1 AND flag_id=$2",
        &[&f.hull, &FLAG_HIDDEN_MODIFIERS]).await.unwrap().get::<_, i32>("type_id"),
        crate::dao::items::ship_stance::timing(&db, f.hull).await.unwrap()),
        (stances.modes[0].1, Some(crate::dao::items::ship_stance::Timing { previous_stance_id: None, switched_at: 0, ready_at: 0 })));
    teardown(&db, f).await;
}
