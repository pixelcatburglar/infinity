//! Real transaction regressions for contract terminal transitions and auction settlement.
use super::super::*;
use super::corporate_fixture as f;

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn guarded_transitions_and_auction_errors_preserve_atomic_state() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let (start, system, station_type, end) = {
        let mut stations = sde.map.stations().filter(|s| s.type_id.is_some() && s.solar_system_id.is_some());
        let station = stations.next().unwrap();
        let end = stations.find(|s| s.station_id != station.station_id).unwrap().station_id;
        (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap(), end)
    };
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
        let (seller_tx, mut seller_rx) = tokio::sync::mpsc::unbounded_channel();
        let (buyer_tx, mut buyer_rx) = tokio::sync::mpsc::unbounded_channel();
        let (owner_tx, mut owner_rx) = tokio::sync::mpsc::unbounded_channel();
        seller.world.register_pilot(buyer.actor.character, buyer_tx);
        seller.world.register_pilot(issuer.actor.character, owner_tx);
        courier.world.register_pilot(issuer.actor.character, seller_tx);

        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let id = f::id(create(&seller, &f::payload(TYPE_COURIER, start, end, f::DELIVERIES, root, 0.0, 20.0, 50.0)).await);
        assert!(accept(&courier, id, true).await.is_ok());
        while owner_rx.try_recv().is_ok() {}
        let before_issuer = f::fingerprint(&db, issuer).await;
        let before_buyer = f::fingerprint(&db, buyer).await;
        assert!(matches!(complete(&seller, id, STATUS_FAILED).await, Err(CallError::User { .. })));
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        assert!(buyer_rx.try_recv().is_err());
        assert!(owner_rx.try_recv().is_err());
        db.execute("UPDATE game.contracts SET date_accepted=0 WHERE contract_id=$1", &[&id]).await.unwrap();
        assert!(complete(&seller, id, STATUS_FAILED).await.is_ok());
        let events: Vec<_> = std::iter::from_fn(|| buyer_rx.try_recv().ok()).collect();
        assert_eq!(events.iter().filter(|(name, _)| name == "OnContractFailed").count(), 1);
        let row = db.query_one("SELECT status FROM game.contracts WHERE contract_id=$1", &[&id]).await.unwrap();
        assert_eq!(row.get::<_, i32>("status"), STATUS_FAILED);
        let row = db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (buyer.id, start, f::DELIVERIES));
        let balance = crate::services::wallets::corp_balance(&db, issuer.id, ACCOUNT_CASH + 1).await.unwrap();
        assert_eq!(balance, IskAmount::from_cents(99_005_000).unwrap());
        let before_issuer = f::fingerprint(&db, issuer).await;
        let before_buyer = f::fingerprint(&db, buyer).await;
        assert!(matches!(complete(&seller, id, STATUS_FAILED).await, Err(CallError::User { .. })));
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        assert!(buyer_rx.try_recv().is_err());

        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let finished = f::id(create(&seller, &f::payload(TYPE_COURIER, start, end, f::DELIVERIES, root, 0.0, 20.0, 50.0)).await);
        assert!(accept(&courier, finished, true).await.is_ok());
        let package: i64 = db.query_one("SELECT package_id FROM game.contracts WHERE contract_id=$1", &[&finished]).await.unwrap().get("package_id");
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&package, &end]).await.unwrap();
        assert!(complete(&courier, finished, STATUS_FINISHED).await.is_ok());
        let events: Vec<_> = std::iter::from_fn(|| seller_rx.try_recv().ok()).collect();
        assert_eq!(events.iter().filter(|(name, _)| name == "OnContractCompleted").count(), 1);
        let before_issuer = f::fingerprint(&db, issuer).await;
        let before_buyer = f::fingerprint(&db, buyer).await;
        assert!(matches!(complete(&courier, finished, STATUS_FINISHED).await, Err(CallError::User { .. })));
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        assert!(seller_rx.try_recv().is_err());

        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let deleted = f::id(create(&seller, &f::payload(TYPE_ITEM_EXCHANGE, start, 0, f::DELIVERIES, root, 0.0, 0.0, 0.0)).await);
        { let _guard = lock().await; assert!(delete_one(&seller, deleted).await.is_ok()); }
        while buyer_rx.try_recv().is_ok() {}
        let before_issuer = f::fingerprint(&db, issuer).await;
        let before_buyer = f::fingerprint(&db, buyer).await;
        while owner_rx.try_recv().is_ok() {}
        { let _guard = lock().await; assert!(matches!(delete_one(&seller, deleted).await, Err(CallError::User { .. }))); }
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        assert!(buyer_rx.try_recv().is_err());

        assert!(owner_rx.try_recv().is_err());
        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let rejected = f::id(create(&seller, &f::payload(TYPE_ITEM_EXCHANGE, start, 0, f::DELIVERIES, root, 0.0, 0.0, 0.0)).await);
        db.execute("UPDATE game.contracts SET assignee_id=$2 WHERE contract_id=$1", &[&rejected, &buyer.id]).await.unwrap();
        assert!(complete(&courier, rejected, STATUS_REJECTED).await.is_ok());
        let events: Vec<_> = std::iter::from_fn(|| seller_rx.try_recv().ok()).collect();
        assert_eq!(events.iter().filter(|(name, _)| name == "OnContractRejected").count(), 1);
        let before_issuer = f::fingerprint(&db, issuer).await;
        let before_buyer = f::fingerprint(&db, buyer).await;
        assert!(matches!(complete(&courier, rejected, STATUS_REJECTED).await, Err(CallError::User { .. })));
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        assert!(seller_rx.try_recv().is_err());

        // The rejected contract's real escrow handover and wallet debit precede
        // the stale status guard, so both must roll back on its refusal.
        while owner_rx.try_recv().is_ok() {}
        let _guard = lock().await;
        let world = &*seller.world;
        let result: Result<(), CallError> = db.transaction(
            |tx, outbox: &mut Outbox| {
                let sde = Arc::clone(&sde);
                Box::pin(async move {
                    wallet_ownership::debit(tx, outbox, wallet_ownership::FinancialOwner { id: buyer.id, wallet_key: ACCOUNT_CASH + 1 }, IskAmount::from_cents(1_000).unwrap()).await?;
                    let preparation = custody::prepare(tx, &sde, &[root], &[]).await?;
                    custody::hand_over(tx, &sde, outbox, &[root], custody::receiving(buyer.id, start), -rejected, &preparation, &[]).await?;
                    outbox.notify_pilot(buyer.actor.character, "OnContractFailed", vec![]);
                    assert!(!shared::set_status(tx, rejected, STATUS_OUTSTANDING, STATUS_FINISHED, 1, buyer.id, ACCOUNT_CASH + 1).await.map_err(db_err)?);
                    let mut stale = contract(tx, rejected).await?;
                    stale.status = STATUS_OUTSTANDING;
                    let completed = stale.completed;
                    let result = set_status(tx, &mut stale, STATUS_FINISHED, 1).await;
                    assert!(matches!(&result, Err(CallError::User { msg, .. }) if msg == messages::MSG_CON_CONTRACT_NOT_OUTSTANDING));
                    assert_eq!((stale.status, stale.completed), (STATUS_OUTSTANDING, completed));
                    result
                })
            },
            |value, outbox| async move { dispatch_world(value, outbox, world) },
        ).await;
        assert!(matches!(result, Err(CallError::User { .. })));
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        assert!(buyer_rx.try_recv().is_err());
        assert!(owner_rx.try_recv().is_err());
        drop(_guard);

        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let auction = f::id(create(&seller, &f::payload(TYPE_AUCTION, start, 0, f::DELIVERIES, root, 10.0, 0.0, 0.0)).await);
        assert!(place_bid(&courier, auction, 10.0, true).await.is_ok());
        db.execute("UPDATE game.contracts SET date_expired=0 WHERE contract_id=$1", &[&auction]).await.unwrap();
        let now = filetime_now();
        let _guard = lock().await;
        let selected = contract_dao::expired_auctions_with_bids(&db, TYPE_AUCTION, STATUS_OUTSTANDING, now).await.unwrap();
        assert!(selected.iter().any(|k| k.id == auction));
        assert!(shared::set_status(&db, auction, STATUS_OUTSTANDING, STATUS_FINISHED, now, buyer.id, ACCOUNT_CASH + 1).await.unwrap());
        while buyer_rx.try_recv().is_ok() {}
        while owner_rx.try_recv().is_ok() {}
        let before_issuer = f::fingerprint(&db, issuer).await;
        let before_buyer = f::fingerprint(&db, buyer).await;
        assert!(matches!(settle_expired_candidate(&db, &sde, &seller.world, auction, now).await, Ok(None)));
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        assert!(buyer_rx.try_recv().is_err());
        assert!(owner_rx.try_recv().is_err());
        drop(_guard);

        let root = transfers::insert(&db, kind, issuer.id, start, f::DELIVERIES, 1).await.unwrap();
        let broken = f::id(create(&seller, &f::payload(TYPE_AUCTION, start, 0, f::DELIVERIES, root, 10.0, 0.0, 0.0)).await);
        assert!(place_bid(&courier, broken, 10.0, true).await.is_ok());
        db.execute("UPDATE game.contracts SET date_expired=0 WHERE contract_id=$1", &[&broken]).await.unwrap();
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&root, &start]).await.unwrap();
        while owner_rx.try_recv().is_ok() {}
        let before_issuer = f::fingerprint(&db, issuer).await;
        let before_buyer = f::fingerprint(&db, buyer).await;
        let _guard = lock().await;
        assert!(matches!(settle_expired_candidate(&db, &sde, &seller.world, broken, filetime_now()).await, Err(CallError::User { msg, .. }) if msg == messages::MSG_CUSTOM_NOTIFY));
        drop(_guard);
        let job = crate::scheduler::Job { id: crate::scheduler::CONTRACTS.into(), kind: crate::scheduler::CONTRACTS.into(), due_at: 0, payload: String::new() };
        let error = crate::services::contracts::settle_auctions(db.clone(), Arc::clone(&seller.world), job).await.unwrap_err();
        assert_eq!(error.to_string(), messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
        assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
        assert!(buyer_rx.try_recv().is_err());
        assert!(owner_rx.try_recv().is_err());
        seller.world.unregister_pilot_generation(buyer.actor.character, None);
        seller.world.unregister_pilot_generation(issuer.actor.character, None);
        courier.world.unregister_pilot_generation(issuer.actor.character, None);
    }).await;
    f::cleanup(&db, issuer).await;
    f::cleanup(&db, buyer).await;
    result.unwrap();
}
