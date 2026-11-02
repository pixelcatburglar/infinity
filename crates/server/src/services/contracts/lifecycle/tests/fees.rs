//! Database regressions for contract fee charging, refunds and atomicity.
use super::super::*;
use super::corporate_fixture as f;

fn request(kind: i32, station: i64, end: i64, roots: &[i64], corporate: bool, assignee: i64, price: f64, reward: f64, collateral: f64, multi: bool) -> Value {
    Value::dict([
        ("contractType", Value::Int(i64::from(kind))), ("startStationID", Value::Int(station)),
        ("destinationID", Value::Int(end)), ("startStationDivision", Value::Int(i64::from(if corporate { f::DELIVERIES } else { FLAG_HANGAR }))),
        ("minutesExpire", Value::Int(1440)), ("numDays", Value::Int(1)),
        ("forCorp", Value::Bool(corporate)), ("assignedToID", Value::Int(assignee)),
        ("price", Value::Float(price)), ("reward", Value::Float(reward)), ("collateral", Value::Float(collateral)),
        ("multiContract", Value::Bool(multi)),
        ("itemList", Value::List(roots.iter().map(|id| Value::tuple([Value::Int(*id), Value::Int(1)])).collect())),
    ])
}

async fn balance(db: &Db, owner: i64, corporate: bool, key: i32) -> f64 {
    let row = if corporate {
        db.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&owner, &key]).await.unwrap()
    } else {
        db.query_one("SELECT balance::float8 AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap()
    };
    row.get("balance")
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn exchange_private_courier_and_deletion_refunds() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let mut stations = sde.map.stations().filter(|s| s.type_id.is_some() && s.solar_system_id.is_some());
    let station = stations.next().unwrap();
    let end = stations.find(|s| s.station_id != station.station_id).unwrap().station_id;
    let (start, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(crate::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let issuer = f::setup(&db, start, station_type).await;
    let buyer = f::setup(&db, start, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _p1) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        let (mut purchaser, _p2) = crate::services::tests::client(db.clone(), buyer.actor.character).await;
        f::session(&mut seller, issuer, start, system, ACCOUNT_CASH + 1);
        f::session(&mut purchaser, buyer, start, system, ACCOUNT_CASH + 1);
        for corporate in [false, true] {
            let owner = if corporate { issuer.id } else { issuer.actor.character };
            let flag = if corporate { f::DELIVERIES } else { FLAG_HANGAR };
            let key = ACCOUNT_CASH + 1;
            let initial = balance(&db, owner, corporate, key).await;
            let item = transfers::insert(&db, kind, owner, start, flag, 1).await.unwrap();
            let id = f::id(create(&seller, &request(TYPE_ITEM_EXCHANGE, start, 0, &[item], corporate, 0, 1.0, 0.0, 0.0, false)).await);
            assert_eq!(balance(&db, owner, corporate, key).await, initial - 120_000.0);
            f::session(&mut seller, issuer, start, system, ACCOUNT_CASH + 2);
            assert!(accept(&purchaser, id, false).await.is_ok());
            assert_eq!(balance(&db, owner, corporate, key).await, initial - 19_999.0);
            assert!(accept(&purchaser, id, false).await.is_err());
            assert_eq!(balance(&db, owner, corporate, key).await, initial - 19_999.0);
            f::session(&mut seller, issuer, start, system, key);
            let item = transfers::insert(&db, kind, owner, start, flag, 1).await.unwrap();
            let before = balance(&db, owner, corporate, key).await;
            let id = f::id(create(&seller, &request(TYPE_ITEM_EXCHANGE, start, 0, &[item], corporate, buyer.actor.character, 1.0, 0.0, 0.0, false)).await);
            assert_eq!(balance(&db, owner, corporate, key).await, before - 10_000.0);
            assert!(accept(&purchaser, id, false).await.is_ok());
            assert_eq!(balance(&db, owner, corporate, key).await, before - 9_999.0);
            let item = transfers::insert(&db, kind, owner, start, flag, 1).await.unwrap();
            let before = balance(&db, owner, corporate, key).await;
            let id = f::id(create(&seller, &request(TYPE_ITEM_EXCHANGE, start, 0, &[item], corporate, 0, 0.0, 0.0, 0.0, false)).await);
            { let _guard = lock().await; assert!(delete_one(&seller, id).await.is_ok()); }
            assert_eq!(balance(&db, owner, corporate, key).await, before - 120_000.0);
            for status in [STATUS_FINISHED, STATUS_FAILED] {
                let item = transfers::insert(&db, kind, owner, start, flag, 1).await.unwrap();
                let before = balance(&db, owner, corporate, key).await;
                let id = f::id(create(&seller, &request(TYPE_COURIER, start, end, &[item], corporate, 0, 0.0, 20.0, 50.0, false)).await);
                assert_eq!(balance(&db, owner, corporate, key).await, before - 20_020.0);
                assert!(accept(&purchaser, id, false).await.is_ok());
                assert_eq!(balance(&db, owner, corporate, key).await, before - 10_020.0);
                if status == STATUS_FINISHED {
                    let package: i64 = db.query_one("SELECT package_id FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap().get("package_id");
                    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&package, &end]).await.unwrap();
                }
                assert!(complete(&purchaser, id, status).await.is_ok());
                assert_eq!(balance(&db, owner, corporate, key).await, before - 10_020.0 + if status == STATUS_FAILED { 70.0 } else { 0.0 });
            }
        }
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn auction_snapshots_tax_and_cap_and_private_auction_is_exempt() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (start, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(crate::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let issuer = f::setup(&db, start, station_type).await;
    let buyer = f::setup(&db, start, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _p1) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        let (mut bidder, _p2) = crate::services::tests::client(db.clone(), buyer.actor.character).await;
        f::session(&mut seller, issuer, start, system, ACCOUNT_CASH + 1);
        f::session(&mut bidder, buyer, start, system, ACCOUNT_CASH + 1);
        db.execute("UPDATE game.corp_wallets SET balance=300000000000 WHERE corporation_id=ANY($1)", &[&vec![issuer.id, buyer.id]]).await.unwrap();
        for skill in [crate::consts::SKILL_ACCOUNTING, crate::consts::SKILL_BROKER_RELATIONS] {
            crate::dao::skills::training::set_level(&db, issuer.actor.character, skill, 5, 0).await.unwrap();
        }
        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let id = f::id(create(&seller, &request(TYPE_AUCTION, start, 0, &[root], true, 0, 100_000_000_000.0, 0.0, 200_000_000_000.0, false)).await);
        assert_eq!(balance(&db, issuer.id, true, ACCOUNT_CASH + 1).await, 299_825_000_000.0);
        crate::dao::skills::training::set_level(&db, issuer.actor.character, crate::consts::SKILL_ACCOUNTING, 0, 0).await.unwrap();
        f::session(&mut seller, issuer, start, system, ACCOUNT_CASH + 2);
        assert!(place_bid(&bidder, id, 200_000_000_000.0, true).await.is_ok());
        assert_eq!(balance(&db, issuer.id, true, ACCOUNT_CASH + 1).await, 499_425_000_000.0);
        assert!(place_bid(&bidder, id, 200_000_000_000.0, true).await.is_err());
        assert_eq!(balance(&db, issuer.id, true, ACCOUNT_CASH + 1).await, 499_425_000_000.0);
        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let before = balance(&db, issuer.id, true, ACCOUNT_CASH + 2).await;
        let id = f::id(create(&seller, &request(TYPE_AUCTION, start, 0, &[root], true, buyer.actor.character, 10.0, 0.0, 30.0, false)).await);
        assert_eq!(balance(&db, issuer.id, true, ACCOUNT_CASH + 2).await, before - 10_000.0);
        assert!(place_bid(&bidder, id, 30.0, false).await.is_ok());
        assert_eq!(balance(&db, issuer.id, true, ACCOUNT_CASH + 2).await, before - 9_970.0);
        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let before = balance(&db, issuer.id, true, ACCOUNT_CASH + 2).await;
        let id = f::id(create(&seller, &request(TYPE_AUCTION, start, 0, &[root], true, 0, 1_000_000.0, 0.0, 0.0, false)).await);
        assert!(place_bid(&bidder, id, 2_000_000.0, true).await.is_ok());
        db.execute("UPDATE game.contracts SET date_expired=0 WHERE contract_id=$1", &[&id]).await.unwrap();
        { let _guard = lock().await;
            assert!(settle_expired_candidate(&db, &sde, &seller.world, id, filetime_now()).await.unwrap().is_some());
            assert!(settle_expired_candidate(&db, &sde, &seller.world, id, filetime_now()).await.unwrap().is_none());
        }
        assert_eq!(balance(&db, issuer.id, true, ACCOUNT_CASH + 2).await, before - 10_000.0 + 2_000_000.0 - 20_000.0);
        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let id = f::id(create(&seller, &request(TYPE_AUCTION, start, 0, &[root], true, 0, 1_000_000.0, 0.0, 0.0, false)).await);
        db.execute("UPDATE game.contracts SET paid_deposit=0, auction_tax_rate=0, auction_tax_maximum=0 WHERE contract_id=$1", &[&id]).await.unwrap();
        db.execute("UPDATE game.corp_wallets SET balance=1000000 WHERE corporation_id=$1 AND account_key=$2", &[&issuer.id, &(ACCOUNT_CASH + 2)]).await.unwrap();
        let baseline = balance(&db, issuer.id, true, ACCOUNT_CASH + 2).await;
        assert_eq!(baseline, 1_000_000.0);
        assert!(place_bid(&bidder, id, 2_000_000.0, true).await.is_ok());
        db.execute("UPDATE game.contracts SET date_expired=0 WHERE contract_id=$1", &[&id]).await.unwrap();
        { let _guard = lock().await;
            assert!(settle_expired_candidate(&db, &sde, &seller.world, id, filetime_now()).await.unwrap().is_some());
            assert_eq!(balance(&db, issuer.id, true, ACCOUNT_CASH + 2).await, baseline + 2_000_000.0);
            assert!(settle_expired_candidate(&db, &sde, &seller.world, id, filetime_now()).await.unwrap().is_none());
        }
        assert_eq!(balance(&db, issuer.id, true, ACCOUNT_CASH + 2).await, baseline + 2_000_000.0);
        let snapshots = db.query_one("SELECT paid_deposit::float8 AS paid_deposit, auction_tax_rate::float8 AS auction_tax_rate, auction_tax_maximum::float8 AS auction_tax_maximum FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        for column in ["paid_deposit", "auction_tax_rate", "auction_tax_maximum"] {
            assert_eq!(snapshots.get::<_, f64>(column), 0.0);
        }
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn multicontract_charges_each_and_insufficient_funds_rollback_everything() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (start, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(crate::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let issuer = f::setup(&db, start, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _peer) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        f::session(&mut seller, issuer, start, system, ACCOUNT_CASH + 1);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        seller.world.register_pilot(issuer.actor.character, tx);
        let roots = [transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap(), transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap()];
        db.execute("UPDATE game.corp_wallets SET balance=120000 WHERE corporation_id=$1 AND account_key=$2", &[&issuer.id, &(ACCOUNT_CASH + 1)]).await.unwrap();
        let before = f::fingerprint(&db, issuer).await;
        let input = request(TYPE_ITEM_EXCHANGE, start, 0, &roots, true, 0, 0.0, 0.0, 0.0, true);
        match create(&seller, &input).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, messages::MSG_NOT_ENOUGH_MONEY);
                assert_eq!(dict, vec![("balance".into(), Value::Float(0.0)), ("amount".into(), Value::Float(120_000.0))]);
            }
            _ => panic!("expected insufficient funds"),
        }
        assert_eq!(f::fingerprint(&db, issuer).await, before);
        assert!(rx.try_recv().is_err());
        for amount in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for (price, reward, collateral) in [(amount, 0.0, 0.0), (0.0, amount, 0.0), (0.0, 0.0, amount)] {
                let invalid = request(TYPE_ITEM_EXCHANGE, start, 0, &roots, true, 0, price, reward, collateral, true);
                assert!(matches!(create(&seller, &invalid).await, Err(CallError::User { .. })));
                assert_eq!(f::fingerprint(&db, issuer).await, before);
                assert!(rx.try_recv().is_err());
            }
        }
        let personal_root = transfers::insert(&db, kind, issuer.actor.character, start, FLAG_HANGAR, 1).await.unwrap();
        db.execute("UPDATE game.characters SET balance=9999 WHERE character_id=$1", &[&issuer.actor.character]).await.unwrap();
        let personal_before = f::fingerprint(&db, issuer).await;
        let personal = request(TYPE_ITEM_EXCHANGE, start, 0, &[personal_root], false, issuer.ceo.character, 0.0, 0.0, 0.0, false);
        match create(&seller, &personal).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, messages::MSG_NOT_ENOUGH_MONEY);
                assert_eq!(dict, vec![("balance".into(), Value::Float(9_999.0)), ("amount".into(), Value::Float(10_000.0))]);
            }
            _ => panic!("expected personal insufficient funds"),
        }
        assert_eq!(f::fingerprint(&db, issuer).await, personal_before);
        assert!(rx.try_recv().is_err());
        db.execute("UPDATE game.corp_wallets SET balance=240000 WHERE corporation_id=$1 AND account_key=$2", &[&issuer.id, &(ACCOUNT_CASH + 1)]).await.unwrap();
        let result = create(&seller, &input).await.unwrap();
        assert_eq!(result.items().unwrap().len(), 2);
        assert_eq!(balance(&db, issuer.id, true, ACCOUNT_CASH + 1).await, 0.0);
        seller.world.unregister_pilot(issuer.actor.character);
    }).await;
    f::cleanup(&db, issuer).await;
    result.unwrap();
}
