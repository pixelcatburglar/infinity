use super::*;
use infinity_core::consts::{FLAG_CORP_DELIVERIES, ROLE_ACCOUNTANT};

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn delivery_assets_return_station_rows_only_to_authorized_members() {
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let stations: Vec<_> = client.sde.map.stations().take(2).map(|s| (s.station_id, s.solar_system_id)).collect();
    assert_eq!(stations.len(), 2);
    let ty = client.sde.types.iter().next().unwrap().type_id;
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name, ticker, ceo_id, creator_id, station_id, created) VALUES ('asset-' || nextval('game.corporations_corporation_id_seq')::text, 'A' || nextval('game.corporations_corporation_id_seq')::text, $1, $1, $2, 0) RETURNING corporation_id", &[&actor.character, &stations[0].0]).await.unwrap().get("corporation_id");
    db.execute("UPDATE game.characters SET corporation_id = $1, corp_roles = $2 WHERE character_id = $3", &[&corp, &ROLE_ACCOUNTANT, &actor.character]).await.unwrap();
    for (station, _) in &stations {
        crate::dao::items::transfers::insert(&db, ty, corp, *station, FLAG_CORP_DELIVERIES, 1).await.unwrap();
    }
    let kwargs = Value::Dict(vec![]);
    let args = [Value::Int(corp), Value::str("deliveries")];
    let call = Call { service: "corpmgr", method: "GetAssetInventory", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let rows = dispatch(&mut client, &call).await.unwrap().ok().unwrap();
    assert_eq!(rows.items().unwrap().len(), 2);
    for (station, system) in &stations {
        assert!(rows.items().unwrap().iter().any(|row| match row {
            Value::Instance(_, fields) => fields.get("locationID") == Some(&Value::Int(*station)) && fields.get("solarsystemID") == Some(&system.map(Value::Int).unwrap_or(Value::None)) && fields.get("itemCount") == Some(&Value::Int(1)),
            _ => false,
        }));
    }
    let foreign = [Value::Int(corp + 1), Value::str("deliveries")];
    let call = Call { args: &foreign, ..call };
    assert!(dispatch(&mut client, &call).await.unwrap().ok().unwrap().items().unwrap().is_empty());
    db.execute("DELETE FROM game.items WHERE owner_id = $1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = $1", &[&actor.character]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}
