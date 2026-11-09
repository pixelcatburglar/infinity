use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn crystal_batch_excludes_other_personal_station_hangar() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let other = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let ty = client.sde.types.iter().find(|ty| {
        let enabled = fit::base_attribute(Some(ty), client.sde.dogma.attribute(ATTR_CRYSTAL_DAMAGE_ENABLED), ATTR_CRYSTAL_DAMAGE_ENABLED);
        let damage = fit::base_attribute(Some(ty), client.sde.dogma.attribute(ATTR_HULL_DAMAGE), ATTR_HULL_DAMAGE);
        enabled.is_finite() && enabled > 0.0 && damage.is_finite() && (0.0..=1.0).contains(&damage)
    }).expect("SDE contains a damageable crystal");
    let base = fit::base_attribute(Some(ty), client.sde.dogma.attribute(ATTR_HULL_DAMAGE), ATTR_HULL_DAMAGE);
    let own = crate::dao::items::transfers::insert(&db, ty.type_id, actor.character, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let foreign = crate::dao::items::transfers::insert(&db, ty.type_id, other.character, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let args = [Value::list([Value::Int(own), Value::Int(foreign)])];
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "invbroker", method: "GetDamageForCrystals", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let result = crate::services::dispatch(&mut client, &call).await.unwrap();
    let Value::Dict(rows) = result else { panic!("expected damage dictionary") };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, Value::Int(own));
    assert_eq!(rows[0].1, Value::Float(base));
    let item = load_items(&db, &client.sde, ItemQuery::Id(own)).await.unwrap().pop().unwrap();
    assert!(crate::dao::items::inventory::move_item(&db, &item, station, infinity_core::consts::FLAG_HANGAR, actor.character, true).await.unwrap());
    assert!(dogma::set_override(&db, own, ATTR_HULL_DAMAGE, 0.5).await.unwrap());
    let result = crate::services::dispatch(&mut client, &call).await.unwrap();
    let Value::Dict(rows) = result else { panic!("expected damage dictionary") };
    assert_eq!(rows, vec![(Value::Int(own), Value::Float(0.5))]);
    let corp = crate::services::tests::fixture::create_corporation(&db, other.character, station).await.unwrap();
    crate::services::tests::fixture::assign_corporation(&db, actor.character, corp, 0).await.unwrap();
    client.session.apply(vec![("corpid", Value::Int(corp))]);
    let office = crate::dao::items::transfers::insert(&db, item.type_id, corp, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    crate::services::tests::fixture::create_office(&db, office, corp, station).await.unwrap();
    let division = crate::dao::items::transfers::insert(&db, item.type_id, corp, office, infinity_core::consts::FLAG_CORP_SAG1, 1).await.unwrap();
    let corp_args = [Value::list([Value::Int(division)])];
    let corp_call = Call { service: "invbroker", method: "GetDamageForCrystals", args: &corp_args, kwargs: &kwargs, bound: false, bind_params: None };
    assert!(matches!(crate::services::dispatch(&mut client, &corp_call).await.unwrap(), Value::Dict(rows) if rows.is_empty()));
    crate::services::tests::fixture::assign_corporation(&db, actor.character, corp, infinity_core::consts::ROLE_HANGAR_CAN_QUERY_1).await.unwrap();
    assert!(matches!(crate::services::dispatch(&mut client, &corp_call).await.unwrap(), Value::Dict(rows) if rows.len() == 1 && rows[0].0 == Value::Int(division)));
    crate::dao::items::inventory::delete(&db, division).await.unwrap();
    crate::services::tests::fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    crate::dao::items::inventory::delete(&db, own).await.unwrap();
    crate::dao::items::inventory::delete(&db, foreign).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, other).await.unwrap();
}
