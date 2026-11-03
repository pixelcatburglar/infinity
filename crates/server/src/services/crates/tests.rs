use crate::dao::items::{loading::{load_items, ItemQuery}, transfers};
use crate::services::{Call, CallError, CallResult};
use crate::services::tests::fixture;
use marshal::Value;

async fn rpc(c: &mut crate::client::Client, service: &str, method: &str, args: &[Value], bound: bool) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    crate::services::dispatch(c, &Call { service, method, args, kwargs: &kwargs, bound, bind_params: None }).await
}

fn triples(value: &Value) {
    assert_eq!(marshal::loads(&marshal::dumps(value)).unwrap(), *value);
    for entry in value.items().unwrap() {
        let fields = entry.items().unwrap();
        assert_eq!(fields.len(), 3);
        assert!(fields[0].as_int().unwrap() > 0);
        assert!(fields[1].as_int().unwrap() > 0);
        assert_eq!(fields[2], Value::None);
    }
}

fn refused(result: CallResult) { assert!(matches!(result, Err(CallError::User { .. }))); }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn dispatch_refusals_and_adjacent_methods() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let value = rpc(&mut client, "crateService", "GetCrate", &[Value::Int(1)], false).await.unwrap();
    assert_eq!(value, Value::list([]));
    triples(&value);
    for args in [&[][..], &[Value::Bool(true)][..], &[Value::Int(0)][..], &[Value::Int(i64::MAX)][..]] {
        refused(rpc(&mut client, "crateService", "GetCrate", args, false).await);
    }
    for method in ["GetCrateLocation", "OpenCrate", "ClaimLootFromCrateStack", "CustomOpenCrate", "ClaimLoot", "ClaimSkillBundle", "ClaimFitting"] {
        refused(rpc(&mut client, "crateService", method, &[Value::Int(1)], false).await);
    }
    for (service, method, bound) in [("ship", "GetShipFittingInfo", false), ("skillHandler", "GetSkillBundleInfo", true)] {
        match rpc(&mut client, service, method, &[Value::Int(1)], bound).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, crate::messages::MSG_CUSTOM_NOTIFY);
                assert!(dict.iter().any(|(key, value)| key == "notify" && *value == Value::unicode(crate::messages::crates::METADATA_UNAVAILABLE)));
            }
            other => panic!("expected metadata refusal: {other:?}"),
        }
    }
    assert!(rpc(&mut client, "ship", "GetDirtTimestamp", &[], false).await.unwrap().as_int().is_some());
    assert_eq!(rpc(&mut client, "skillHandler", "GetSkillChangesForISIS", &[], true).await.unwrap(), Value::list([]));
    let kwargs = Value::Dict(vec![]);
    let unknown = Call { service: "crateService", method: "NotACrateMethod", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert!(super::dispatch(&mut client, &unknown).await.is_none());
    assert!(client.outbox.is_empty());
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn previews_require_access_and_preserve_items() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let mut stations = client.sde.map.stations();
    let station = stations.next().unwrap().station_id;
    let remote = stations.find(|s| s.station_id != station).unwrap().station_id;
    drop(stations);
    client.session.apply(vec![("stationid", Value::Int(station)), ("corpid", Value::Int(1))]);
    let ordinary = client.sde.crates.iter().find(|d| d.preview(false, Some(1)).is_ok_and(|v| !v.is_empty())).expect("catalogue requires an ordinary preview").type_id;
    let choice = client.sde.crates.iter().find(|d| d.preview(true, Some(1)).is_ok_and(|v| !v.is_empty())).expect("catalogue requires a choice preview").type_id;
    let type_preview = rpc(&mut client, "crateService", "PeekInsideCrate", &[Value::Int(i64::from(ordinary))], false).await.unwrap();
    triples(&type_preview);
    assert!(!type_preview.items().unwrap().is_empty());
    assert_eq!(rpc(&mut client, "crateService", "PeekInsideCrate", &[Value::Int(i64::from(ordinary))], false).await.unwrap(), type_preview);
    let container_type = client.sde.types.iter().find(|t| t.group_id == Some(crate::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    for (method, type_id) in [("PeekInsideFixedCrate", ordinary), ("CustomGetLootFromFixedCrate", choice)] {
        let local = transfers::insert(&db, type_id, actor.character, station, crate::consts::FLAG_HANGAR, 1).await.unwrap();
        let remote_item = transfers::insert(&db, type_id, actor.character, remote, crate::consts::FLAG_HANGAR, 1).await.unwrap();
        let foreign_item = transfers::insert(&db, type_id, foreign.character, station, crate::consts::FLAG_HANGAR, 1).await.unwrap();
        let missing_parent = transfers::insert(&db, type_id, actor.character, i64::MAX, crate::consts::FLAG_CARGO, 1).await.unwrap();
        let parent = crate::dao::items::creation::rookie_ship(&db, container_type, actor.character, remote, crate::consts::FLAG_HANGAR).await.unwrap();
        let nested = transfers::insert(&db, type_id, actor.character, parent, crate::consts::FLAG_CARGO, 1).await.unwrap();
        let ids = [local, remote_item, foreign_item, missing_parent, parent, nested];
        let before = load_items(&db, ItemQuery::Ids(&ids)).await.unwrap().into_iter().map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info)).collect::<Vec<_>>();
        let args = [Value::Int(local)];
        let first = rpc(&mut client, "crateService", method, &args, false).await.unwrap();
        triples(&first);
        assert!(!first.items().unwrap().is_empty());
        if method == "PeekInsideFixedCrate" { assert_eq!(first, type_preview); }
        assert_eq!(rpc(&mut client, "crateService", method, &args, false).await.unwrap(), first);
        for id in [i64::MAX, remote_item, foreign_item, missing_parent, nested] {
            refused(rpc(&mut client, "crateService", method, &[Value::Int(id)], false).await);
        }
        for action in ["GetCrateLocation", "OpenCrate", "ClaimLootFromCrateStack", "CustomOpenCrate", "ClaimLoot", "ClaimSkillBundle", "ClaimFitting"] {
            refused(rpc(&mut client, "crateService", action, &[Value::Int(local)], false).await);
        }
        let after = load_items(&db, ItemQuery::Ids(&ids)).await.unwrap().into_iter().map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info)).collect::<Vec<_>>();
        assert_eq!(after, before);
    }
    assert!(client.outbox.is_empty());
    fixture::cleanup(&db, foreign).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn native_special_reference_instance_refuses_unavailable_metadata() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let type_id = crate::dao::sde::crates::load(&db).await.unwrap().into_iter()
        .filter(|row| row.data.get("fittings").is_some() || row.data.get("skillBundleID").is_some())
        .filter_map(|row| row.data.get("typeID").and_then(|id| id.as_i64()).and_then(|id| i32::try_from(id).ok()))
        .find(|id| client.sde.crates.get(*id).is_some_and(|def| def.preview(false, Some(1)).is_err()))
        .expect("native special-reference crate must be in the catalogue and unavailable");
    let id = transfers::insert(&db, type_id, actor.character, station, crate::consts::FLAG_HANGAR, 1).await.unwrap();
    let before = load_items(&db, ItemQuery::Id(id)).await.unwrap().into_iter()
        .map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info)).collect::<Vec<_>>();
    assert_eq!(before.len(), 1);
    match rpc(&mut client, "crateService", "PeekInsideFixedCrate", &[Value::Int(id)], false).await {
        Err(CallError::User { msg, dict }) => {
            assert_eq!(msg, crate::messages::MSG_CUSTOM_NOTIFY);
            assert!(dict.iter().any(|(key, value)| key == "notify" && *value == Value::unicode(crate::messages::crates::METADATA_UNAVAILABLE)));
        }
        other => panic!("expected unavailable metadata refusal: {other:?}"),
    }
    let after = load_items(&db, ItemQuery::Id(id)).await.unwrap().into_iter()
        .map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info)).collect::<Vec<_>>();
    assert_eq!(after, before);
    assert!(client.outbox.is_empty());
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn corporate_custody_previews_require_view_role_and_local_reachability() {
    use crate::consts::{FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, ROLE_HANGAR_CAN_QUERY_1, ROLE_JUNIOR_ACCOUNTANT};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let mut stations = client.sde.map.stations();
    let station = stations.next().unwrap().station_id;
    let remote = stations.find(|s| s.station_id != station).unwrap().station_id;
    drop(stations);
    let type_id = client.sde.crates.iter().find(|d| d.preview(false, Some(1)).is_ok_and(|v| !v.is_empty())).expect("catalogue requires an ordinary preview").type_id;
    let corp = fixture::create_corporation(&db, ceo.character, station).await.unwrap();
    fixture::assign_corporation(&db, actor.character, corp, 0).await.unwrap();
    client.session.apply(vec![("stationid", Value::Int(station)), ("corpid", Value::Int(corp))]);
    let office = transfers::insert(&db, type_id, corp, station, crate::consts::FLAG_HANGAR, 1).await.unwrap();
    fixture::create_office(&db, office, corp, station).await.unwrap();
    let deliveries = transfers::insert(&db, type_id, corp, station, FLAG_CORP_DELIVERIES, 1).await.unwrap();
    let division = transfers::insert(&db, type_id, corp, office, FLAG_CORP_SAG1, 1).await.unwrap();
    let ids = [office, deliveries, division];
    let before = load_items(&db, ItemQuery::Ids(&ids)).await.unwrap().into_iter()
        .map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info)).collect::<Vec<_>>();
    assert_eq!(before.len(), ids.len());
    for (item, role) in [(deliveries, ROLE_JUNIOR_ACCOUNTANT), (division, ROLE_HANGAR_CAN_QUERY_1)] {
        fixture::assign_corporation(&db, actor.character, corp, 0).await.unwrap();
        refused(rpc(&mut client, "crateService", "PeekInsideFixedCrate", &[Value::Int(item)], false).await);
        fixture::assign_corporation(&db, actor.character, corp, role).await.unwrap();
        let result = rpc(&mut client, "crateService", "PeekInsideFixedCrate", &[Value::Int(item)], false).await.unwrap();
        triples(&result);
        assert!(!result.items().unwrap().is_empty());
        client.session.apply(vec![("stationid", Value::Int(remote))]);
        refused(rpc(&mut client, "crateService", "PeekInsideFixedCrate", &[Value::Int(item)], false).await);
        client.session.apply(vec![("stationid", Value::Int(station))]);
    }
    let after = load_items(&db, ItemQuery::Ids(&ids)).await.unwrap().into_iter()
        .map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info)).collect::<Vec<_>>();
    assert_eq!(after, before);
    assert!(client.outbox.is_empty());
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
}
