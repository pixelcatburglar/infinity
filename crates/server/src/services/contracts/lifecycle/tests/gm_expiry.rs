//! GM expiry through the real contract service and corporate escrow.
use super::super::*;
use super::corporate_fixture as f;

async fn rpc(c: &mut Client, args: &[Value]) -> CallResult {
    let kwargs = Value::None;
    crate::services::dispatch(c, &Call { service: "contractProxy", method: "GM_ExpireContract", args, kwargs: &kwargs, bound: false, bind_params: None }).await
}

async fn wallet(db: &Db, owner: i64, key: i32) -> f64 {
    db.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&owner, &key]).await.unwrap().get("balance")
}

async fn item_state(db: &Db, id: i64) -> (i64, i64, i32) {
    let row = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    (row.get("owner_id"), row.get("location_id"), row.get("flag_id"))
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn authorization_cutoff_and_corporate_reclaim() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (station_id, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let corp = f::setup(&db, station_id, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut client, _peer) = crate::services::tests::client(db.clone(), corp.actor.character).await;
        f::session(&mut client, corp, station_id, system, ACCOUNT_CASH + 1);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(corp.actor.character, tx);
        let root = transfers::insert(&db, kind, corp.id, corp.office, f::SAG, 1).await.unwrap();
        let id = f::id(create(&client, &f::payload(TYPE_ITEM_EXCHANGE, station_id, 0, f::SAG, root, 0.0, 0.0, 0.0)).await);
        while rx.try_recv().is_ok() {}
        for role in [0, 9007199254740992, 262144, 4096, infinity_core::consts::GM_ROLES & !infinity_core::consts::ROLE_GML, infinity_core::consts::ROLE_DIRECTOR] {
            client.login.role = role;
            assert!(rpc(&mut client, &[Value::Int(id)]).await.is_err());
        }
        client.login.role = infinity_core::consts::ROLE_GML;
        for args in [vec![], vec![Value::Bool(true)], vec![Value::str("1")], vec![Value::None], vec![Value::Int(0)], vec![Value::Int(-1)], vec![Value::Float(1.0)], vec![Value::Int(id), Value::Int(id)]] {
            assert!(rpc(&mut client, &args).await.is_err());
        }
        client.session.apply(vec![("charid", Value::Int(0))]);
        assert!(rpc(&mut client, &[Value::Int(id)]).await.is_err());
        client.session.apply(vec![("charid", Value::Int(corp.actor.character))]);
        let before = filetime_now();
        let value = rpc(&mut client, &[Value::Int(id)]).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), Value::None);
        let after = filetime_now();
        let row = db.query_one("SELECT date_expired,status FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        let expiry: i64 = row.get("date_expired");
        assert!(expiry >= before && expiry <= after);
        assert_eq!(row.get::<_, i32>("status"), STATUS_OUTSTANDING);
        assert!(rx.try_recv().is_err());
        assert_eq!(rpc(&mut client, &[Value::Int(id)]).await.unwrap(), Value::None);
        assert_eq!(db.query_one("SELECT date_expired FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap().get::<_, i64>("date_expired"), expiry);
        assert!(contract_dao::expire_outstanding(&db, id, expiry, expiry, STATUS_OUTSTANDING).await.unwrap());
        assert!(!contract_dao::expire_outstanding(&db, id, expiry + 1, expiry, STATUS_OUTSTANDING).await.unwrap());
        assert!(contract(&db, id).await.unwrap().expired(expiry));
        assert!(!contract(&db, id).await.unwrap().expired(expiry - 1));
        assert!(rx.try_recv().is_err());
        { let _guard = lock().await; assert!(delete_one(&client, id).await.is_ok()); }
        let item = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
        assert_eq!((item.get::<_, i64>("owner_id"), item.get::<_, i64>("location_id"), item.get::<_, i32>("flag_id")), (corp.id, corp.office, f::SAG));
        { let _guard = lock().await; assert!(delete_one(&client, id).await.is_err()); }
        assert!(rpc(&mut client, &[Value::Int(id)]).await.is_err());
        for status in [STATUS_REJECTED, STATUS_FAILED] {
            db.execute("UPDATE game.contracts SET status=$2 WHERE contract_id=$1", &[&id, &status]).await.unwrap();
            let before: String = db.query_one("SELECT row_to_json(k)::text AS state FROM game.contracts k WHERE contract_id=$1", &[&id]).await.unwrap().get("state");
            assert!(rpc(&mut client, &[Value::Int(id)]).await.is_err());
            let after: String = db.query_one("SELECT row_to_json(k)::text AS state FROM game.contracts k WHERE contract_id=$1", &[&id]).await.unwrap().get("state");
            assert_eq!(after, before);
        }
        client.world.unregister_pilot_generation(corp.actor.character, None);
    }).await;
    f::cleanup(&db, corp).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn bid_auction_settles_once_and_scheduler_does_not_pay_again() {
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
        f::session(&mut buyer, bidder, station_id, system, ACCOUNT_CASH + 2);
        seller.login.role = infinity_core::consts::ROLE_GML;
        let root = transfers::insert(&db, kind, issuer.id, station_id, f::DELIVERIES, 1).await.unwrap();
        let id = f::id(create(&seller, &f::payload(TYPE_AUCTION, station_id, 0, f::DELIVERIES, root, 10.0, 0.0, 0.0)).await);
        assert!(place_bid(&buyer, id, 10.0, true).await.is_ok());
        let original = db.query_one("SELECT date_expired,paid_deposit::float8 AS deposit,auction_tax_rate::float8 AS rate,auction_tax_maximum::float8 AS maximum FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        let expiry: i64 = original.get("date_expired");
        let deposit: f64 = original.get("deposit");
        let rate: f64 = original.get("rate");
        let maximum: f64 = original.get("maximum");
        let balance_before = db.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&issuer.id, &(ACCOUNT_CASH + 1)]).await.unwrap().get::<_, f64>("balance");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        seller.world.register_pilot(issuer.actor.character, tx);
        let (winner_tx, mut winner_rx) = tokio::sync::mpsc::unbounded_channel();
        seller.world.register_pilot(bidder.actor.character, winner_tx);
        let bidder_balance = db.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&bidder.id, &(ACCOUNT_CASH + 2)]).await.unwrap().get::<_, f64>("balance");
        let bid_before: String = db.query_one("SELECT row_to_json(b)::text AS state FROM game.contract_bids b WHERE contract_id=$1 ORDER BY amount DESC, bid_id LIMIT 1", &[&id]).await.unwrap().get("state");
        let acceptor_before = db.query_one("SELECT acceptor_id,acceptor_wallet_key FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        let acceptor_id: i64 = acceptor_before.get("acceptor_id");
        let acceptor_key: i32 = acceptor_before.get("acceptor_wallet_key");
        assert!(super::super::super::fees::sales_tax_amount(10.0, 1_000_000.0, 1_000_000_000.0) > balance_before + 10.0 + deposit);
        db.execute("UPDATE game.contracts SET auction_tax_rate=1000000,auction_tax_maximum=1000000000 WHERE contract_id=$1", &[&id]).await.unwrap();
        assert!(matches!(rpc(&mut seller, &[Value::Int(id)]).await, Err(CallError::User { msg, .. }) if msg == messages::MSG_NOT_ENOUGH_MONEY));
        let rolled_back = db.query_one("SELECT date_expired,status,acceptor_id,acceptor_wallet_key,paid_deposit::float8 AS deposit FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        assert_eq!((rolled_back.get::<_, i64>("date_expired"), rolled_back.get::<_, i32>("status"), rolled_back.get::<_, f64>("deposit")), (expiry, STATUS_OUTSTANDING, deposit));
        assert_eq!((rolled_back.get::<_, i64>("acceptor_id"), rolled_back.get::<_, i32>("acceptor_wallet_key")), (acceptor_id, acceptor_key));
        let bid_after: String = db.query_one("SELECT row_to_json(b)::text AS state FROM game.contract_bids b WHERE contract_id=$1 ORDER BY amount DESC, bid_id LIMIT 1", &[&id]).await.unwrap().get("state");
        assert_eq!(bid_after, bid_before);
        assert_eq!(db.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&bidder.id, &(ACCOUNT_CASH + 2)]).await.unwrap().get::<_, f64>("balance"), bidder_balance);
        assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap().get::<_, i64>("location_id"), -id);
        assert_eq!(db.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&issuer.id, &(ACCOUNT_CASH + 1)]).await.unwrap().get::<_, f64>("balance"), balance_before);
        assert!(rx.try_recv().is_err());
        assert!(winner_rx.try_recv().is_err());
        db.execute("UPDATE game.contracts SET auction_tax_rate=$2,auction_tax_maximum=$3 WHERE contract_id=$1", &[&id, &rate, &maximum]).await.unwrap();
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(10), rpc(&mut seller, &[Value::Int(id)])).await.unwrap().unwrap(), Value::None);
        let row = db.query_one("SELECT status,acceptor_id,acceptor_wallet_key,paid_deposit::float8 AS paid_deposit FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        assert_eq!(row.get::<_, i32>("status"), STATUS_FINISHED);
        assert_eq!(row.get::<_, i64>("acceptor_id"), bidder.id);
        assert_eq!(row.get::<_, i32>("acceptor_wallet_key"), ACCOUNT_CASH + 2);
        assert_eq!(row.get::<_, f64>("paid_deposit"), 0.0);
        let item = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
        assert_eq!((item.get::<_, i64>("owner_id"), item.get::<_, i64>("location_id"), item.get::<_, i32>("flag_id")), (bidder.id, station_id, f::DELIVERIES));
        let balance = db.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&issuer.id, &(ACCOUNT_CASH + 1)]).await.unwrap().get::<_, f64>("balance");
        assert_eq!(balance, balance_before + 10.0 + deposit - super::super::super::fees::sales_tax_amount(10.0, rate, maximum));
        assert_eq!(std::iter::from_fn(|| rx.try_recv().ok()).filter(|(event, _)| event == "OnContractCompleted").count(), 1);
        assert_eq!(std::iter::from_fn(|| winner_rx.try_recv().ok()).filter(|(event, _)| event == "OnContractCompleted").count(), 1);
        assert!(rpc(&mut seller, &[Value::Int(id)]).await.is_err());
        assert!(tokio::time::timeout(std::time::Duration::from_secs(10), settle_expired(&db, &seller.sde, &seller.world)).await.unwrap().is_ok());
        assert_eq!(db.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&issuer.id, &(ACCOUNT_CASH + 1)]).await.unwrap().get::<_, f64>("balance"), balance);
        assert_eq!(std::iter::from_fn(|| rx.try_recv().ok()).filter(|(event, _)| event == "OnContractCompleted").count(), 0);
        assert_eq!(std::iter::from_fn(|| winner_rx.try_recv().ok()).filter(|(event, _)| event == "OnContractCompleted").count(), 0);
        seller.world.unregister_pilot_generation(issuer.actor.character, None);
        seller.world.unregister_pilot_generation(bidder.actor.character, None);
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, bidder).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn courier_and_unbid_auction_cutoff_and_concurrent_actions() {
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
        seller.login.role = infinity_core::consts::ROLE_GML;
        let (issuer_tx, mut issuer_rx) = tokio::sync::mpsc::unbounded_channel();
        let (bidder_tx, mut bidder_rx) = tokio::sync::mpsc::unbounded_channel();
        seller.world.register_pilot(issuer.actor.character, issuer_tx);
        seller.world.register_pilot(bidder.actor.character, bidder_tx);
        for contract_type in [TYPE_AUCTION, TYPE_COURIER] {
            let root = transfers::insert(&db, kind, issuer.id, issuer.office, f::SAG, 1).await.unwrap();
            let reward = if contract_type == TYPE_COURIER { 50.0 } else { 0.0 };
            let id = f::id(create(&seller, &f::payload(contract_type, station_id, station_id, f::SAG, root, 10.0, reward, 0.0)).await);
            let before = wallet(&db, issuer.id, ACCOUNT_CASH + 1).await;
            while issuer_rx.try_recv().is_ok() {}
            assert_eq!(rpc(&mut seller, &[Value::Int(id)]).await.unwrap(), Value::None);
            assert_eq!(wallet(&db, issuer.id, ACCOUNT_CASH + 1).await, before);
            assert!(issuer_rx.try_recv().is_err());
            assert!(if contract_type == TYPE_AUCTION { place_bid(&buyer, id, 10.0, true).await.is_err() } else { accept(&buyer, id, true).await.is_err() });
            assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap().get::<_, i64>("location_id"), -id);
            { let _guard = lock().await; assert!(delete_one(&seller, id).await.is_ok()); }
            assert_eq!(item_state(&db, root).await, (issuer.id, issuer.office, f::SAG));
            let after = wallet(&db, issuer.id, ACCOUNT_CASH + 1).await;
            assert_eq!(after, before + reward);
            { let _guard = lock().await; assert!(delete_one(&seller, id).await.is_err()); }
            assert_eq!(item_state(&db, root).await, (issuer.id, issuer.office, f::SAG));
            assert_eq!(wallet(&db, issuer.id, ACCOUNT_CASH + 1).await, after);
        }
        let root = transfers::insert(&db, kind, issuer.id, issuer.office, f::SAG, 1).await.unwrap();
        let id = f::id(create(&seller, &f::payload(TYPE_COURIER, station_id, station_id, f::SAG, root, 0.0, 0.0, 0.0)).await);
        assert!(accept(&buyer, id, true).await.is_ok());
        let expiry = db.query_one("SELECT date_expired FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap().get::<_, i64>("date_expired");
        assert!(rpc(&mut seller, &[Value::Int(id)]).await.is_err());
        let row = db.query_one("SELECT status,date_expired FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        assert_eq!((row.get::<_, i32>("status"), row.get::<_, i64>("date_expired")), (STATUS_IN_PROGRESS, expiry));
        for contract_type in [TYPE_ITEM_EXCHANGE, TYPE_AUCTION] {
            let root = transfers::insert(&db, kind, issuer.id, issuer.office, f::SAG, 1).await.unwrap();
            let id = f::id(create(&seller, &f::payload(contract_type, station_id, 0, f::SAG, root, 10.0, 0.0, 0.0)).await);
            let original = db.query_one("SELECT date_expired,paid_deposit::float8 AS deposit,auction_tax_rate::float8 AS rate,auction_tax_maximum::float8 AS maximum FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
            let deadline: i64 = original.get("date_expired");
            let deposit: f64 = original.get("deposit");
            let rate: f64 = original.get("rate");
            let maximum: f64 = original.get("maximum");
            let seller_before = wallet(&db, issuer.id, ACCOUNT_CASH + 1).await;
            let buyer_before = wallet(&db, bidder.id, ACCOUNT_CASH + 1).await;
            let escrow = item_state(&db, root).await;
            let args = [Value::Int(id)];
            let pair = async {
                tokio::join!(rpc(&mut seller, &args), async {
                    if contract_type == TYPE_AUCTION { place_bid(&buyer, id, 10.0, true).await }
                    else { accept(&buyer, id, true).await }
                })
            };
            let (expired, action) = tokio::time::timeout(std::time::Duration::from_secs(10), pair).await.unwrap();
            let row = db.query_one("SELECT status,date_expired FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
            if contract_type == TYPE_ITEM_EXCHANGE {
                assert_ne!(expired.is_ok(), action.is_ok());
            } else {
                assert!(expired.is_ok());
            }
            if action.is_ok() {
                assert_eq!(row.get::<_, i32>("status"), STATUS_FINISHED);
                if contract_type == TYPE_ITEM_EXCHANGE {
                    assert_eq!(row.get::<_, i64>("date_expired"), deadline);
                }
                assert_eq!(item_state(&db, root).await, (bidder.id, station_id, f::DELIVERIES));
                let tax = if contract_type == TYPE_AUCTION { super::super::super::fees::sales_tax_amount(10.0, rate, maximum) } else { 0.0 };
                assert_eq!(wallet(&db, issuer.id, ACCOUNT_CASH + 1).await, seller_before + 10.0 + deposit - tax);
                assert_eq!(wallet(&db, bidder.id, ACCOUNT_CASH + 1).await, buyer_before - 10.0);
            } else {
                assert!(expired.is_ok());
                assert_eq!(row.get::<_, i32>("status"), STATUS_OUTSTANDING);
                assert!(row.get::<_, i64>("date_expired") <= filetime_now());
                assert_eq!(item_state(&db, root).await, escrow);
                assert_eq!(wallet(&db, issuer.id, ACCOUNT_CASH + 1).await, seller_before);
                assert_eq!(wallet(&db, bidder.id, ACCOUNT_CASH + 1).await, buyer_before);
            }
        }
        let root = transfers::insert(&db, kind, issuer.id, issuer.office, f::SAG, 1).await.unwrap();
        let id = f::id(create(&seller, &f::payload(TYPE_ITEM_EXCHANGE, station_id, 0, f::SAG, root, 0.0, 0.0, 0.0)).await);
        db.execute("DELETE FROM game.offices WHERE office_id=$1", &[&issuer.office]).await.unwrap();
        let (mut deleter, _peer3) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        f::session(&mut deleter, issuer, station_id, system, ACCOUNT_CASH + 1);
        let args = [Value::Int(id)];
        let (expired, deleted) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(rpc(&mut seller, &args), async {
                let _guard = lock().await;
                delete_one(&deleter, id).await
            })
        }).await.unwrap();
        assert!(deleted.is_ok());
        if let Err(error) = expired {
            assert!(matches!(error, CallError::User { msg, .. } if msg == messages::MSG_CON_CONTRACT_NOT_OUTSTANDING));
        }
        let item = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
        assert_eq!((item.get::<_, i64>("owner_id"), item.get::<_, i64>("location_id"), item.get::<_, i32>("flag_id")), (issuer.id, station_id, f::DELIVERIES));
        let root = transfers::insert(&db, kind, issuer.id, station_id, f::DELIVERIES, 1).await.unwrap();
        let id = f::id(create(&seller, &f::payload(TYPE_AUCTION, station_id, 0, f::DELIVERIES, root, 10.0, 0.0, 0.0)).await);
        assert!(place_bid(&buyer, id, 10.0, true).await.is_ok());
        let original = db.query_one("SELECT paid_deposit::float8 AS deposit,auction_tax_rate::float8 AS rate,auction_tax_maximum::float8 AS maximum FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        let deposit: f64 = original.get("deposit");
        let rate: f64 = original.get("rate");
        let maximum: f64 = original.get("maximum");
        let seller_before = wallet(&db, issuer.id, ACCOUNT_CASH + 1).await;
        let buyer_before = wallet(&db, bidder.id, ACCOUNT_CASH + 1).await;
        db.execute("UPDATE game.contracts SET date_expired=$2 WHERE contract_id=$1", &[&id, &(filetime_now() - 1)]).await.unwrap();
        while issuer_rx.try_recv().is_ok() {}
        while bidder_rx.try_recv().is_ok() {}
        let sde = seller.sde.clone();
        let world = seller.world.clone();
        let args = [Value::Int(id)];
        let (expired, scheduled) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(rpc(&mut seller, &args), settle_expired(&db, &sde, &world))
        }).await.unwrap();
        if let Err(error) = expired {
            assert!(matches!(error, CallError::User { msg, .. } if msg == messages::MSG_CON_CONTRACT_NOT_OUTSTANDING));
        }
        assert!(scheduled.is_ok());
        assert_eq!(db.query_one("SELECT status FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap().get::<_, i32>("status"), STATUS_FINISHED);
        assert_eq!(item_state(&db, root).await, (bidder.id, station_id, f::DELIVERIES));
        assert_eq!(wallet(&db, issuer.id, ACCOUNT_CASH + 1).await, seller_before + 10.0 + deposit - super::super::super::fees::sales_tax_amount(10.0, rate, maximum));
        assert_eq!(wallet(&db, bidder.id, ACCOUNT_CASH + 1).await, buyer_before);
        assert_eq!(std::iter::from_fn(|| issuer_rx.try_recv().ok()).filter(|(event, _)| event == "OnContractCompleted").count(), 1);
        assert_eq!(std::iter::from_fn(|| bidder_rx.try_recv().ok()).filter(|(event, _)| event == "OnContractCompleted").count(), 1);
        seller.world.unregister_pilot_generation(issuer.actor.character, None);
        seller.world.unregister_pilot_generation(bidder.actor.character, None);
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, bidder).await;
    result.unwrap();
}
