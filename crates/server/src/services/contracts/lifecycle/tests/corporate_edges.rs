//! Database regressions for contract limits, nested escrow and courier custody.
use super::super::*;
use super::corporate_fixture as f;

fn request(station: i64, flag: i32, root: i64, corporate: bool, assignee: i64) -> Value {
    Value::dict([
        ("contractType", Value::Int(i64::from(TYPE_ITEM_EXCHANGE))),
        ("startStationID", Value::Int(station)),
        ("startStationDivision", Value::Int(i64::from(flag))),
        ("minutesExpire", Value::Int(1440)),
        ("assignedToID", Value::Int(assignee)),
        ("forCorp", Value::Bool(corporate)),
        ("itemList", Value::list([Value::tuple([Value::Int(root), Value::Int(1)])])),
        ("requestItemTypeList", Value::list([])),
    ])
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn creation_count_boundaries_use_live_personal_and_corporate_limits() {
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
        f::session(&mut client, corp, station_id, system, ACCOUNT_CASH);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(corp.actor.character, tx);
        for corporate in [false, true] {
            for internal in [false, true] {
                let cap = if internal { 500_i32 } else if corporate { 10 } else { 1 };
                let assignee = if internal { corp.ceo.character } else { 0 };
                db.execute("INSERT INTO game.contracts (type,issuer_id,issuer_corp_id,assignee_id,start_station_id,date_issued,date_expired,status,for_corp,issuer_source_location) SELECT $1,$2,$3,$4,$5,0,0,$6,$7,$5 FROM generate_series(1,$8)", &[&TYPE_ITEM_EXCHANGE, &corp.actor.character, &corp.id, &assignee, &station_id, &STATUS_OUTSTANDING, &corporate, &(cap - 1)]).await.unwrap();
                let owner = if corporate { corp.id } else { corp.actor.character };
                let flag = if corporate { f::DELIVERIES } else { FLAG_HANGAR };
                let first = transfers::insert(&db, kind, owner, station_id, flag, 1).await.unwrap();
                let second = transfers::insert(&db, kind, owner, station_id, flag, 1).await.unwrap();
                assert!(create(&client, &request(station_id, flag, first, corporate, assignee)).await.is_ok());
                while rx.try_recv().is_ok() {}
                let before = f::fingerprint(&db, corp).await;
                assert!(matches!(create(&client, &request(station_id, flag, second, corporate, assignee)).await, Err(CallError::User { .. })));
                assert_eq!(f::fingerprint(&db, corp).await, before);
                assert!(rx.try_recv().is_err());
                db.execute("DELETE FROM game.contract_items WHERE contract_id IN (SELECT contract_id FROM game.contracts WHERE issuer_corp_id=$1)", &[&corp.id]).await.unwrap();
                db.execute("DELETE FROM game.contracts WHERE issuer_corp_id=$1", &[&corp.id]).await.unwrap();
            }
        }
        client.world.unregister_pilot_generation(corp.actor.character, None);
    }).await;
    f::cleanup(&db, corp).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn nested_active_blueprints_refuse_escrow_until_delivered() {
    use infinity_core::consts::{INDUSTRY_JOB_STATUS_INSTALLED, INDUSTRY_JOB_STATUS_READY};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (station_id, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let charge = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let bp_type = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(9)).unwrap().type_id;
    let corp = f::setup(&db, station_id, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut client, _peer) = crate::services::tests::client(db.clone(), corp.actor.character).await;
        f::session(&mut client, corp, station_id, system, ACCOUNT_CASH);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(corp.actor.character, tx);
        for corporate in [false, true] {
            let owner = if corporate { corp.id } else { corp.actor.character };
            let flag = if corporate { f::DELIVERIES } else { FLAG_HANGAR };
            let root = transfers::insert(&db, charge, owner, station_id, flag, 1).await.unwrap();
            let bp = transfers::insert(&db, bp_type, owner, root, FLAG_HANGAR, 1).await.unwrap();
            db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&bp]).await.unwrap();
            crate::dao::industry::set_blueprint(&db, bp, 0, 0, -1).await.unwrap();
            let job = crate::dao::industry::Job {
                id: 0, installer: corp.actor.character, owner, activity: 1, blueprint_id: bp,
                blueprint_type: bp_type, blueprint_copy: false, facility: station_id, system,
                runs: 1, licensed_runs: 1, successful_runs: None, product_type: None, optional_type: None,
                cost: 0.0, probability: 1.0, time: 1, start: 0, end: 1, completed: None,
                completed_by: None, status: INDUSTRY_JOB_STATUS_INSTALLED, input_location: station_id,
                input_flag: flag, output_location: station_id, output_flag: flag, wallet_owner: owner,
                wallet_account_key: ACCOUNT_CASH, blueprint_location: root, blueprint_flag: FLAG_HANGAR,
            };
            let job_id = crate::dao::industry::insert_job(&db, &job).await.unwrap();
            let input = request(station_id, flag, root, corporate, 0);
            for status in [INDUSTRY_JOB_STATUS_INSTALLED, INDUSTRY_JOB_STATUS_READY] {
                db.execute("UPDATE game.industry_jobs SET status=$2 WHERE job_id=$1", &[&job_id, &status]).await.unwrap();
                let before = f::fingerprint(&db, corp).await;
                match create(&client, &input).await {
                    Err(CallError::User { msg, dict }) => {
                        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                        assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::industry::BLUEPRINT_ACTIVE))]);
                    }
                    _ => panic!("expected active blueprint refusal"),
                }
                assert_eq!(f::fingerprint(&db, corp).await, before);
                assert!(rx.try_recv().is_err());
            }
            db.execute("UPDATE game.industry_jobs SET status=101 WHERE job_id=$1", &[&job_id]).await.unwrap();
            assert!(create(&client, &input).await.is_ok());
            while rx.try_recv().is_ok() {}
        }
        client.world.unregister_pilot_generation(corp.actor.character, None);
    }).await;
    f::cleanup(&db, corp).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn stale_acceptance_guard_rolls_back_real_wallet_and_item_transfers() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (station_id, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let issuer = f::setup(&db, station_id, station_type).await;
    let buyer = f::setup(&db, station_id, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _p1) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        let (mut purchaser, _p2) = crate::services::tests::client(db.clone(), buyer.actor.character).await;
        f::session(&mut seller, issuer, station_id, system, ACCOUNT_CASH);
        f::session(&mut purchaser, buyer, station_id, system, ACCOUNT_CASH);
        let item = transfers::insert(&db, kind, issuer.id, station_id, f::DELIVERIES, 1).await.unwrap();
        let id = f::id(create(&seller, &f::payload(TYPE_ITEM_EXCHANGE, station_id, 0, f::DELIVERIES, item, 0.0, 0.0, 0.0)).await);
        assert!(accept(&purchaser, id, true).await.is_ok());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        purchaser.world.register_pilot(buyer.actor.character, tx);
        let before_issuer = f::fingerprint(&db, issuer).await;
        let before_buyer = f::fingerprint(&db, buyer).await;
        assert!(matches!(accept(&purchaser, id, true).await, Err(CallError::User { .. })));
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        let _guard = lock().await;
        let world = &*purchaser.world;
        let result: Result<(), CallError> = db.transaction(
            |tx, outbox: &mut Outbox| {
                let sde = Arc::clone(&sde);
                Box::pin(async move {
                    wallet_ownership::debit(tx, outbox, wallet_ownership::FinancialOwner { id: buyer.id, wallet_key: ACCOUNT_CASH }, IskAmount::from_cents(1_000).unwrap()).await?;
                    let preparation = custody::prepare(tx, &sde, &[item], &[]).await?;
                    custody::hand_over(tx, &sde, outbox, &[item], custody::receiving(issuer.id, station_id), station_id, &preparation, &[]).await?;
                    if !contract_dao::accept_exchange(tx, id, STATUS_OUTSTANDING, buyer.id, ACCOUNT_CASH, 1, STATUS_FINISHED, 1).await.map_err(db_err)? {
                        return Err(user_error(messages::MSG_CON_CONTRACT_NOT_OUTSTANDING, vec![]));
                    }
                    Ok(())
                })
            },
            |value, outbox| async move { dispatch_world(value, outbox, world) },
        ).await;
        assert!(matches!(result, Err(CallError::User { .. })));
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        assert!(rx.try_recv().is_err());
        purchaser.world.unregister_pilot_generation(buyer.actor.character, None);
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn courier_failure_uses_current_custody_and_missing_package_destroys_descendants() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let mut stations = sde.map.stations().filter(|s| s.type_id.is_some() && s.solar_system_id.is_some());
    let station = stations.next().unwrap();
    let end = stations.find(|s| s.station_id != station.station_id).unwrap().station_id;
    let (start, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let issuer = f::setup(&db, start, station_type).await;
    let buyer = f::setup(&db, start, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _p1) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        let (mut courier, _p2) = crate::services::tests::client(db.clone(), buyer.actor.character).await;
        f::session(&mut seller, issuer, start, system, ACCOUNT_CASH + 1);
        f::session(&mut courier, buyer, start, system, ACCOUNT_CASH + 1);
        for mode in 0..3 {
            f::session(&mut courier, buyer, start, system, ACCOUNT_CASH + 1);
            let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
            let child = transfers::insert(&db, kind, issuer.id, root, FLAG_HANGAR, 1).await.unwrap();
            let grandchild = transfers::insert(&db, kind, issuer.id, child, FLAG_HANGAR, 1).await.unwrap();
            let id = f::id(create(&seller, &f::payload(TYPE_COURIER, start, end, f::DELIVERIES, root, 0.0, 20.0, 50.0)).await);
            assert!(accept(&courier, id, true).await.is_ok());
            let package: i64 = db.query_one("SELECT package_id FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap().get("package_id");
            db.execute("UPDATE game.characters SET corp_roles=0,corp_roles_hq=0 WHERE character_id=$1", &[&buyer.actor.character]).await.unwrap();
            let before = f::fingerprint(&db, buyer).await;
            assert!(matches!(complete(&courier, id, STATUS_FAILED).await, Err(CallError::User { .. })));
            assert_eq!(f::fingerprint(&db, buyer).await, before);
            let roles = infinity_core::consts::ROLE_ACCOUNTANT | infinity_core::consts::ROLE_CONTRACT_MANAGER | infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1 | infinity_core::consts::ROLE_ACCOUNT_CAN_TAKE_1 | (infinity_core::consts::ROLE_ACCOUNT_CAN_TAKE_1 << 1) | (infinity_core::consts::ROLE_ACCOUNT_CAN_TAKE_1 << 2);
            db.execute("UPDATE game.characters SET corp_roles=$2::bigint & ~$3::bigint,corp_roles_hq=$2::bigint & $3::bigint WHERE character_id=$1", &[&buyer.actor.character, &roles, &infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
            if mode == 2 {
                db.execute("DELETE FROM game.items WHERE item_id=$1", &[&package]).await.unwrap();
            } else {
                let owner = if mode == 0 { buyer.ceo.character } else { buyer.id };
                let flag = if mode == 0 { FLAG_HANGAR } else { f::DELIVERIES };
                db.execute("UPDATE game.items SET owner_id=$2,location_id=$3,flag_id=$4 WHERE item_id=$1", &[&package, &owner, &end, &flag]).await.unwrap();
                f::session(&mut courier, buyer, end, system, ACCOUNT_CASH + 2);
                if mode == 0 {
                    let before = f::fingerprint(&db, buyer).await;
                    assert!(matches!(complete(&courier, id, STATUS_FINISHED).await, Err(CallError::User { .. })));
                    assert_eq!(f::fingerprint(&db, buyer).await, before);
                }
            }
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            courier.world.register_pilot(issuer.actor.character, tx);
            f::session(&mut courier, buyer, start, system, ACCOUNT_CASH + 2);
            assert!(complete(&courier, id, STATUS_FAILED).await.is_ok());
            assert!(rx.try_recv().is_ok());
            courier.world.unregister_pilot_generation(issuer.actor.character, None);
            if mode == 2 {
                let count: i64 = db.query_one("SELECT count(*) AS count FROM game.items WHERE item_id=ANY($1)", &[&vec![root, child, grandchild]]).await.unwrap().get("count");
                assert_eq!(count, 0);
            } else {
                let row = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
                let owner = if mode == 0 { buyer.ceo.character } else { buyer.id };
                let flag = if mode == 0 { FLAG_HANGAR } else { f::DELIVERIES };
                assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (owner, end, flag));
                let row = db.query_one("SELECT owner_id,location_id FROM game.items WHERE item_id=$1", &[&grandchild]).await.unwrap();
                assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id")), (owner, child));
            }
        }
        let balance = crate::services::wallets::corp_balance(&db, issuer.id, ACCOUNT_CASH + 1).await.unwrap();
        assert_eq!(balance, IskAmount::from_cents(97_015_000).unwrap());
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn stale_corporation_session_cannot_accept_or_bid_on_corporate_assignment() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (station_id, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let issuer = f::setup(&db, station_id, station_type).await;
    let buyer = f::setup(&db, station_id, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _p1) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        let (mut purchaser, _p2) = crate::services::tests::client(db.clone(), buyer.actor.character).await;
        f::session(&mut seller, issuer, station_id, system, ACCOUNT_CASH);
        f::session(&mut purchaser, buyer, station_id, system, ACCOUNT_CASH);
        let item = transfers::insert(&db, kind, issuer.id, station_id, f::DELIVERIES, 1).await.unwrap();
        let exchange = f::id(create(&seller, &request(station_id, f::DELIVERIES, item, true, buyer.id)).await);
        let auction_item = transfers::insert(&db, kind, issuer.id, station_id, f::DELIVERIES, 1).await.unwrap();
        let auction_request = Value::dict([
            ("contractType", Value::Int(i64::from(TYPE_AUCTION))),
            ("startStationID", Value::Int(station_id)),
            ("startStationDivision", Value::Int(i64::from(f::DELIVERIES))),
            ("minutesExpire", Value::Int(1440)), ("forCorp", Value::Bool(true)),
            ("assignedToID", Value::Int(buyer.id)),
            ("price", Value::Float(10.0)),
            ("itemList", Value::list([Value::tuple([Value::Int(auction_item), Value::Int(1)])])),
        ]);
        let auction = f::id(create(&seller, &auction_request).await);
        db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&buyer.actor.character]).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        purchaser.world.register_pilot(buyer.actor.character, tx);
        let before_issuer = f::fingerprint(&db, issuer).await;
        let before_buyer = f::fingerprint(&db, buyer).await;
        assert!(matches!(accept(&purchaser, exchange, false).await, Err(CallError::User { .. })));
        assert!(matches!(accept(&purchaser, exchange, true).await, Err(CallError::User { .. })));
        assert!(matches!(place_bid(&purchaser, auction, 10.0, false).await, Err(CallError::User { .. })));
        assert!(matches!(place_bid(&purchaser, auction, 10.0, true).await, Err(CallError::User { .. })));
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        assert!(rx.try_recv().is_err());
        purchaser.world.unregister_pilot_generation(buyer.actor.character, None);
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn corporate_member_completion_requires_personal_holder_or_corporate_package() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let mut stations = sde.map.stations().filter(|s| s.type_id.is_some() && s.solar_system_id.is_some());
    let station = stations.next().unwrap();
    let end = stations.find(|s| s.station_id != station.station_id).unwrap().station_id;
    let (start, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let issuer = f::setup(&db, start, station_type).await;
    let buyer = f::setup(&db, start, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _p1) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        let (mut member, _p2) = crate::services::tests::client(db.clone(), buyer.actor.character).await;
        let (mut holder, _p3) = crate::services::tests::client(db.clone(), buyer.ceo.character).await;
        f::session(&mut seller, issuer, start, system, ACCOUNT_CASH);
        f::session(&mut member, buyer, start, system, ACCOUNT_CASH);
        f::session(&mut holder, buyer, end, system, ACCOUNT_CASH);
        for personal in [true, false] {
            let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
            let id = f::id(create(&seller, &f::payload(TYPE_COURIER, start, end, f::DELIVERIES, root, 0.0, 0.0, 0.0)).await);
            assert!(accept(&member, id, true).await.is_ok());
            let package: i64 = db.query_one("SELECT package_id FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap().get("package_id");
            let owner = if personal { buyer.ceo.character } else { buyer.id };
            let flag = if personal { FLAG_HANGAR } else { f::DELIVERIES };
            db.execute("UPDATE game.items SET owner_id=$2,location_id=$3,flag_id=$4 WHERE item_id=$1", &[&package, &owner, &end, &flag]).await.unwrap();
            f::session(&mut member, buyer, end, system, ACCOUNT_CASH);
            if personal {
                let before_issuer = f::fingerprint(&db, issuer).await;
                let before_buyer = f::fingerprint(&db, buyer).await;
                assert!(matches!(complete(&member, id, STATUS_FINISHED).await, Err(CallError::User { .. })));
                assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
                assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
                assert!(complete(&holder, id, STATUS_FINISHED).await.is_ok());
            } else {
                db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&buyer.actor.character]).await.unwrap();
                assert!(complete(&member, id, STATUS_FINISHED).await.is_ok());
            }
            f::session(&mut member, buyer, start, system, ACCOUNT_CASH);
        }
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn courier_completion_notifies_authorized_corporate_item_viewers_after_commit() {
    use infinity_core::consts::{IX_FLAG, IX_LOCATION_ID, IX_OWNER_ID};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let (start, end, system, station_type) = {
        let mut stations = sde.map.stations().filter(|s| s.type_id.is_some() && s.solar_system_id.is_some());
        let station = stations.next().unwrap();
        let end = stations.find(|s| s.station_id != station.station_id).unwrap().station_id;
        (station.station_id, end, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap())
    };
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let issuer = f::setup(&db, start, station_type).await;
    let buyer = f::setup(&db, start, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _p1) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        let (mut courier, _p2) = crate::services::tests::client(db.clone(), buyer.actor.character).await;
        f::session(&mut seller, issuer, start, system, ACCOUNT_CASH);
        f::session(&mut courier, buyer, start, system, ACCOUNT_CASH);
        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let id = f::id(create(&seller, &f::payload(TYPE_COURIER, start, end, f::DELIVERIES, root, 0.0, 0.0, 0.0)).await);
        assert!(accept(&courier, id, true).await.is_ok());
        let package: i64 = db.query_one("SELECT package_id FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap().get("package_id");
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&package, &end]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&issuer.actor.character]).await.unwrap();
        let (authorized_tx, mut authorized_rx) = tokio::sync::mpsc::unbounded_channel();
        let (denied_tx, mut denied_rx) = tokio::sync::mpsc::unbounded_channel();
        courier.world.register_pilot(issuer.ceo.character, authorized_tx);
        courier.world.register_pilot(issuer.actor.character, denied_tx);
        f::session(&mut courier, buyer, end, system, ACCOUNT_CASH);
        assert!(complete(&courier, id, STATUS_FINISHED).await.is_ok());
        let item = load_items(&db, &sde, ItemQuery::Id(root)).await.unwrap().pop().unwrap();
        assert_eq!((item.owner_id, item.location_id, item.flag_id), (issuer.id, end, f::DELIVERIES));
        let mut saw_item = false;
        let mut saw_completed = false;
        while let Ok((event, args)) = authorized_rx.try_recv() {
            if event == "OnItemChange" {
                assert_eq!(args, vec![item.row(), Value::Dict(vec![
                    (Value::Int(IX_LOCATION_ID), Value::Int(-id)),
                    (Value::Int(IX_OWNER_ID), Value::Int(issuer.id)),
                    (Value::Int(IX_FLAG), Value::Int(i64::from(FLAG_HANGAR))),
                ]), Value::None]);
                saw_item = true;
            } else if event == "OnContractCompleted" { saw_completed = true; }
        }
        assert!(saw_item && saw_completed);
        let mut denied_completed = false;
        while let Ok((event, _)) = denied_rx.try_recv() {
            assert_ne!(event, "OnItemChange");
            if event == "OnContractCompleted" { denied_completed = true; }
        }
        assert!(denied_completed);
        courier.world.unregister_pilot_generation(issuer.ceo.character, None);
        courier.world.unregister_pilot_generation(issuer.actor.character, None);
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}
