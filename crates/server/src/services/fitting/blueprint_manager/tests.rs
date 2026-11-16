//! Blueprint projection from real ancestry, lease and job facts.
use super::*;
use crate::services::{self, tests::fixture};
use infinity_core::consts::{BLUEPRINT_COPY_QUANTITY, FLAG_CARGO, FLAG_CORP_SAG1, FLAG_HANGAR, INDUSTRY_JOB_STATUS_INSTALLED, ROLE_ACCOUNTANT, TYPE_OFFICE};

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn owner_facility_nested_blueprints_and_impound_follow_current_custody() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let stranger = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
    let station_id = station.station_id;
    let system = station.solar_system_id.unwrap();
    let corp = fixture::create_corporation(&db, actor.character, station_id).await.unwrap();
    fixture::assign_corporation(&db, actor.character, corp, ROLE_ACCOUNTANT).await.unwrap();
    client.session.apply(vec![("corpid", Value::Int(corp)), ("stationid", Value::Int(station_id))]);
    let bp_type = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_BLUEPRINT)).unwrap().type_id;
    let container_type = client.sde.types.iter().find(|ty| ty.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let office = crate::dao::items::creation::rookie_ship(&db, TYPE_OFFICE, corp, station_id, 0).await.unwrap();
    crate::dao::offices::insert(&db, office, corp, station_id, crate::client::filetime_now() + infinity_core::consts::FILETIME_DAY, 0).await.unwrap();
    let container = crate::dao::items::creation::rookie_ship(&db, container_type, corp, office, FLAG_CORP_SAG1).await.unwrap();
    let original = crate::dao::items::creation::rookie_ship(&db, bp_type, corp, container, FLAG_CARGO).await.unwrap();
    let copy = crate::dao::items::transfers::insert(&db, bp_type, corp, office, FLAG_CORP_SAG1, BLUEPRINT_COPY_QUANTITY).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&copy]).await.unwrap();
    crate::dao::industry::set_blueprint(&db, copy, 4, 6, 8).await.unwrap();
    let job: i64 = db.query_one("INSERT INTO game.industry_jobs (installer_id,owner_id,activity_id,blueprint_id,blueprint_type_id,blueprint_copy,facility_id,solar_system_id,runs,cost,time_seconds,start_date,end_date,status) VALUES ($1,$2,1,$3,$4,false,$5,$6,1,0,1,0,1,$7) RETURNING job_id", &[&actor.character, &corp, &original, &bp_type, &station_id, &system, &INDUSTRY_JOB_STATUS_INSTALLED]).await.unwrap().get("job_id");
    let unrelated = crate::dao::items::transfers::insert(&db, container_type, corp, station_id, infinity_core::consts::FLAG_CORP_DELIVERIES, 1).await.unwrap();
    let (selected, mut selected_rows) = crate::services::universe::assets::Snapshot::blueprints(&client, corp, Some(original)).await.unwrap();
    assert_eq!(selected_rows.len(), 1);
    assert!(selected.item_type(unrelated).is_none());
    assert!(selected.matches_blueprint(&selected_rows[0]));
    selected_rows[0].location_id = station_id;
    assert!(!selected.matches_blueprint(&selected_rows[0]));
    let (rows, counts) = blueprints(&client, corp, None, Some(station_id)).await.unwrap();
    assert_eq!(counts.get(&Some(station_id)), Some(&2));
    let original_row = rows.iter().find(|row| services::value_field(row, "itemID") == Some(&Value::Int(original))).unwrap();
    assert_eq!(services::value_field(original_row, "locationID"), Some(&Value::Int(container)));
    assert_eq!(services::value_field(original_row, "locationFlagID"), Some(&Value::Int(i64::from(FLAG_CORP_SAG1))));
    assert_eq!(services::value_field(original_row, "jobID"), Some(&Value::Int(job)));
    assert_eq!(services::value_field(original_row, "quantity"), Some(&Value::Int(-1)));
    let copy_row = rows.iter().find(|row| services::value_field(row, "itemID") == Some(&Value::Int(copy))).unwrap();
    assert_eq!(services::value_field(copy_row, "quantity"), Some(&Value::Int(BLUEPRINT_COPY_QUANTITY)));
    assert_eq!(services::value_field(copy_row, "runs"), Some(&Value::Int(8)));
    assert!(blueprints(&client, corp, None, Some(station_id + 1)).await.unwrap().0.is_empty());
    db.execute("UPDATE game.offices SET lease_state=1 WHERE office_id=$1", &[&office]).await.unwrap();
    let kwargs = Value::None;
    let args = [Value::Int(corp), Value::None];
    let call = Call { service: "blueprintManager", method: "GetBlueprintDataByOwner", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let reply = services::dispatch(&mut client, &call).await.unwrap();
    let reply = marshal::loads(&marshal::dumps(&reply)).unwrap();
    assert_eq!(reply.items().unwrap()[0].items().unwrap().len(), 2);
    assert!(reply.items().unwrap()[0].items().unwrap().iter().all(|row| services::value_field(row, "isImpounded") == Some(&Value::Bool(true))));
    assert!(blueprints(&client, stranger.character, None, None).await.is_err());
    let personal = crate::dao::items::creation::rookie_ship(&db, bp_type, actor.character, station_id, FLAG_HANGAR).await.unwrap();
    assert!(blueprints(&client, actor.character, Some(personal), None).await.unwrap().0.iter().any(|row| services::value_field(row, "itemID") == Some(&Value::Int(personal))));
    db.execute("DELETE FROM game.industry_jobs WHERE job_id=$1", &[&job]).await.unwrap();
    db.execute("DELETE FROM game.blueprints WHERE item_id=$1", &[&copy]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, stranger).await.unwrap();
}
