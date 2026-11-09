//! Corporate contract creation and source custody regressions.
use super::super::*;
use super::corporate_fixture as f;
use infinity_core::consts::{ROLE_ACCOUNTANT, ROLE_ACCOUNT_CAN_TAKE_1, ROLE_CONTRACT_MANAGER, ROLE_HANGAR_CAN_TAKE_1};

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn corporate_creation_authority_and_office_return() {
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
        let item = transfers::insert(&db, kind, corp.id, corp.office, f::SAG, 1).await.unwrap();
        let child = transfers::insert(&db, kind, corp.id, item, FLAG_HANGAR, 1).await.unwrap();
        let request = f::payload(TYPE_ITEM_EXCHANGE, station_id, 0, f::SAG, item, 0.0, 0.0, 0.0);
        for roles in [ROLE_ACCOUNTANT | ROLE_HANGAR_CAN_TAKE_1 | (ROLE_ACCOUNT_CAN_TAKE_1 << 1), ROLE_CONTRACT_MANAGER | ROLE_HANGAR_CAN_TAKE_1, ROLE_CONTRACT_MANAGER | ROLE_ACCOUNTANT | (ROLE_ACCOUNT_CAN_TAKE_1 << 1), ROLE_CONTRACT_MANAGER | ROLE_ACCOUNTANT | ROLE_HANGAR_CAN_TAKE_1] {
            db.execute("UPDATE game.characters SET corp_roles=$2::bigint & ~$3::bigint,corp_roles_hq=$2::bigint & $3::bigint WHERE character_id=$1", &[&corp.actor.character, &roles, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
            let before = f::fingerprint(&db, corp).await;
            assert!(create(&client, &request).await.is_err());
            assert_eq!(f::fingerprint(&db, corp).await, before);
            assert!(rx.try_recv().is_err());
        }
        let roles = ROLE_ACCOUNTANT | ROLE_CONTRACT_MANAGER | ROLE_HANGAR_CAN_TAKE_1 | (ROLE_ACCOUNT_CAN_TAKE_1 << 1);
        db.execute("UPDATE game.characters SET corp_roles=$2::bigint & ~$3::bigint,corp_roles_hq=$2::bigint & $3::bigint WHERE character_id=$1", &[&corp.actor.character, &roles, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&corp.actor.character]).await.unwrap();
        let before = f::fingerprint(&db, corp).await;
        assert!(create(&client, &request).await.is_err());
        assert_eq!(f::fingerprint(&db, corp).await, before);
        assert!(rx.try_recv().is_err());
        db.execute("UPDATE game.characters SET corporation_id=$2 WHERE character_id=$1", &[&corp.actor.character, &corp.id]).await.unwrap();
        let id = f::id(create(&client, &request).await);
        let row = db.query_one("SELECT issuer_id,for_corp,issuer_wallet_key,issuer_hangar_flag,issuer_source_location FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        assert_eq!(row.get::<_, i64>("issuer_id"), corp.actor.character);
        assert!(row.get::<_, bool>("for_corp"));
        assert_eq!(row.get::<_, i32>("issuer_wallet_key"), ACCOUNT_CASH + 1);
        assert_eq!(row.get::<_, i32>("issuer_hangar_flag"), f::SAG);
        assert_eq!(row.get::<_, i64>("issuer_source_location"), corp.office);
        assert!(rx.try_recv().is_ok());
        { let _guard = lock().await; assert!(delete_one(&client, id).await.is_ok()); }
        let row = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (corp.id, corp.office, f::SAG));
        assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&child]).await.unwrap().get::<_, i64>("location_id"), item);
        let id = f::id(create(&client, &request).await);
        db.execute("DELETE FROM game.offices WHERE office_id=$1", &[&corp.office]).await.unwrap();
        { let _guard = lock().await; assert!(delete_one(&client, id).await.is_ok()); }
        let row = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (corp.id, station_id, f::DELIVERIES));
        assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&child]).await.unwrap().get::<_, i64>("location_id"), item);
        client.world.unregister_pilot_generation(corp.actor.character, None);
    }).await;
    f::cleanup(&db, corp).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn active_blueprint_jobs_block_personal_and_corporate_escrow() {
    use infinity_core::consts::{INDUSTRY_JOB_STATUS_INSTALLED, INDUSTRY_JOB_STATUS_READY};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (station_id, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let bp_type = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(9)).unwrap().type_id;
    let corp = f::setup(&db, station_id, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut client, _peer) = crate::services::tests::client(db.clone(), corp.actor.character).await;
        f::session(&mut client, corp, station_id, system, ACCOUNT_CASH + 1);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(corp.actor.character, tx);
        for corporate in [false, true] {
            let owner = if corporate { corp.id } else { corp.actor.character };
            let flag = if corporate { f::DELIVERIES } else { FLAG_HANGAR };
            let bp = transfers::insert(&db, bp_type, owner, station_id, flag, 1).await.unwrap();
            db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&bp]).await.unwrap();
            crate::dao::industry::set_blueprint(&db, bp, 0, 0, -1).await.unwrap();
            let job = crate::dao::industry::Job {
                id: 0, installer: corp.actor.character, owner, activity: 1, blueprint_id: bp,
                blueprint_type: bp_type, blueprint_copy: false, facility: station_id, system,
                runs: 1, licensed_runs: 1, successful_runs: None, product_type: None, optional_type: None,
                cost: 0.0, probability: 1.0, time: 1, start: 0, end: 1, completed: None,
                completed_by: None, status: INDUSTRY_JOB_STATUS_INSTALLED, input_location: station_id,
                input_flag: flag, output_location: station_id, output_flag: flag, wallet_owner: owner,
                wallet_account_key: ACCOUNT_CASH, blueprint_location: station_id, blueprint_flag: flag,
            };
            let job_id = crate::dao::industry::insert_job(&db, &job).await.unwrap();
            let request = Value::dict([
                ("contractType", Value::Int(i64::from(TYPE_ITEM_EXCHANGE))),
                ("startStationID", Value::Int(station_id)), ("startStationDivision", Value::Int(i64::from(flag))),
                ("minutesExpire", Value::Int(1440)), ("forCorp", Value::Bool(corporate)),
                ("itemList", Value::list([Value::tuple([Value::Int(bp), Value::Int(1)])])),
                ("requestItemTypeList", Value::list([])),
            ]);
            for status in [INDUSTRY_JOB_STATUS_INSTALLED, INDUSTRY_JOB_STATUS_READY] {
                db.execute("UPDATE game.industry_jobs SET status=$2 WHERE job_id=$1", &[&job_id, &status]).await.unwrap();
                let before = f::fingerprint(&db, corp).await;
                match create(&client, &request).await {
                    Err(CallError::User { msg, dict }) => {
                        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                        assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::industry::BLUEPRINT_ACTIVE))]);
                    }
                    _ => panic!("expected active blueprint refusal"),
                }
                assert_eq!(f::fingerprint(&db, corp).await, before);
                assert!(rx.try_recv().is_err());
            }
            // industry.const STATUS_DELIVERED is terminal.
            db.execute("UPDATE game.industry_jobs SET status=101 WHERE job_id=$1", &[&job_id]).await.unwrap();
            assert!(create(&client, &request).await.is_ok());
            while rx.try_recv().is_ok() {}
        }
        client.world.unregister_pilot_generation(corp.actor.character, None);
    }).await;
    f::cleanup(&db, corp).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn corporate_exchange_transfers_requested_blueprint_and_descendants() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (station_id, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let bp_type = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(9)).unwrap().type_id;
    let charge = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let issuer = f::setup(&db, station_id, station_type).await;
    let buyer = f::setup(&db, station_id, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _p1) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        let (mut purchaser, _p2) = crate::services::tests::client(db.clone(), buyer.actor.character).await;
        f::session(&mut seller, issuer, station_id, system, ACCOUNT_CASH + 1);
        f::session(&mut purchaser, buyer, station_id, system, ACCOUNT_CASH + 2);
        let offered = transfers::insert(&db, charge, issuer.id, station_id, f::DELIVERIES, 1).await.unwrap();
        let bp = transfers::insert(&db, bp_type, buyer.id, station_id, f::DELIVERIES, 1).await.unwrap();
        db.execute("UPDATE game.items SET singleton=true,quantity=-2 WHERE item_id=$1", &[&bp]).await.unwrap();
        crate::dao::industry::set_blueprint(&db, bp, 7, 9, 3).await.unwrap();
        let child = transfers::insert(&db, charge, buyer.id, bp, FLAG_HANGAR, 1).await.unwrap();
        let request = Value::dict([
            ("contractType", Value::Int(i64::from(TYPE_ITEM_EXCHANGE))), ("startStationID", Value::Int(station_id)),
            ("startStationDivision", Value::Int(i64::from(f::DELIVERIES))), ("minutesExpire", Value::Int(1440)),
            ("forCorp", Value::Bool(true)), ("price", Value::Float(10.0)),
            ("itemList", Value::list([Value::tuple([Value::Int(offered), Value::Int(1)])])),
            ("requestItemTypeList", Value::list([Value::tuple([Value::Int(i64::from(bp_type)), Value::Int(1)])])),
        ]);
        let id = f::id(create(&seller, &request).await);
        let roles = ROLE_CONTRACT_MANAGER | (134_217_728_i64 << 2);
        db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&buyer.actor.character, &roles]).await.unwrap();
        let before = f::fingerprint(&db, buyer).await;
        assert!(accept(&purchaser, id, true).await.is_err());
        assert_eq!(f::fingerprint(&db, buyer).await, before);
        let roles = ROLE_CONTRACT_MANAGER | ROLE_ACCOUNTANT | (ROLE_ACCOUNT_CAN_TAKE_1 << 2);
        db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&buyer.actor.character, &roles]).await.unwrap();
        assert!(accept(&purchaser, id, true).await.is_ok());
        let row = db.query_one("SELECT i.owner_id,i.location_id,i.flag_id,i.quantity,i.singleton,b.material_efficiency,b.time_efficiency,b.runs FROM game.items i JOIN game.blueprints b USING(item_id) WHERE i.item_id=$1", &[&bp]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (issuer.id, station_id, f::DELIVERIES));
        assert_eq!((row.get::<_, i64>("quantity"), row.get::<_, bool>("singleton")), (-2, true));
        assert_eq!((row.get::<_, i32>("material_efficiency"), row.get::<_, i32>("time_efficiency"), row.get::<_, i32>("runs")), (7, 9, 3));
        let child_row = db.query_one("SELECT owner_id,location_id FROM game.items WHERE item_id=$1", &[&child]).await.unwrap();
        assert_eq!((child_row.get::<_, i64>("owner_id"), child_row.get::<_, i64>("location_id")), (issuer.id, bp));
        let row = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&offered]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (buyer.id, station_id, f::DELIVERIES));
        let before = f::fingerprint(&db, buyer).await;
        assert!(accept(&purchaser, id, true).await.is_err());
        assert_eq!(f::fingerprint(&db, buyer).await, before);
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn corporate_courier_member_moves_and_delivers_overdue_package() {
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
        let item = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let id = f::id(create(&seller, &f::payload(TYPE_COURIER, start, end, f::DELIVERIES, item, 0.0, 20.0, 50.0)).await);
        assert!(accept(&courier, id, true).await.is_ok());
        let row = db.query_one("SELECT package_id,acceptor_wallet_key FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        let package: i64 = row.get("package_id");
        assert_eq!(row.get::<_, i32>("acceptor_wallet_key"), ACCOUNT_CASH + 1);
        let wrap = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&package]).await.unwrap();
        assert_eq!((wrap.get::<_, i64>("owner_id"), wrap.get::<_, i64>("location_id"), wrap.get::<_, i32>("flag_id")), (buyer.id, start, f::DELIVERIES));
        let destination = crate::services::inventory::access::Container::Hangar { station_id: start, owner_id: buyer.actor.character };
        assert!(matches!(crate::services::inventory::movement::move_item(&crate::services::identity::Identity::of_client(&courier), &courier.db, &courier.world, &courier.sde, courier.session.int("solarsystemid"), courier.notify_tx.clone(), &destination, package, Some(start), Some(FLAG_HANGAR), None, |n| crate::services::effects::online::dispatch_notification(n, &courier.world, &courier.notify_tx, courier.session.int("charid"))).await, Ok(Some(id)) if id == package));
        db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&buyer.actor.character]).await.unwrap();
        assert!(complete(&courier, id, STATUS_FAILED).await.is_err());
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&package, &end]).await.unwrap();
        db.execute("UPDATE game.contracts SET date_accepted=0 WHERE contract_id=$1", &[&id]).await.unwrap();
        f::session(&mut courier, buyer, end, system, ACCOUNT_CASH + 2);
        assert!(complete(&courier, id, STATUS_FINISHED).await.is_ok());
        let row = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (issuer.id, end, f::DELIVERIES));
        let row = db.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&buyer.id, &(ACCOUNT_CASH + 1)]).await.unwrap();
        assert_eq!(row.get::<_, f64>("balance"), 1000020.0);
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}
