use super::*;

#[test]
fn refurbishment_allocation_handles_partial_and_rounds_up_final_component() {
    assert_eq!(allocation(2.5, 1.0, 1).unwrap(), (1, 1.5));
    assert_eq!(allocation(0.5, 1.0, 2).unwrap(), (1, 0.0));
    assert!(allocation(f64::NAN, 1.0, 1).is_err());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn corporate_output_requires_live_office_and_take_role_and_duplicate_inputs_fail() {
    use crate::services::tests::fixture;
    use infinity_core::consts::{FLAG_CORP_SAG1, ROLE_HANGAR_CAN_TAKE_1, TYPE_OFFICE};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let corp = fixture::create_corporation(&db, ceo.character, station).await.unwrap();
    fixture::assign_corporation(&db, actor.character, corp, ROLE_HANGAR_CAN_TAKE_1).await.unwrap();
    let office = crate::dao::items::creation::rookie_ship(&db, TYPE_OFFICE, corp, station, FLAG_HANGAR).await.unwrap();
    fixture::create_office(&db, office, corp, station).await.unwrap();
    let test_db = db.clone();
    let outcome = tokio::spawn(async move {
        client.session.apply(vec![("stationid", Value::Int(station))]);
        let args = [Value::List(vec![Value::Int(office), Value::Int(office)]), Value::Int(office),
            Value::Int(corp), Value::Int(office), Value::Int(i64::from(FLAG_CORP_SAG1))];
        let kwargs = Value::Dict(vec![]);
        let call = Call { service: "repairableShipMgr", method: "OnDisassemble", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(disassemble(&mut client, &call).await.is_err());
        let item = crate::dao::items::loading::Item { item_id: office, type_id: TYPE_OFFICE, owner_id: corp,
            location_id: station, flag_id: FLAG_HANGAR, quantity: 1, singleton: true, group_id: 0, category_id: 0, custom_info: None };
        let different = crate::dao::items::loading::Item { quantity: 2, ..item.clone() };
        let args = [Value::List(vec![item.row(), different.row()]), Value::Dict(vec![])];
        let call = Call { service: "repairableShipMgr", method: "OnRefurbishment", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(refurbish(&mut client, &call).await.is_err());
        let restore = client.sde.repairable_ship.restore_per_component();
        let max_life = client.sde.repairable_ship.max_life();
        let yield_count = (max_life / restore).floor() as i64;
        assert!(yield_count >= 3);
        let source_type = client.sde.types.iter().find(|ty| {
            ty.category_id == Some(infinity_core::consts::CATEGORY_MODULE) && ty.published == Some(true)
                && ty.group_id.is_some_and(|group| !client.sde.repairable_ship.unrepairable(infinity_core::consts::CATEGORY_MODULE, group, ty.type_id))
                && client.sde.repairable_ship.component(ty.type_id).is_some()
        }).unwrap().type_id;
        let component = client.sde.repairable_ship.component(source_type).unwrap();
        let source = crate::dao::items::transfers::insert(&test_db, source_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
        let target = crate::dao::items::creation::rookie_ship(&test_db, source_type, actor.character, station, FLAG_HANGAR).await.unwrap();
        let damage = restore * 1.5;
        test_db.execute("INSERT INTO game.item_service_life (item_id,damage) VALUES ($1,$2)", &[&target, &damage]).await.unwrap();
        let args = [Value::list([Value::Int(source)]), Value::Int(station), Value::Int(actor.character), Value::Int(station), Value::Int(i64::from(FLAG_HANGAR))];
        let call = Call { service: "repairableShipMgr", method: "OnDisassemble", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(disassemble(&mut client, &call).await.unwrap(), Value::list([Value::Int(source)]));
        assert!(test_db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().is_none());
        let count_sql = "SELECT COALESCE(sum(quantity),0)::bigint AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4 AND NOT singleton";
        let count = || { let db = test_db.clone(); async move { db.query_one(count_sql, &[&actor.character, &station, &FLAG_HANGAR, &component]).await.unwrap().get::<_, i64>("n") } };
        assert_eq!(count().await, yield_count);
        let original = load_items(&test_db, &client.sde, ItemQuery::Id(target)).await.unwrap().remove(0).row();
        let other_component = client.sde.types.iter().filter_map(|ty| client.sde.repairable_ship.component(ty.type_id)).find(|&id| id != component).unwrap();
        let args = [Value::list([original.clone()]), Value::Dict(vec![(Value::Int(i64::from(other_component)), Value::Int(1))])];
        let call = Call { service: "repairableShipMgr", method: "OnRefurbishment", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(refurbish(&mut client, &call).await.is_err());
        let args = [Value::list([original.clone()]), Value::Dict(vec![(Value::Int(i64::from(component)), Value::Int(yield_count + 1))])];
        let call = Call { service: "repairableShipMgr", method: "OnRefurbishment", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(refurbish(&mut client, &call).await.is_err());
        let args = [Value::list([original.clone()]), Value::Dict(vec![(Value::Int(i64::from(component)), Value::Int(1))])];
        let call = Call { service: "repairableShipMgr", method: "OnRefurbishment", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(refurbish(&mut client, &call).await.unwrap(), Value::Dict(vec![(original.clone(), Value::Float(damage - restore))]));
        let stored: f64 = test_db.query_one("SELECT damage FROM game.item_service_life WHERE item_id=$1", &[&target]).await.unwrap().get("damage");
        assert_eq!(stored, damage - restore);
        assert_eq!(count().await, yield_count - 1);
        let args = [Value::list([original.clone()]), Value::Dict(vec![(Value::Int(i64::from(component)), Value::Int(2))])];
        let call = Call { service: "repairableShipMgr", method: "OnRefurbishment", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(refurbish(&mut client, &call).await.is_err());
        assert_eq!(count().await, yield_count - 1);
        let args = [Value::list([original.clone()]), Value::Dict(vec![(Value::Int(i64::from(component)), Value::Int(1))])];
        let call = Call { service: "repairableShipMgr", method: "OnRefurbishment", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(refurbish(&mut client, &call).await.unwrap(), Value::Dict(vec![(original, Value::Float(0.0))]));
        assert_eq!(count().await, yield_count - 2);
        let stored: f64 = test_db.query_one("SELECT damage FROM game.item_service_life WHERE item_id=$1", &[&target]).await.unwrap().get("damage");
        assert_eq!(stored, 0.0);

        let (first_type, second_type, shared) = client.sde.types.iter().filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).find_map(|ty| {
            let shared = client.sde.repairable_ship.component(ty.type_id)?;
            client.sde.types.iter().find(|other| other.category_id == ty.category_id && other.type_id != ty.type_id && client.sde.repairable_ship.component(other.type_id) == Some(shared))
                .map(|other| (ty.type_id, other.type_id, shared))
        }).expect("two ship types must share a refurbished component");
        let first = crate::dao::items::creation::rookie_ship(&test_db, first_type, actor.character, station, FLAG_HANGAR).await.unwrap();
        let second = crate::dao::items::creation::rookie_ship(&test_db, second_type, actor.character, station, FLAG_HANGAR).await.unwrap();
        test_db.execute("INSERT INTO game.item_service_life (item_id,damage) VALUES ($1,$3),($2,$3)", &[&first, &second, &restore]).await.unwrap();
        let before = test_db.query_one(count_sql, &[&actor.character, &station, &FLAG_HANGAR, &shared]).await.unwrap().get::<_, i64>("n");
        crate::dao::items::transfers::insert(&test_db, shared, actor.character, station, FLAG_HANGAR, 2).await.unwrap();
        let originals = load_items(&test_db, &client.sde, ItemQuery::Ids(&[first, second])).await.unwrap().into_iter().map(|item| item.row()).collect::<Vec<_>>();
        let args = [Value::List(originals.clone()), Value::Dict(vec![(Value::Int(i64::from(shared)), Value::Int(2))])];
        let call = Call { service: "repairableShipMgr", method: "OnRefurbishment", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(refurbish(&mut client, &call).await.unwrap(), Value::Dict(originals.into_iter().map(|row| (row, Value::Float(0.0))).collect()));
        let damages = life_dao::get_many(&test_db, &[ItemId::new(first).unwrap(), ItemId::new(second).unwrap()]).await.unwrap();
        assert_eq!((damages[&ItemId::new(first).unwrap()], damages[&ItemId::new(second).unwrap()]), (0.0, 0.0));
        assert_eq!(test_db.query_one(count_sql, &[&actor.character, &station, &FLAG_HANGAR, &shared]).await.unwrap().get::<_, i64>("n"), before);
        assert_eq!(resolve_output(&test_db, actor.character, station, corp, office, FLAG_CORP_SAG1).await.unwrap(), (corp, office, FLAG_CORP_SAG1));
        fixture::assign_corporation(&test_db, actor.character, corp, 0).await.unwrap();
        assert!(resolve_output(&test_db, actor.character, station, corp, office, FLAG_CORP_SAG1).await.is_err());
        assert!(resolve_output(&test_db, actor.character, station, actor.character, station, FLAG_CORP_SAG1).await.is_err());
    }).await;
    fixture::cleanup_corporation(&db, corp, &[actor.character, ceo.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    outcome.unwrap();
}
