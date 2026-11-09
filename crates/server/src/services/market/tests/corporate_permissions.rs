//! Corporate market authority is checked against current membership and division roles.
use super::*;
use super::corporate_fixture::CorporateFixture;
use super::super::{authorization, lifecycle, trading::place_buy};
use infinity_core::consts::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_JUNIOR_ACCOUNTANT};
use crate::services::{identity::Identity, tests::{client, fixture}};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn read_only_roles_cannot_trade_and_other_director_cannot_cancel() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        fixture::assign_corporation(&test_db, f.counterparty.character, f.corporation, ROLE_ACCOUNTANT).await.unwrap();
        let (mut issuer, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut member, _b) = client(test_db.clone(), f.counterparty.character).await;
        for c in [&mut issuer, &mut member] { c.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]); }
        let key = ACCOUNT_CASH + 1;
        let identity = Identity::of_client(&member);
        assert!(authorization::readable_divisions(&test_db, &identity, Some(ACCOUNT_CASH - 1)).await.is_err());
        for role in [ROLE_ACCOUNTANT, ROLE_JUNIOR_ACCOUNTANT] {
            fixture::assign_corporation(&test_db, f.counterparty.character, f.corporation, role).await.unwrap();
            assert!(authorization::readable_divisions(&test_db, &identity, Some(key)).await.is_ok());
            assert!(authorization::authorize_trade(&test_db, &identity, Some(key)).await.is_err());
        }
        fixture::assign_corporation(&test_db, f.counterparty.character, f.corporation, ROLE_DIRECTOR).await.unwrap();
        let order = place_buy(&Identity::of_client(&issuer), &issuer.db, &issuer.world, &issuer.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, Some(key)).await.unwrap();
        assert!(lifecycle::cancel_order(&identity, &member.db, &member.world, &member.sde, order.id, f.at.region).await.is_err());
        assert!(orders::my_order(&test_db, f.issuer.character, order.id).await.unwrap().is_some());
        lifecycle::cancel_order(&Identity::of_client(&issuer), &issuer.db, &issuer.world, &issuer.sde, order.id, f.at.region).await.unwrap();
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn title_hq_trader_source_validation_and_revocation() {
    use infinity_core::consts::{FLAG_CORP_SAG1, FLAG_HANGAR, ROLE_ACCOUNT_CAN_TAKE_1, ROLE_HANGAR_CAN_TAKE_1, ROLE_TRADER};
    use super::super::trading::place_sell;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let foreign = CorporateFixture::create(&db).await;
    let unauthorized = fixture::create(&db).await.unwrap();
    let departed = fixture::create(&db).await.unwrap();
    let new_viewer = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let key = ACCOUNT_CASH;
        fixture::assign_corporation(&test_db, unauthorized.character, f.corporation, 0).await.unwrap();
        fixture::assign_corporation(&test_db, departed.character, f.corporation, ROLE_DIRECTOR).await.unwrap();
        fixture::assign_corporation(&test_db, departed.character, 1, 0).await.unwrap();
        fixture::assign_corporation(&test_db, f.counterparty.character, f.corporation, 0).await.unwrap();
        test_db.execute("UPDATE game.characters SET corp_title_mask=1 WHERE character_id=$1", &[&f.counterparty.character]).await.unwrap();
        test_db.execute("INSERT INTO game.corp_titles(corporation_id,title_id,name,roles,roles_hq) VALUES ($1,1,'Market test',$2,$3)", &[&f.corporation, &(ROLE_TRADER | ROLE_ACCOUNT_CAN_TAKE_1), &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        test_db.execute("INSERT INTO game.corp_wallets(corporation_id,account_key,balance) VALUES ($1,$2,10000000) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&f.corporation, &key]).await.unwrap();
        let (mut trader, _peer) = client(test_db.clone(), f.counterparty.character).await;
        trader.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let (director_tx, mut director_rx) = tokio::sync::mpsc::unbounded_channel();
        let (trader_tx, mut trader_rx) = tokio::sync::mpsc::unbounded_channel();
        let (unauthorized_tx, mut unauthorized_rx) = tokio::sync::mpsc::unbounded_channel();
        let (departed_tx, mut departed_rx) = tokio::sync::mpsc::unbounded_channel();
        trader.world.register_pilot(f.issuer.character, director_tx);
        trader.world.register_pilot(f.counterparty.character, trader_tx);
        trader.world.register_pilot(unauthorized.character, unauthorized_tx);
        trader.world.register_pilot(departed.character, departed_tx);
        let item: i64 = test_db.query_one("INSERT INTO game.items(type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&f.type_id, &f.corporation, &f.office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
        let request = SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, duration: 1, office_id: Some(f.office), expected_brokers_fee: None };
        let identity = Identity::of_client(&trader);
        let remote = trader.sde.map.stations().find(|s| s.station_id != f.at.id && s.solar_system_id == Some(f.at.system) && s.region_id == Some(f.at.region)).unwrap().station_id;
        let before = f.fingerprint(&test_db).await;
        for invalid in [
            SellRequest { office_id: None, ..request },
            SellRequest { station_id: 0, ..request },
            SellRequest { station_id: remote, ..request },
            SellRequest { office_id: Some(f.office + 1), ..request },
            SellRequest { type_id: i64::from(f.type_id) + 1, ..request },
            SellRequest { quantity: 3, ..request },
        ] {
            assert!(place_sell(&identity, &test_db, &trader.world, &trader.sde, invalid, Some(key)).await.is_err());
            assert_eq!(f.fingerprint(&test_db).await, before);
        }
        for (column, value) in [("owner_id", f.issuer.character), ("flag_id", i64::from(FLAG_HANGAR)), ("singleton", 1)] {
            match column {
                "owner_id" => { test_db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&item, &value]).await.unwrap(); }
                "flag_id" => { test_db.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=$1", &[&item, &(value as i32)]).await.unwrap(); }
                _ => { test_db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&item]).await.unwrap(); }
            }
            let changed = f.fingerprint(&test_db).await;
            assert!(place_sell(&identity, &test_db, &trader.world, &trader.sde, request, Some(key)).await.is_err());
            assert_eq!(f.fingerprint(&test_db).await, changed);
            test_db.execute("UPDATE game.items SET owner_id=$2,flag_id=$3,singleton=false WHERE item_id=$1", &[&item, &f.corporation, &FLAG_CORP_SAG1]).await.unwrap();
        }
        test_db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&item, &foreign.office]).await.unwrap();
        let foreign_office = f.fingerprint(&test_db).await;
        assert!(place_sell(&identity, &test_db, &trader.world, &trader.sde, SellRequest { office_id: Some(foreign.office), ..request }, Some(key)).await.is_err());
        assert_eq!(f.fingerprint(&test_db).await, foreign_office);
        test_db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&item, &f.office]).await.unwrap();
        let blueprint_type = trader.sde.types.iter().find(|t| t.type_id > 0 && t.published == Some(true) && t.market_group_id.is_some() && t.category_id == Some(infinity_core::consts::CATEGORY_BLUEPRINT)).expect("marketable blueprint").type_id;
        let blueprint: i64 = test_db.query_one("INSERT INTO game.items(type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&blueprint_type, &f.corporation, &f.office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
        test_db.execute("INSERT INTO game.industry_jobs (installer_id,owner_id,activity_id,blueprint_id,blueprint_type_id,blueprint_copy,facility_id,solar_system_id,runs,cost,time_seconds,start_date,end_date,status) VALUES ($1,$2,1,$3,$4,false,$5,$6,1,0,1,0,1,$7)", &[&f.counterparty.character, &f.corporation, &blueprint, &blueprint_type, &f.at.id, &f.at.system, &infinity_core::consts::INDUSTRY_JOB_STATUS_INSTALLED]).await.unwrap();
        let busy = f.fingerprint(&test_db).await;
        assert!(place_sell(&identity, &test_db, &trader.world, &trader.sde, SellRequest { item_id: blueprint, type_id: i64::from(blueprint_type), ..request }, Some(key)).await.is_err());
        assert_eq!(f.fingerprint(&test_db).await, busy);
        test_db.execute("DELETE FROM game.industry_jobs WHERE blueprint_id=$1", &[&blueprint]).await.unwrap();
        test_db.execute("UPDATE game.corp_titles SET roles_hq=0 WHERE corporation_id=$1 AND title_id=1", &[&f.corporation]).await.unwrap();
        let without_hq = f.fingerprint(&test_db).await;
        assert!(place_sell(&identity, &test_db, &trader.world, &trader.sde, request, Some(key)).await.is_err());
        assert_eq!(f.fingerprint(&test_db).await, without_hq);
        test_db.execute("UPDATE game.corp_titles SET roles_hq=$2 WHERE corporation_id=$1 AND title_id=1", &[&f.corporation, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        for rx in [&mut director_rx, &mut trader_rx, &mut unauthorized_rx, &mut departed_rx] {
            assert!(rx.try_recv().is_err(), "refused source emitted a notification");
        }
        assert!(trader.outbox.is_empty());
        place_sell(&identity, &test_db, &trader.world, &trader.sde, request, Some(key)).await.unwrap();
        let director_notices: Vec<_> = std::iter::from_fn(|| director_rx.try_recv().ok()).collect();
        assert_eq!(director_notices.iter().filter(|(name, _)| name == "OnItemChange").count(), 1);
        let trader_notices: Vec<_> = std::iter::from_fn(|| trader_rx.try_recv().ok()).collect();
        let highsec = trader.sde.map.system(f.at.system).and_then(|s| s.security).is_some_and(|security| security >= crate::config::game().crimewatch.highsec_threshold);
        for notices in [&director_notices, &trader_notices] {
            assert_eq!(notices.iter().filter(|(name, _)| name == "OnOwnOrdersChanged").count(), 1);
            assert_eq!(notices.iter().filter(|(name, _)| name == "OnAccountChange").count(), if highsec { 2 } else { 1 });
        }
        for rx in [&mut unauthorized_rx, &mut departed_rx] {
            assert!(!std::iter::from_fn(|| rx.try_recv().ok()).any(|(name, _)| matches!(name.as_str(), "OnOwnOrdersChanged" | "OnAccountChange" | "OnItemChange" | "OnMarketItemReceived")));
        }
        let order: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND NOT bid", &[&f.counterparty.character]).await.unwrap().get("order_id");
        test_db.execute("UPDATE game.characters SET corp_title_mask=0 WHERE character_id=$1", &[&f.counterparty.character]).await.unwrap();
        assert!(lifecycle::cancel_order(&identity, &test_db, &trader.world, &trader.sde, order, f.at.region).await.is_err());
        assert!(orders::my_order(&test_db, f.counterparty.character, order).await.unwrap().is_some());
        fixture::assign_corporation(&test_db, new_viewer.character, f.corporation, ROLE_DIRECTOR).await.unwrap();
        let (new_tx, mut new_rx) = tokio::sync::mpsc::unbounded_channel();
        trader.world.register_pilot(new_viewer.character, new_tx);
        let (mut buyer, _buyer_peer) = client(test_db.clone(), f.issuer.character).await;
        buyer.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        place_buy(&Identity::of_client(&buyer), &test_db, &trader.world, &buyer.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, None).await.unwrap();
        let current: Vec<_> = std::iter::from_fn(|| new_rx.try_recv().ok()).collect();
        assert_eq!(current.iter().filter(|(name, _)| name == "OnOwnOrdersChanged").count(), 1);
        assert_eq!(current.iter().filter(|(name, _)| name == "OnAccountChange").count(), 1);
        for rx in [&mut trader_rx, &mut unauthorized_rx, &mut departed_rx] {
            assert!(!std::iter::from_fn(|| rx.try_recv().ok()).any(|(name, _)| matches!(name.as_str(), "OnOwnOrdersChanged" | "OnAccountChange" | "OnItemChange" | "OnMarketItemReceived")));
        }
    }).await;
    db.execute("DELETE FROM game.industry_jobs WHERE owner_id=$1", &[&f.corporation]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&f.corporation]).await.unwrap();
    fixture::assign_corporation(&db, unauthorized.character, 1, 0).await.unwrap();
    fixture::cleanup(&db, unauthorized).await.unwrap();
    fixture::cleanup(&db, departed).await.unwrap();
    fixture::assign_corporation(&db, new_viewer.character, 1, 0).await.unwrap();
    fixture::cleanup(&db, new_viewer).await.unwrap();
    f.cleanup(&db).await;
    foreign.cleanup(&db).await;
    scenario.unwrap();
}
