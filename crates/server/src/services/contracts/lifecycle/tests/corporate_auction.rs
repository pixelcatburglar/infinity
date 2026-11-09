//! Corporate auction wallet and item custody regressions.
use super::super::*;
use super::corporate_fixture as f;

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn corporate_auction_refunds_and_settles_captured_divisions() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (station_id, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let issuer = f::setup(&db, station_id, station_type).await;
    let bidder = f::setup(&db, station_id, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _peer1) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        let (mut buyer, _peer2) = crate::services::tests::client(db.clone(), bidder.actor.character).await;
        f::session(&mut seller, issuer, station_id, system, ACCOUNT_CASH + 1);
        f::session(&mut buyer, bidder, station_id, system, ACCOUNT_CASH + 1);
        let root = transfers::insert(&db, kind, issuer.id, station_id, f::DELIVERIES, 1).await.unwrap();
        let id = f::id(create(&seller, &f::payload(TYPE_AUCTION, station_id, 0, f::DELIVERIES, root, 10.0, 0.0, 30.0)).await);
        assert!(place_bid(&buyer, id, 10.0, true).await.is_ok());
        f::session(&mut buyer, bidder, station_id, system, ACCOUNT_CASH + 2);
        f::session(&mut seller, issuer, station_id, system, ACCOUNT_CASH + 2);
        assert!(place_bid(&buyer, id, 30.0, true).await.is_ok());
        let rows = db.query("SELECT corporation_id,account_key,balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=ANY($1) ORDER BY corporation_id,account_key", &[&vec![issuer.id, bidder.id]]).await.unwrap();
        let balance = |corp, key| rows.iter().find(|r| r.get::<_, i64>("corporation_id") == corp && r.get::<_, i32>("account_key") == key).unwrap().get::<_, f64>("balance");
        assert_eq!(balance(bidder.id, ACCOUNT_CASH + 1), 1000000.0);
        assert_eq!(balance(bidder.id, ACCOUNT_CASH + 2), 999970.0);
        assert_eq!(balance(issuer.id, ACCOUNT_CASH + 1), 980030.0);
        assert_eq!(balance(issuer.id, ACCOUNT_CASH + 2), 1000000.0);
        let contract = db.query_one("SELECT acceptor_id,acceptor_wallet_key,status FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        assert_eq!(contract.get::<_, i64>("acceptor_id"), bidder.id);
        assert_eq!(contract.get::<_, i32>("acceptor_wallet_key"), ACCOUNT_CASH + 2);
        assert_eq!(contract.get::<_, i32>("status"), STATUS_FINISHED);
        let item = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
        assert_eq!((item.get::<_, i64>("owner_id"), item.get::<_, i64>("location_id"), item.get::<_, i32>("flag_id")), (bidder.id, station_id, f::DELIVERIES));
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, bidder).await;
    result.unwrap();
}
