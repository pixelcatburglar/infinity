//! Courier wrap volume, persistence and movement regressions.
use super::super::*;
use super::corporate_fixture as f;

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn courier_volume_limit_and_wrap_persistence() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let mut stations = sde.map.stations().filter(|s| s.type_id.is_some() && s.solar_system_id.is_some());
    let station = stations.next().unwrap();
    let end = stations.find(|s| s.station_id != station.station_id).unwrap().station_id;
    let (start, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    drop(stations);
    let (fractional_type, fractional_volume) = sde.types.iter().find_map(|ty| {
        let v = crate::services::item_volume::item_volume(&sde, ty.type_id, false, 1, None).ok()?;
        (v > 0.0 && v < MAX_COURIER_VOLUME && (v * 100.0).fract() > 0.0).then_some((ty.type_id, v))
    }).expect("SDE needs a type with fractional hundredth volume");
    let issuer = f::setup(&db, start, station_type).await;
    let buyer = f::setup(&db, start, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut seller, _p1) = crate::services::tests::client(db.clone(), issuer.actor.character).await;
        let (mut courier, _p2) = crate::services::tests::client(db.clone(), buyer.actor.character).await;
        f::session(&mut seller, issuer, start, system, ACCOUNT_CASH + 1);
        f::session(&mut courier, buyer, start, system, ACCOUNT_CASH + 1);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        seller.world.register_pilot(issuer.actor.character, tx);
        let root = transfers::insert(&db, TYPE_PLASTIC_WRAP, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        db.execute("UPDATE game.items SET singleton=true, quantity=-120000001 WHERE item_id=$1", &[&root]).await.unwrap();
        let balance: String = db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&issuer.id, &(ACCOUNT_CASH + 1)]).await.unwrap().get("balance");
        db.execute("UPDATE game.corp_wallets SET balance=0 WHERE corporation_id=$1 AND account_key=$2", &[&issuer.id, &(ACCOUNT_CASH + 1)]).await.unwrap();
        let before = f::fingerprint(&db, issuer).await;
        assert!(rx.try_recv().is_err());
        let result = create(&seller, &f::payload(TYPE_COURIER, start, end, f::DELIVERIES, root, 0.0, 20.0, 50.0)).await;
        assert!(matches!(result, Err(CallError::User { msg, .. }) if msg == messages::MSG_NOT_ENOUGH_MONEY));
        assert_eq!(f::fingerprint(&db, issuer).await, before);
        assert!(rx.try_recv().is_err());
        db.execute("UPDATE game.corp_wallets SET balance=$3::text::numeric WHERE corporation_id=$1 AND account_key=$2", &[&issuer.id, &(ACCOUNT_CASH + 1), &balance]).await.unwrap();
        let before = f::fingerprint(&db, issuer).await;
        let result = create(&seller, &f::payload(TYPE_COURIER, start, end, f::DELIVERIES, root, 0.0, 20.0, 50.0)).await;
        match result {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, messages::MSG_CON_EXCEEDS_MAX_VOLUME);
                assert_eq!(dict, vec![("max".into(), Value::Float(MAX_COURIER_VOLUME)), ("vol".into(), Value::Float(1_200_000.01))]);
            }
            other => panic!("unexpected result: {other:?}"),
        }
        assert_eq!(f::fingerprint(&db, issuer).await, before);
        assert!(rx.try_recv().is_err());
        seller.world.unregister_pilot_generation(issuer.actor.character, None);
        db.execute("UPDATE game.items SET quantity=-120000000 WHERE item_id=$1", &[&root]).await.unwrap();
        let id = f::id(create(&seller, &f::payload(TYPE_COURIER, start, end, f::DELIVERIES, root, 0.0, 20.0, 50.0)).await);
        let row = db.query_one("SELECT volume FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        assert_eq!(row.get::<_, f64>("volume"), MAX_COURIER_VOLUME);
        assert!(accept(&courier, id, true).await.is_ok());
        let package: i64 = db.query_one("SELECT package_id FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap().get("package_id");
        let wrap = load_items(&db, &sde, ItemQuery::Id(package)).await.unwrap().pop().unwrap();
        assert_eq!(wrap.quantity, -120000000);
        assert_eq!(crate::services::item_descriptor().get(&wrap.row(), "quantity"), Some(Value::Int(-120000000)));
        assert_eq!(crate::services::item_volume::stack_size(wrap.singleton, i64::from(wrap.quantity)), 1);
        assert_eq!(crate::services::item_volume::item_volume(&sde, wrap.type_id, wrap.singleton, i64::from(wrap.quantity), None).unwrap(), MAX_COURIER_VOLUME);
        let destination = crate::services::inventory::access::Container::Hangar { station_id: start, owner_id: buyer.actor.character };
        assert!(matches!(crate::services::inventory::movement::move_item(&crate::services::identity::Identity::of_client(&courier), &courier.db, &courier.world, &courier.sde, courier.session.int("solarsystemid"), courier.notify_tx.clone(), &destination, package, Some(start), Some(FLAG_HANGAR), None, |n| crate::services::effects::online::dispatch_notification(n, &courier.world, &courier.notify_tx, courier.session.int("charid"))).await, Ok(Some(moved)) if moved == package));
        let moved = load_items(&db, &sde, ItemQuery::Id(package)).await.unwrap().pop().unwrap();
        assert_eq!(moved.quantity, -120000000);
        assert_eq!(crate::services::item_descriptor().get(&moved.row(), "quantity"), Some(Value::Int(-120000000)));
        let fractional_root = transfers::insert(&db, fractional_type, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let fractional_id = f::id(create(&seller, &f::payload(TYPE_COURIER, start, end, f::DELIVERIES, fractional_root, 0.0, 0.0, 0.0)).await);
        assert!(accept(&courier, fractional_id, true).await.is_ok());
        let row = db.query_one("SELECT i.quantity, c.volume FROM game.contracts c JOIN game.items i ON i.item_id=c.package_id WHERE c.contract_id=$1", &[&fractional_id]).await.unwrap();
        assert_eq!(row.get::<_, f64>("volume"), fractional_volume);
        assert_eq!(row.get::<_, i64>("quantity"), -(fractional_volume * 100.0).ceil() as i64);
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}
