use crate::dao::items::{loading::{load_items, ItemQuery}, transfers};
use crate::services::{Call, CallError, CallResult};
use crate::services::tests::fixture;
use marshal::Value;

mod container_capacity;

async fn rpc(c: &mut crate::client::Client, service: &str, method: &str, args: &[Value], bound: bool) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    crate::services::dispatch(c, &Call { service, method, args, kwargs: &kwargs, bound, bind_params: None }).await
}

fn triples(value: &Value) {
    assert_eq!(marshal::loads(&marshal::dumps(value)).unwrap(), *value);
    for entry in value.items().unwrap() {
        let fields = entry.items().unwrap();
        assert_eq!(fields.len(), 3);
        assert!(matches!(&fields[2], Value::Int(quantity) if *quantity > 0));
    }
}

// recruitment_crate_controller.py builds (typeID, count, None) locally;
// crate preview consumers read the first two fields.
fn preview_triples(value: &Value) {
    assert_eq!(marshal::loads(&marshal::dumps(value)).unwrap(), *value);
    for entry in value.items().unwrap() {
        let fields = entry.items().unwrap();
        assert_eq!(fields.len(), 3);
        assert!(matches!(&fields[2], Value::None) || matches!(&fields[2], Value::Int(quantity) if *quantity > 0));
    }
}

fn refused(result: CallResult) { assert!(matches!(result, Err(CallError::User { .. }))); }

#[test]
fn opening_response_shapes_are_exact() {
    let direct = Value::list([Value::tuple([Value::Int(41), Value::Int(9001), Value::Int(2)])]);
    assert_eq!(direct.items().unwrap()[0].items().unwrap().len(), 3);
    let stack = Value::tuple([Value::Int(30), Value::Int(30)]);
    assert_eq!(stack.items().unwrap().len(), 2);
}

