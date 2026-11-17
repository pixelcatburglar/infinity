//! Dedicated database regressions for contract custody notifications.
use super::*;
use crate::db::{outbox::dispatch_world, Db};
use crate::services::tests::fixture;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn personal_escrow_departure_and_docked_arrival_notify_once() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let pilot = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    // Keep cleanup outside the task so assertion failures do not leave fixture rows.
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap());
        let station = sde.map.stations().next().unwrap().station_id;
        let type_id = sde.types.iter().find(|t| t.type_id > 0).unwrap().type_id;
        test_db.execute("UPDATE game.characters SET station_id = $2 WHERE character_id = $1", &[&pilot.character, &station]).await.unwrap();
        let root = transfers::insert(&test_db, type_id, pilot.character, station, FLAG_HANGAR, 1).await.unwrap();
        let escrow = -root;
        let world = crate::space::World::new(test_db.clone(), sde.clone(), 1);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        world.register_pilot(pilot.character, sender);
        for (source, destination) in [(station, escrow), (escrow, station), (station, station)] {
            let dest = Custody { owner: pilot.character, location: destination, flag: FLAG_HANGAR };
            let result: Result<(), CallError> = test_db.transaction(
                |tx, outbox: &mut Outbox| {
                    let sde = sde.clone();
                    Box::pin(async move {
                        let preparation = prepare(tx, &sde, &[root], &[]).await?;
                        hand_over(tx, &sde, outbox, &[root], dest, source, &preparation, &[]).await
                    })
                },
                |value, outbox| {
                    let world = &world;
                    async move { dispatch_world(value, outbox, world) }
                },
            ).await;
            assert!(result.is_ok(), "handover failed");
            let (event, args) = receiver.try_recv().expect("missing item notification");
            assert_eq!(event, "OnItemChange");
            let item = load_items(&test_db, &sde, ItemQuery::Id(root)).await.unwrap().pop().unwrap();
            assert_eq!(item.item_id, root);
            assert_eq!(item.owner_id, pilot.character);
            assert_eq!(item.location_id, destination);
            assert_eq!(item.flag_id, FLAG_HANGAR);
            assert_eq!(args, vec![item.row(), Value::Dict(vec![
                (Value::Int(IX_LOCATION_ID), Value::Int(source)),
                (Value::Int(IX_OWNER_ID), Value::Int(pilot.character)),
                (Value::Int(IX_FLAG), Value::Int(i64::from(FLAG_HANGAR))),
            ]), Value::None]);
            assert!(receiver.try_recv().is_err(), "duplicate item notification");
        }
        world.unregister_pilot_generation(pilot.character, None);
    }).await;
    fixture::cleanup(&db, pilot).await.unwrap();
    assert_eq!(fixture::remaining(&db, pilot).await.unwrap(), 0);
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn inactive_retained_office_denies_source_and_returns_to_station_deliveries() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let station = sde.map.stations().next().unwrap().station_id;
    let corp = fixture::create_corporation(&db, actor.character, station).await.unwrap();
    fixture::assign_corporation(&db, actor.character, corp, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
    let office = transfers::insert(&db, infinity_core::consts::TYPE_OFFICE, corp, station, 0, 1).await.unwrap();
    crate::dao::offices::insert(&db, office, corp, station, crate::client::filetime_now() + infinity_core::consts::FILETIME_DAY, 0).await.unwrap();
    let contract = Contract {
        id: 0, kind: 0, issuer: actor.character, issuer_corp: corp, for_corp: true,
        issuer_wallet_key: 0, acceptor_wallet_key: 0, issuer_hangar_flag: FLAG_CORP_SAG1,
        issuer_source_location: office, assignee: 0, acceptor: 0, start: station, end: 0,
        price: 0.0, reward: 0.0, collateral: 0.0, paid_deposit: 0.0, auction_tax_rate: 0.0,
        auction_tax_maximum: 0.0, title: String::new(), description: String::new(), volume: 0.0,
        num_days: 0, status: 0, issued: 0, expires: 0, accepted: 0, completed: 0, package: None,
    };
    assert_eq!(issuer_return(&db, &contract).await.unwrap().location, office);
    db.execute("UPDATE game.offices SET lease_state=1 WHERE office_id=$1", &[&office]).await.unwrap();
    assert!(issuer_source(&db, actor.character, corp, true, station, FLAG_CORP_SAG1).await.is_err());
    let destination = issuer_return(&db, &contract).await.unwrap();
    assert_eq!((destination.owner, destination.location, destination.flag), (corp, station, FLAG_CORP_DELIVERIES));
    assert!(crate::dao::offices::identity(&db, corp, station).await.unwrap().is_some());
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}
