//! Inventory reads must not expose reserved custody or alter fixture state.
use infinity_core::{consts::{FLAG_CARGO, FLAG_HANGAR, GROUP_CARGO_CONTAINER, GROUP_STATION}};
use crate::{dao::items::{loading::ItemQuery, transfers}, services::{self, items::load_items, Call, CallError}};
use marshal::Value;

pub(super) async fn fingerprint(db: &crate::db::Db, owner: i64) -> String {
    db.query_one("SELECT json_build_object('items',(SELECT json_agg(row_to_json(i) ORDER BY item_id) FROM game.items i WHERE owner_id=$1), 'charges',(SELECT json_agg(row_to_json(c) ORDER BY ship_id,flag_id) FROM game.module_charges c WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)), 'wallets',(SELECT json_agg(row_to_json(w) ORDER BY account_id) FROM game.wallet_accounts w WHERE owner_id=$1), 'character',(SELECT row_to_json(c) FROM game.characters c WHERE character_id=$1), 'jobs',(SELECT json_agg(row_to_json(j) ORDER BY job_id) FROM game.jobs j WHERE job_id LIKE '%' || $1::text || '%' OR payload LIKE '%' || $1::text || '%'))::text AS state", &[&owner]).await.unwrap().get("state")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn real_contract_escrow_and_open_trade_deny_roots_and_children() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = services::tests::fixture::create(&db).await.unwrap();
    let other = services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = services::tests::client(db.clone(), owner.character).await;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some() && s.type_id.is_some()).unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    db.execute("UPDATE game.characters SET balance=1000000,station_id=$2 WHERE character_id=$1", &[&owner.character, &station]).await.unwrap();
    let ty = client.sde.types.iter().find(|t| t.group_id == Some(GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let root = transfers::insert(&db, ty, owner.character, station, FLAG_HANGAR, 1).await.unwrap();
    let child = transfers::insert(&db, ty, owner.character, root, FLAG_CARGO, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=ANY($1)", &[&vec![root, child]]).await.unwrap();
    let payload = Value::dict([
        ("contractType", Value::Int(1)), ("startStationID", Value::Int(station)),
        ("startStationDivision", Value::Int(i64::from(FLAG_HANGAR))), ("minutesExpire", Value::Int(1440)),
        ("forCorp", Value::Bool(false)), ("itemList", Value::list([Value::tuple([Value::Int(root), Value::Int(1)])])),
        ("requestItemTypeList", Value::list([])),
    ]);
    let kwargs = Value::None;
    let args = [payload, Value::Bool(true)];
    let create = Call { service: "contractProxy", method: "CreateContract", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let result = services::dispatch(&mut client, &create).await.unwrap();
    let contract = result.items().unwrap()[0].as_int().unwrap();
    let row = db.query_one("SELECT owner_id,location_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
    assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id")), (owner.character, -contract));
    let trade = crate::dao::trades::create(&db, station, owner.character, other.character, 0).await.unwrap();
    let staged = transfers::insert(&db, ty, owner.character, trade.id, FLAG_HANGAR, 1).await.unwrap();
    let staged_child = transfers::insert(&db, ty, owner.character, staged, FLAG_CARGO, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=ANY($1)", &[&vec![staged, staged_child]]).await.unwrap();
    let bind = Value::tuple([Value::Int(station), Value::Int(GROUP_STATION)]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = tx;
    client.outbox.clear();
    let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
    let before = fingerprint(&db, owner.character).await;
    for id in [root, child, staged, staged_child] {
        let snapshot = load_items(&db, &client.sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap().row();
        let args = [Value::Int(id), Value::Int(station)];
        let call = Call { service: "invbroker", method: "GetContainerContents", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        assert!(matches!(services::dispatch(&mut client, &call).await, Err(CallError::User { .. })));
        let args = [Value::list([snapshot])];
        let call = Call { service: "invbroker", method: "ValidateItemListCanBeOpened", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        assert_eq!(services::dispatch(&mut client, &call).await.unwrap(), Value::list([Value::Int(id)]));
    }
    assert_eq!(fingerprint(&db, owner.character).await, before);
    assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
    assert!(client.outbox.is_empty());
    assert!(rx.try_recv().is_err());
    db.execute("DELETE FROM game.trades WHERE trade_id=$1", &[&trade.id]).await.unwrap();
    db.execute("DELETE FROM game.contract_items WHERE contract_id=$1", &[&contract]).await.unwrap();
    db.execute("DELETE FROM game.contracts WHERE contract_id=$1", &[&contract]).await.unwrap();
    services::tests::fixture::cleanup(&db, owner).await.unwrap();
    services::tests::fixture::cleanup(&db, other).await.unwrap();
}