#[test]
fn reward_quantity_checks_multiplication_and_inventory_range() {
    for (per_open, amount, expected) in [
        (i64::from(i32::MAX), 1, Some(i64::from(i32::MAX))),
        (i64::from(i32::MAX / 2), 2, Some(i64::from(i32::MAX - 1))),
        (i64::from(i32::MAX), 2, Some(i64::from(i32::MAX) * 2)),
        (i64::MAX, 2, None),
    ] {
        assert_eq!(super::opening::reward_quantity(per_open, amount), expected);
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn dispatch_refusals_and_adjacent_methods() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let value = rpc(&mut client, "crateService", "GetCrate", &[Value::Int(1)], false).await.unwrap();
    assert_eq!(value, Value::list([]));
    preview_triples(&value);
    for args in [&[][..], &[Value::Bool(true)][..], &[Value::Int(0)][..], &[Value::Int(i64::MAX)][..]] {
        refused(rpc(&mut client, "crateService", "GetCrate", args, false).await);
    }
    for args in [&[][..], &[Value::Bool(true)][..], &[Value::Int(0)][..], &[Value::Int(-1)][..]] {
        refused(rpc(&mut client, "crateService", "GetCrateLocation", args, false).await);
    }
    for method in ["GetCrateLocation", "CustomOpenCrate", "ClaimLoot", "ClaimSkillBundle", "ClaimFitting"] {
        refused(rpc(&mut client, "crateService", method, &[Value::Int(1)], false).await);
    }
    for (service, method, bound) in [("ship", "GetShipFittingInfo", false), ("skillHandler", "GetSkillBundleInfo", true)] {
        match rpc(&mut client, service, method, &[Value::Int(1)], bound).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert!(dict.iter().any(|(key, value)| key == "notify" && *value == Value::unicode(infinity_core::messages::crates::METADATA_UNAVAILABLE)));
            }
            other => panic!("expected metadata refusal: {other:?}"),
        }
    }
    for amount in [Value::None, Value::Bool(true), Value::Int(0), Value::Int(31), Value::Int(i64::MAX)] {
        refused(rpc(&mut client, "crateService", "ClaimLootFromCrateStack", &[Value::Int(1), amount], false).await);
    }
    assert_eq!(rpc(&mut client, "ship", "CheckEnoughSubSystemToUndock", &[], false).await.unwrap(), Value::Bool(true));
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
    preview_triples(&type_preview);
    assert!(!type_preview.items().unwrap().is_empty());
    assert_eq!(rpc(&mut client, "crateService", "PeekInsideCrate", &[Value::Int(i64::from(ordinary))], false).await.unwrap(), type_preview);
    let container_type = client.sde.types.iter().find(|t| t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    for (method, type_id) in [("PeekInsideFixedCrate", ordinary), ("CustomGetLootFromFixedCrate", choice)] {
        let local = transfers::insert(&db, type_id, actor.character, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
        let remote_item = transfers::insert(&db, type_id, actor.character, remote, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
        let foreign_item = transfers::insert(&db, type_id, foreign.character, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
        let missing_parent = transfers::insert(&db, type_id, actor.character, i64::MAX, infinity_core::consts::FLAG_CARGO, 1).await.unwrap();
        let parent = crate::dao::items::creation::rookie_ship(&db, container_type, actor.character, remote, infinity_core::consts::FLAG_HANGAR).await.unwrap();
        let nested = transfers::insert(&db, type_id, actor.character, parent, infinity_core::consts::FLAG_CARGO, 1).await.unwrap();
        let ids = [local, remote_item, foreign_item, missing_parent, parent, nested];
        let before = load_items(&db, ItemQuery::Ids(&ids)).await.unwrap().into_iter().map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info)).collect::<Vec<_>>();
        let args = [Value::Int(local)];
        let first = rpc(&mut client, "crateService", method, &args, false).await.unwrap();
        preview_triples(&first);
        assert!(!first.items().unwrap().is_empty());
        if method == "PeekInsideFixedCrate" { assert_eq!(first, type_preview); }
        assert_eq!(rpc(&mut client, "crateService", method, &args, false).await.unwrap(), first);
        for id in [i64::MAX, remote_item, foreign_item, missing_parent, nested] {
            refused(rpc(&mut client, "crateService", method, &[Value::Int(id)], false).await);
        }
        for action in ["CustomOpenCrate", "ClaimLoot", "ClaimSkillBundle", "ClaimFitting"] {
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
    let id = transfers::insert(&db, type_id, actor.character, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let before = load_items(&db, ItemQuery::Id(id)).await.unwrap().into_iter()
        .map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info)).collect::<Vec<_>>();
    assert_eq!(before.len(), 1);
    match rpc(&mut client, "crateService", "PeekInsideFixedCrate", &[Value::Int(id)], false).await {
        Err(CallError::User { msg, dict }) => {
            assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
            assert!(dict.iter().any(|(key, value)| key == "notify" && *value == Value::unicode(infinity_core::messages::crates::METADATA_UNAVAILABLE)));
        }
        other => panic!("expected unavailable metadata refusal: {other:?}"),
    }
    refused(rpc(&mut client, "crateService", "GetCrateLocation", &[Value::Int(id)], false).await);
    let after = load_items(&db, ItemQuery::Id(id)).await.unwrap().into_iter()
        .map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info)).collect::<Vec<_>>();
    assert_eq!(after, before);
    assert!(client.outbox.is_empty());
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn corporate_custody_previews_require_view_role_and_local_reachability() {
    use infinity_core::consts::{FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, ROLE_HANGAR_CAN_QUERY_1, ROLE_JUNIOR_ACCOUNTANT};
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
    let office = transfers::insert(&db, type_id, corp, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    fixture::create_office(&db, office, corp, station).await.unwrap();
    let deliveries = transfers::insert(&db, type_id, corp, station, FLAG_CORP_DELIVERIES, 1).await.unwrap();
    let division = transfers::insert(&db, type_id, corp, office, FLAG_CORP_SAG1, 1).await.unwrap();
    let ids = [office, deliveries, division];
    let before = load_items(&db, ItemQuery::Ids(&ids)).await.unwrap().into_iter()
        .map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info)).collect::<Vec<_>>();
    assert_eq!(before.len(), ids.len());
    refused(rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(office)], false).await);
    assert_eq!(load_items(&db, ItemQuery::Id(office)).await.unwrap().len(), 1);
    for (item, role) in [(deliveries, ROLE_JUNIOR_ACCOUNTANT), (division, ROLE_HANGAR_CAN_QUERY_1)] {
        fixture::assign_corporation(&db, actor.character, corp, 0).await.unwrap();
        refused(rpc(&mut client, "crateService", "PeekInsideFixedCrate", &[Value::Int(item)], false).await);
        fixture::assign_corporation(&db, actor.character, corp, role).await.unwrap();
        let result = rpc(&mut client, "crateService", "PeekInsideFixedCrate", &[Value::Int(item)], false).await.unwrap();
        preview_triples(&result);
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

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn ordinary_open_claim_boundary_and_malformed_quantities_are_atomic() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let (crate_type, expected) = {
        let definition = client.sde.crates.iter()
            .find(|definition| definition.preview(false, None).is_ok_and(|rewards| !rewards.is_empty()))
            .expect("SDE must contain an ordinary direct-list crate");
        (definition.type_id, definition.preview(false, None).unwrap())
    };
    let source = transfers::insert(&db, crate_type, actor.character, station, infinity_core::consts::FLAG_HANGAR, 32).await.unwrap();
    let items_before = db.query_one("SELECT count(*) FROM game.items WHERE owner_id=$1", &[&actor.character]).await.unwrap().get::<_, i64>("count");
    let destination = Value::tuple([Value::Int(station), Value::Int(i64::from(infinity_core::consts::FLAG_HANGAR))]);
    for _ in 0..2 {
        let result = rpc(&mut client, "crateService", "GetCrateLocation", &[Value::Int(source)], false).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&result)).unwrap(), destination);
    }
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get::<_, i64>("quantity"), 32);
    assert_eq!(db.query_one("SELECT count(*) FROM game.items WHERE owner_id=$1", &[&actor.character]).await.unwrap().get::<_, i64>("count"), items_before);
    assert_eq!(db.query_one("SELECT count(*) FROM game.crate_rewards WHERE character_id=$1", &[&actor.character]).await.unwrap().get::<_, i64>("count"), 0);
    assert!(client.outbox.is_empty());
    let direct = rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await.unwrap();
    triples(&direct);
    let direct_rows = direct.items().unwrap();
    assert_eq!(direct_rows.len(), expected.len());
    for (entry, (type_id, per_open)) in direct_rows.iter().zip(expected.iter()) {
        let fields = entry.items().unwrap();
        let item_id = fields[0].as_int().unwrap();
        let quantity = fields[2].as_int().unwrap();
        assert_eq!(fields[1].as_int(), Some(i64::from(*type_id)));
        assert_eq!(quantity, *per_open);
        let item = db.query_one("SELECT type_id, owner_id, location_id, flag_id, quantity, singleton FROM game.items WHERE item_id=$1", &[&item_id]).await.unwrap();
        assert_eq!(item.get::<_, i32>("type_id"), *type_id);
        assert_eq!(item.get::<_, i64>("owner_id"), actor.character);
        assert_eq!(item.get::<_, i64>("location_id"), station);
        assert_eq!(item.get::<_, i32>("flag_id"), infinity_core::consts::FLAG_HANGAR);
        assert_eq!(item.get::<_, i64>("quantity"), *per_open);
        assert!(!item.get::<_, bool>("singleton"));
        let receipt = db.query_one("SELECT character_id, crate_type_id, destination_location, destination_flag, opened_at FROM game.crate_rewards WHERE item_id=$1", &[&item_id]).await.unwrap();
        assert_eq!(receipt.get::<_, i64>("character_id"), actor.character);
        assert_eq!(receipt.get::<_, i32>("crate_type_id"), crate_type);
        assert_eq!(receipt.get::<_, i64>("destination_location"), station);
        assert_eq!(receipt.get::<_, i32>("destination_flag"), infinity_core::consts::FLAG_HANGAR);
        assert!(receipt.get::<_, i64>("opened_at") > 0);
        assert_eq!(rpc(&mut client, "crateService", "ClaimLoot", &[Value::Int(item_id), Value::Int(station), Value::Int(i64::from(infinity_core::consts::FLAG_HANGAR))], false).await.unwrap(), Value::None);
    }
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get::<_, i64>("quantity"), 31);
    let reward_count: i64 = db.query_one("SELECT count(*) AS count FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2", &[&actor.character, &crate_type]).await.unwrap().get("count");
    assert_eq!(reward_count, i64::try_from(expected.len()).unwrap());
    let claimed = rpc(&mut client, "crateService", "ClaimLootFromCrateStack", &[Value::Int(source), Value::Int(30)], false).await.unwrap();
    assert_eq!(claimed, Value::tuple([Value::Int(30), Value::Int(30)]));
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get::<_, i64>("quantity"), 1);
    refused(rpc(&mut client, "crateService", "ClaimLootFromCrateStack", &[Value::Int(source), Value::Int(31)], false).await);
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get::<_, i64>("quantity"), 1);
    let second = rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await.unwrap();
    triples(&second);
    assert_eq!(second.items().unwrap().len(), expected.len());
    assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().is_none());
    refused(rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await);
    let malformed_source = transfers::insert(&db, crate_type, actor.character, station, infinity_core::consts::FLAG_HANGAR, 4).await.unwrap();
    let reward_ids = db.query("SELECT item_id FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2 ORDER BY item_id", &[&actor.character, &crate_type]).await.unwrap()
        .into_iter().map(|row| row.get::<_, i64>("item_id")).collect::<Vec<_>>();
    let reward_count = i64::try_from(reward_ids.len()).unwrap();
    let outbox_before = client.outbox.len();
    for amount in [Value::None, Value::Bool(true), Value::Int(0), Value::Int(31), Value::Int(i64::MAX)] {
        refused(rpc(&mut client, "crateService", "ClaimLootFromCrateStack", &[Value::Int(malformed_source), amount], false).await);
        assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&malformed_source]).await.unwrap().get::<_, i64>("quantity"), 4);
        let current_ids = db.query("SELECT item_id FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2 ORDER BY item_id", &[&actor.character, &crate_type]).await.unwrap()
            .into_iter().map(|row| row.get::<_, i64>("item_id")).collect::<Vec<_>>();
        assert_eq!(current_ids, reward_ids);
        assert_eq!(i64::try_from(current_ids.len()).unwrap(), reward_count);
        assert!(db.query_opt("SELECT item_id FROM game.crate_rewards WHERE item_id=$1 AND character_id=$2", &[&malformed_source, &actor.character]).await.unwrap().is_none());
    }
    assert_eq!(client.outbox.len(), outbox_before);
    db.execute("DELETE FROM game.crate_rewards WHERE character_id=$1", &[&actor.character]).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn opening_rolls_back_on_receipt_primary_key_conflict() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let definition = client.sde.crates.iter()
        .find(|definition| definition.preview(false, None).is_ok_and(|rewards| !rewards.is_empty()))
        .expect("SDE must contain an ordinary direct-list crate");
    let crate_type = definition.type_id;
    let expected = definition.preview(false, None).expect("crate definition preflight must succeed");
    assert!(!expected.is_empty());
    let source = transfers::insert(&db, crate_type, actor.character, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let source_before = db.query_one(
        "SELECT type_id, owner_id, location_id, flag_id, quantity, singleton, online FROM game.items WHERE item_id=$1",
        &[&source],
    ).await.unwrap();
    let source_before = (
        source_before.get::<_, i32>("type_id"),
        source_before.get::<_, i64>("owner_id"),
        source_before.get::<_, i64>("location_id"),
        source_before.get::<_, i32>("flag_id"),
        source_before.get::<_, i64>("quantity"),
        source_before.get::<_, bool>("singleton"),
        source_before.get::<_, bool>("online"),
    );

    let sequence = db.query_one("SELECT last_value, is_called FROM game.items_item_id_seq", &[]).await.unwrap();
    let previous_value: i64 = sequence.get("last_value");
    let previous_called: bool = sequence.get("is_called");
    let reserved_start: i64 = db.query_one("SELECT nextval('game.items_item_id_seq') AS id", &[]).await.unwrap().get("id");
    let safety_margin = 1_i64;
    let reserved_end = reserved_start + i64::try_from(expected.len()).unwrap() + safety_margin;
    db.execute(
        "DELETE FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2 AND item_id BETWEEN $3 AND $4",
        &[&actor.character, &crate_type, &reserved_start, &reserved_end],
    ).await.unwrap();
    db.execute(
        "INSERT INTO game.crate_rewards (item_id, character_id, crate_type_id, destination_location, destination_flag, opened_at)
         SELECT reserved.item_id, $1, $2, $3, $4, $5 FROM generate_series($6::bigint, $7::bigint) AS reserved(item_id)",
        &[&actor.character, &crate_type, &station, &infinity_core::consts::FLAG_HANGAR, &crate::client::filetime_now(), &reserved_start, &reserved_end],
    ).await.unwrap();
    db.query_one("SELECT setval('game.items_item_id_seq', $1, false)", &[&reserved_start]).await.unwrap();

    let result = rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await;
    assert!(result.is_err());
    let source_after = db.query_one(
        "SELECT type_id, owner_id, location_id, flag_id, quantity, singleton, online FROM game.items WHERE item_id=$1",
        &[&source],
    ).await.unwrap();
    assert_eq!(
        (
            source_after.get::<_, i32>("type_id"),
            source_after.get::<_, i64>("owner_id"),
            source_after.get::<_, i64>("location_id"),
            source_after.get::<_, i32>("flag_id"),
            source_after.get::<_, i64>("quantity"),
            source_after.get::<_, bool>("singleton"),
            source_after.get::<_, bool>("online"),
        ),
        source_before,
    );
    let reserved_items: i64 = db.query_one(
        "SELECT count(*) AS count FROM game.items WHERE item_id BETWEEN $1 AND $2",
        &[&reserved_start, &reserved_end],
    ).await.unwrap().get("count");
    assert_eq!(reserved_items, 0);
    let reserved_receipts: i64 = db.query_one(
        "SELECT count(*) AS count FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2 AND item_id BETWEEN $3 AND $4",
        &[&actor.character, &crate_type, &reserved_start, &reserved_end],
    ).await.unwrap().get("count");
    assert_eq!(reserved_receipts, reserved_end - reserved_start + 1);
    assert!(client.outbox.is_empty());

    db.execute("DELETE FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2 AND item_id BETWEEN $3 AND $4", &[&actor.character, &crate_type, &reserved_start, &reserved_end]).await.unwrap();
    db.query_one("SELECT setval('game.items_item_id_seq', $1, $2)", &[&previous_value, &previous_called]).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn corporate_opening_requires_take_and_awards_the_opener() {
    use infinity_core::consts::{FLAG_CORP_SAG1, FLAG_HANGAR, ROLE_HANGAR_CAN_TAKE_1};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let crate_type = client.sde.crates.iter()
        .find(|d| d.preview(false, Some(1)).is_ok_and(|v| !v.is_empty()))
        .expect("SDE must contain an ordinary direct-list crate").type_id;
    let corp = fixture::create_corporation(&db, ceo.character, station).await.unwrap();
    fixture::assign_corporation(&db, actor.character, corp, 0).await.unwrap();
    client.session.apply(vec![("stationid", Value::Int(station)), ("corpid", Value::Int(corp))]);
    let office = transfers::insert(&db, crate_type, corp, station, FLAG_HANGAR, 1).await.unwrap();
    fixture::create_office(&db, office, corp, station).await.unwrap();
    let source = transfers::insert(&db, crate_type, corp, office, FLAG_CORP_SAG1, 1).await.unwrap();
    let before = client.outbox.len();
    refused(rpc(&mut client, "crateService", "GetCrateLocation", &[Value::Int(source)], false).await);
    refused(rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await);
    assert_eq!(db.query_one("SELECT owner_id, quantity FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get::<_, i64>("owner_id"), corp);
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get::<_, i64>("quantity"), 1);
    assert_eq!(db.query_one("SELECT count(*) FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2", &[&actor.character, &crate_type]).await.unwrap().get::<_, i64>("count"), 0);
    assert_eq!(client.outbox.len(), before);
    fixture::assign_corporation(&db, actor.character, corp, ROLE_HANGAR_CAN_TAKE_1).await.unwrap();
    let destination = rpc(&mut client, "crateService", "GetCrateLocation", &[Value::Int(source)], false).await.unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&destination)).unwrap(), Value::tuple([Value::Int(station), Value::Int(i64::from(FLAG_HANGAR))]));
    assert_eq!(client.outbox.len(), before);
    let result = rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await.unwrap();
    let rows = result.items().unwrap();
    assert!(!rows.is_empty());
    for row in rows {
        let fields = row.items().unwrap();
        let item_id = fields[0].as_int().unwrap();
        let item = db.query_one("SELECT owner_id, location_id, flag_id FROM game.items WHERE item_id=$1", &[&item_id]).await.unwrap();
        assert_eq!(item.get::<_, i64>("owner_id"), actor.character);
        assert_eq!(item.get::<_, i64>("location_id"), station);
        assert_eq!(item.get::<_, i32>("flag_id"), FLAG_HANGAR);
        let receipt = db.query_one("SELECT character_id, destination_location, destination_flag FROM game.crate_rewards WHERE item_id=$1", &[&item_id]).await.unwrap();
        assert_eq!(receipt.get::<_, i64>("character_id"), actor.character);
        assert_eq!(receipt.get::<_, i64>("destination_location"), station);
        assert_eq!(receipt.get::<_, i32>("destination_flag"), FLAG_HANGAR);
    }
    assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().is_none());
    let reward_ids = db.query("SELECT item_id FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2", &[&actor.character, &crate_type]).await.unwrap()
        .into_iter().map(|row| row.get::<_, i64>("item_id")).collect::<Vec<_>>();
    db.execute("DELETE FROM game.crate_rewards WHERE item_id = ANY($1)", &[&reward_ids]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&reward_ids]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn opening_in_space_targets_active_ship_cargo() {
    use infinity_core::consts::FLAG_CARGO;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let system = client.sde.map.systems().next().unwrap().solar_system_id;
    let ship_type = client.sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP) && ty.capacity.is_some()).expect("SDE must contain a ship with capacity").type_id;
    let ship = transfers::insert(&db, ship_type, actor.character, system, 0, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=$2, solar_system_id=$3, station_id=NULL WHERE character_id=$1", &[&actor.character, &ship, &system]).await.unwrap();
    client.world.register_pilot(actor.character, client.notify_tx.clone());
    client.world.add_ship(system, crate::space::Ball {
        id: ship, type_id: ship_type, owner_id: actor.character, corp_id: 1, alliance_id: -1,
        char_id: Some(actor.character), signature_radius: Some(1.0), mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: true,
        massive: true, warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }).await.unwrap();
    let crate_type = client.sde.crates.iter().find(|d| d.preview(false, None).is_ok_and(|v| !v.is_empty())).expect("ordinary crate").type_id;
    let source = transfers::insert(&db, crate_type, actor.character, ship, FLAG_CARGO, 1).await.unwrap();
    client.session.apply(vec![("stationid", Value::None), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    let destination = rpc(&mut client, "crateService", "GetCrateLocation", &[Value::Int(source)], false).await.unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&destination)).unwrap(), Value::tuple([Value::Int(ship), Value::Int(i64::from(FLAG_CARGO))]));
    assert!(client.outbox.is_empty());
    let result = rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await.unwrap();
    for row in result.items().unwrap() {
        let item_id = row.items().unwrap()[0].as_int().unwrap();
        let item = db.query_one("SELECT location_id, flag_id FROM game.items WHERE item_id=$1", &[&item_id]).await.unwrap();
        assert_eq!(item.get::<_, i64>("location_id"), ship);
        assert_eq!(item.get::<_, i32>("flag_id"), FLAG_CARGO);
        let receipt = db.query_one("SELECT destination_location, destination_flag FROM game.crate_rewards WHERE item_id=$1", &[&item_id]).await.unwrap();
        assert_eq!(receipt.get::<_, i64>("destination_location"), ship);
        assert_eq!(receipt.get::<_, i32>("destination_flag"), FLAG_CARGO);
    }
    let reward_ids = db.query("SELECT item_id FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2", &[&actor.character, &crate_type]).await.unwrap()
        .into_iter().map(|row| row.get::<_, i64>("item_id")).collect::<Vec<_>>();
    db.execute("DELETE FROM game.crate_rewards WHERE item_id = ANY($1)", &[&reward_ids]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&reward_ids]).await.unwrap();
    client.world.remove_ball(system, ship).await;
    client.world.unregister_pilot_generation(actor.character, None);
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn opening_refuses_when_active_ship_cargo_is_full() {
    use infinity_core::consts::FLAG_CARGO;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let system = client.sde.map.systems().next().unwrap().solar_system_id;
    let ship_data = client.sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP) && ty.capacity.is_some()).expect("ship capacity in SDE");
    let ship = transfers::insert(&db, ship_data.type_id, actor.character, system, 0, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=$2, solar_system_id=$3, station_id=NULL WHERE character_id=$1", &[&actor.character, &ship, &system]).await.unwrap();
    client.world.register_pilot(actor.character, client.notify_tx.clone());
    client.world.add_ship(system, crate::space::Ball {
        id: ship, type_id: ship_data.type_id, owner_id: actor.character, corp_id: 1, alliance_id: -1,
        char_id: Some(actor.character), signature_radius: Some(1.0), mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: true,
        massive: true, warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }).await.unwrap();
    let definition = client.sde.crates.iter().find_map(|d| {
        let rewards = d.preview(false, None).ok()?;
        (!rewards.is_empty() && rewards.iter().any(|(id, quantity)| client.sde.types.get(*id).and_then(|t| t.volume).is_some_and(|v| v > 0.0 && v * (*quantity as f64) > 0.0))).then_some((d.type_id, rewards))
    }).expect("ordinary crate with volumetric rewards");
    let (crate_type, _rewards) = definition;
    let (fill_type, fill_volume) = client.sde.types.iter().find_map(|ty| ty.volume.filter(|v| *v > 0.0).map(|v| (ty.type_id, v))).unwrap();
    let capacity = crate::services::inventory::hold_storage::capacity(&db, &client.sde, actor.character, ship, FLAG_CARGO).await.unwrap().unwrap().0;
    let fill_quantity = (capacity / fill_volume).ceil() as i64;
    let filler = transfers::insert(&db, fill_type, actor.character, ship, FLAG_CARGO, fill_quantity.max(1)).await.unwrap();
    let source = transfers::insert(&db, crate_type, actor.character, ship, FLAG_CARGO, 1).await.unwrap();
    client.session.apply(vec![("stationid", Value::None), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    let before = client.outbox.len();
    let reward_count_before: i64 = db.query_one("SELECT count(*) AS count FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2", &[&actor.character, &crate_type]).await.unwrap().get("count");
    refused(rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await);
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get::<_, i64>("quantity"), 1);
    assert_eq!(db.query_one("SELECT count(*) AS count FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2", &[&actor.character, &crate_type]).await.unwrap().get::<_, i64>("count"), reward_count_before);
    assert_eq!(client.outbox.len(), before);
    db.execute("DELETE FROM game.items WHERE item_id IN ($1,$2,$3)", &[&ship, &filler, &source]).await.unwrap();
    client.world.remove_ball(system, ship).await;
    client.world.unregister_pilot_generation(actor.character, None);
    fixture::cleanup(&db, actor).await.unwrap();
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn claim_loot_acknowledges_only_owned_receipts_and_survives_consumption() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let other = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let (mut second, _peer2) = crate::services::tests::client(db.clone(), actor.character).await;
    let (mut foreign, _peer3) = crate::services::tests::client(db.clone(), other.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let flag = infinity_core::consts::FLAG_HANGAR;
    let item = transfers::insert(&db, 34, actor.character, station, flag, 1).await.unwrap();
    crate::dao::items::crates::insert(&db, crate::dao::items::crates::RewardReceipt {
        item_id: item, character_id: actor.character, crate_type_id: 34,
        destination_location: station, destination_flag: flag, opened_at: crate::client::filetime_now(),
    }).await.unwrap();
    let args = [Value::Int(item), Value::Int(station), Value::Int(i64::from(flag))];
    let before = client.outbox.len();
    refused(rpc(&mut foreign, "crateService", "ClaimLoot", &args, false).await);
    refused(rpc(&mut client, "crateService", "ClaimLoot", &[Value::Int(item), Value::Int(station + 1), Value::Int(i64::from(flag))], false).await);
    refused(rpc(&mut client, "crateService", "ClaimLoot", &[Value::Int(item), Value::Int(station), Value::Int(i64::from(flag) + 1)], false).await);
    refused(rpc(&mut client, "crateService", "ClaimLoot", &[Value::Int(i64::MAX), Value::Int(station), Value::Int(i64::from(flag))], false).await);
    refused(rpc(&mut client, "crateService", "ClaimLoot", &[Value::Int(-1), Value::Int(station), Value::Int(i64::from(flag))], false).await);
    refused(rpc(&mut client, "crateService", "ClaimLoot", &args[..2], false).await);
    refused(rpc(&mut client, "crateService", "ClaimLoot", &[Value::Int(item), Value::Int(station), Value::Bool(true)], false).await);
    refused(rpc(&mut client, "crateService", "ClaimLoot", &[Value::Int(item), Value::Int(station), Value::Int(i64::MAX)], false).await);
    assert_eq!(rpc(&mut client, "crateService", "ClaimLoot", &args, false).await.unwrap(), Value::None);
    let timestamp: i64 = db.query_one("SELECT acknowledged_at FROM game.crate_rewards WHERE item_id=$1", &[&item]).await.unwrap().get("acknowledged_at");
    assert_eq!(rpc(&mut client, "crateService", "ClaimLoot", &args, false).await.unwrap(), Value::None);
    assert_eq!(rpc(&mut second, "crateService", "ClaimLoot", &args, false).await.unwrap(), Value::None);
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
    assert_eq!(rpc(&mut second, "crateService", "ClaimLoot", &args, false).await.unwrap(), Value::None);
    assert_eq!(db.query_one("SELECT acknowledged_at FROM game.crate_rewards WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i64>("acknowledged_at"), timestamp);
    let deleted = transfers::insert(&db, 34, actor.character, station, flag, 1).await.unwrap();
    crate::dao::items::crates::insert(&db, crate::dao::items::crates::RewardReceipt {
        item_id: deleted, character_id: actor.character, crate_type_id: 34,
        destination_location: station, destination_flag: flag, opened_at: crate::client::filetime_now(),
    }).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&deleted]).await.unwrap();
    let deleted_args = [Value::Int(deleted), Value::Int(station), Value::Int(i64::from(flag))];
    let items_before = load_items(&db, ItemQuery::Id(deleted)).await.unwrap();
    assert!(items_before.is_empty());
    let inventory_before = db.query("SELECT item_id, location_id, flag_id, quantity FROM game.items WHERE owner_id=$1 ORDER BY item_id", &[&actor.character]).await.unwrap()
        .into_iter().map(|row| (row.get::<_, i64>("item_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, i64>("quantity"))).collect::<Vec<_>>();
    let outbox_before = second.outbox.len();
    assert_eq!(rpc(&mut second, "crateService", "ClaimLoot", &deleted_args, false).await.unwrap(), Value::None);
    let deleted_timestamp: i64 = db.query_one("SELECT acknowledged_at FROM game.crate_rewards WHERE item_id=$1", &[&deleted]).await.unwrap().get("acknowledged_at");
    assert!(deleted_timestamp > 0);
    assert_eq!(rpc(&mut second, "crateService", "ClaimLoot", &deleted_args, false).await.unwrap(), Value::None);
    assert_eq!(db.query_one("SELECT acknowledged_at FROM game.crate_rewards WHERE item_id=$1", &[&deleted]).await.unwrap().get::<_, i64>("acknowledged_at"), deleted_timestamp);
    assert!(load_items(&db, ItemQuery::Id(deleted)).await.unwrap().is_empty());
    let inventory_after = db.query("SELECT item_id, location_id, flag_id, quantity FROM game.items WHERE owner_id=$1 ORDER BY item_id", &[&actor.character]).await.unwrap()
        .into_iter().map(|row| (row.get::<_, i64>("item_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, i64>("quantity"))).collect::<Vec<_>>();
    assert_eq!(inventory_after, inventory_before);
    assert_eq!(second.outbox.len(), outbox_before);
    db.execute("DELETE FROM game.crate_rewards WHERE item_id=$1", &[&deleted]).await.unwrap();
    assert_eq!(client.outbox.len(), before);
    db.execute("DELETE FROM game.crate_rewards WHERE item_id=$1", &[&item]).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, other).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn location_checks_sources_and_recomputes_after_session_change() {
    use infinity_core::consts::{FLAG_CARGO, FLAG_HANGAR, GROUP_CARGO_CONTAINER};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let mut stations = client.sde.map.stations();
    let station = stations.next().unwrap().station_id;
    let other = stations.find(|s| s.station_id != station).unwrap().station_id;
    drop(stations);
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let crate_type = client.sde.crates.iter().find(|d| d.preview(false, None).is_ok_and(|v| !v.is_empty())).unwrap().type_id;
    let container_type = client.sde.types.iter().find(|t| t.group_id == Some(GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let parent = crate::dao::items::creation::rookie_ship(&db, container_type, actor.character, station, FLAG_HANGAR).await.unwrap();
    let source = transfers::insert(&db, crate_type, actor.character, parent, FLAG_CARGO, 1).await.unwrap();
    let remote = transfers::insert(&db, crate_type, actor.character, other, FLAG_HANGAR, 1).await.unwrap();
    let foreign_source = transfers::insert(&db, crate_type, foreign.character, station, FLAG_HANGAR, 1).await.unwrap();
    let unreachable = transfers::insert(&db, crate_type, actor.character, i64::MAX, FLAG_CARGO, 1).await.unwrap();
    let singleton = transfers::insert(&db, crate_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    let custom_type = client.sde.crates.iter().find(|d| d.preview(true, None).is_ok_and(|v| !v.is_empty()) && d.preview(false, None).is_ok_and(|v| v.is_empty())).expect("custom-only crate").type_id;
    let custom = transfers::insert(&db, custom_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&singleton]).await.unwrap();
    let before = fixture::mission_fingerprint(&db, actor.character).await.unwrap();
    let expected = Value::tuple([Value::Int(station), Value::Int(i64::from(FLAG_HANGAR))]);
    for _ in 0..2 {
        let result = rpc(&mut client, "crateService", "GetCrateLocation", &[Value::Int(source)], false).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&result)).unwrap(), expected);
    }
    for id in [i64::MAX, remote, foreign_source, unreachable, singleton, custom] {
        refused(rpc(&mut client, "crateService", "GetCrateLocation", &[Value::Int(id)], false).await);
    }
    client.session.apply(vec![("charid", Value::None)]);
    refused(rpc(&mut client, "crateService", "GetCrateLocation", &[Value::Int(source)], false).await);
    client.session.apply(vec![("charid", Value::Int(actor.character))]);
    assert_eq!(fixture::mission_fingerprint(&db, actor.character).await.unwrap(), before);
    assert_eq!(db.query_one("SELECT count(*) FROM game.crate_rewards WHERE character_id=$1", &[&actor.character]).await.unwrap().get::<_, i64>("count"), 0);
    assert!(client.outbox.is_empty());
    db.execute("UPDATE game.items SET location_id=$2, flag_id=$3 WHERE item_id=$1", &[&source, &other, &FLAG_HANGAR]).await.unwrap();
    client.session.apply(vec![("stationid", Value::Int(other))]);
    let opened = rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await.unwrap();
    for row in opened.items().unwrap() {
        let item_id = row.items().unwrap()[0].as_int().unwrap();
        let receipt = db.query_one("SELECT destination_location, destination_flag FROM game.crate_rewards WHERE item_id=$1", &[&item_id]).await.unwrap();
        assert_eq!(receipt.get::<_, i64>("destination_location"), other);
        assert_eq!(receipt.get::<_, i32>("destination_flag"), FLAG_HANGAR);
    }
    db.execute("DELETE FROM game.crate_rewards WHERE character_id=$1", &[&actor.character]).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn shared_guest_crate_yields_private_actor_rewards_and_preserves_remaining_stack() {
    use infinity_core::consts::{FLAG_CARGO, FLAG_FLEET_HANGAR};
    use crate::services::drones::tests::fixture::Fixture;
    let mut f = Fixture::with_hold_attributes(&[infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY]).await;
    let (guest, _) = f.add_pilot().await;
    let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(guest, sender);
    let type_id = f.sde.crates.iter().find(|d| d.preview(false, None).is_ok_and(|v| !v.is_empty())).expect("ordinary crate").type_id;
    let source = transfers::insert(&f.db, type_id, guest, f.ship, FLAG_FLEET_HANGAR, 2).await.unwrap();
    let original = crate::services::items::load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    let ship = f.ship;
    assert!(crate::dao::items::dogma::set_override(&f.db, ship, infinity_core::consts::dogma::ATTR_CAPACITY, 1_000_000.0).await.unwrap());
    let actor_before = f.client.outbox.len();
    let result = rpc(&mut f.client, "crateService", "OpenCrate", &[Value::Int(source)], false).await.unwrap();
    assert_eq!(f.client.outbox.len() - actor_before, 1 + result.items().unwrap().len(), "actor must receive one source notice and one per reward");
    for reward in result.items().unwrap() {
        let id = reward.items().unwrap()[0].as_int().unwrap();
        let row = load_items(&f.db, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
        assert_eq!((row.owner_id, row.location_id, row.flag_id), (f.owner, ship, FLAG_CARGO));
    }
    let row = load_items(&f.db, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((row.owner_id, row.location_id, row.flag_id, row.quantity), (guest, ship, FLAG_FLEET_HANGAR, 1));
    let (name, args) = received.try_recv().expect("guest receives source quantity change");
    assert_eq!(name, "OnItemChange");
    let remaining = crate::dao::items::loading::Item { quantity: 1, ..original };
    assert_eq!(args, vec![remaining.row(), Value::Dict(vec![(Value::Int(infinity_core::consts::IX_STACK_SIZE), Value::Int(2))]), Value::None]);
    assert!(received.try_recv().is_err(), "private reward leaked to guest");
    let private = transfers::insert(&f.db, type_id, guest, ship, FLAG_CARGO, 1).await.unwrap();
    let actor_notices = f.client.outbox.len();
    refused(rpc(&mut f.client, "crateService", "OpenCrate", &[Value::Int(private)], false).await);
    assert_eq!(load_items(&f.db, ItemQuery::Id(private)).await.unwrap()[0].quantity, 1);
    assert_eq!(f.client.outbox.len(), actor_notices, "refusal emitted an actor notification");
    assert!(received.try_recv().is_err(), "refusal emitted a guest notification");
    let ship_type = f.world.ball(f.system, ship).await.unwrap().type_id;
    let nested_hull = crate::dao::items::creation::rookie_ship(&f.db, ship_type, guest, ship, FLAG_FLEET_HANGAR).await.unwrap();
    let nested = transfers::insert(&f.db, type_id, f.owner, nested_hull, FLAG_CARGO, 1).await.unwrap();
    refused(rpc(&mut f.client, "crateService", "OpenCrate", &[Value::Int(nested)], false).await);
    let nested_row = load_items(&f.db, ItemQuery::Id(nested)).await.unwrap().pop().unwrap();
    assert_eq!((nested_row.owner_id, nested_row.location_id, nested_row.quantity), (f.owner, nested_hull, 1));
    assert_eq!(f.client.outbox.len(), actor_notices, "nested refusal emitted actor notification");
    assert!(received.try_recv().is_err(), "nested refusal emitted guest notification");
    f.db.execute("DELETE FROM game.crate_rewards WHERE character_id=$1", &[&f.owner]).await.unwrap();
    f.cleanup().await;
}
