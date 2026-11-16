use super::*;

#[test]
fn selection_rejects_bad_ids_and_deduplicates_members() {
    let group = infinity_core::consts::RECOVERY_ID_FIRST;
    let member = group + 1;
    let kwargs = Value::Dict(vec![]);
    let args = [Value::Dict(vec![(Value::Int(group), Value::List(vec![Value::Int(member), Value::Int(member)]))])];
    let call = Call { service: "repairableShipMgr", method: "CallAbortRequested", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let parsed = requested_groups(&call).unwrap();
    assert_eq!(parsed[&RecoveryGroupId::new(group).unwrap()], vec![RecoveryItemId::new(member).unwrap()]);
    let args = [Value::Dict(vec![(Value::Int(group), Value::List(vec![Value::Int(1)]))])];
    let invalid = Call { args: &args, ..call };
    assert!(requested_groups(&invalid).is_err());
}


#[test]
fn withdrawal_window_rejects_ready_after_expiry() {
    assert!(withdrawal_window(Some(10), Some(20), 9).is_err());
    assert!(withdrawal_window(Some(10), Some(20), 10).is_ok());
    assert!(withdrawal_window(Some(10), Some(20), 19).is_ok());
    assert!(withdrawal_window(Some(10), Some(20), 20).is_err());
}

#[test]
fn delivery_cost_uses_client_sent_service_life() {
    let max_life = 100.0;
    let cost = delivery_item_cost(
        max_life - 20.123456789, max_life - 30.123456789, max_life, 1e8, 1, 0.2,
    );
    assert_eq!(cost.trunc(), 1_999_999.0);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn active_member_with_missing_custody_is_not_terminalized() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), 0).await;
    let sde = client.sde.clone();
    let group_id = RecoveryGroupId::new(infinity_core::consts::RECOVERY_ID_FIRST).unwrap();
    let item_id = ItemId::new(i64::MAX).unwrap();
    let source = dao::SourceShape { item: item_id, owner: 1, type_id: 1, location: 1, flag: 0,
        quantity: 1, singleton: true, x: None, y: None, z: None, brain_slot: None, jump_clone: None };
    let group = Group { id: group_id, beneficiary: 1, kind: dao::SourceKind::PilotedShip, shape: source,
        system: 1, occurred_at: 1, position: [None; 3], loss_id: None, destination_station: 1, payment_expires_at: 2 };
    let member = RecoveryItem { id: RecoveryItemId::new(infinity_core::consts::RECOVERY_ID_FIRST + 1).unwrap(),
        group: group_id, item: item_id, original_owner: 1, type_id: 1, flag: 0, quantity: 1, singleton: true,
        last_service_life: 1.0, repair_completed_at: 1,
        delivery_completed_at: None, withdraw_expires_at: None, status: Status::AwaitingPayment,
        withdrawn_item: None };
    let result = db.transaction(|tx, _: &mut ()| {
        let sde = sde.clone();
        let group = group.clone();
        let member = member.clone();
        Box::pin(async move {
            discard_members(tx, &sde, &group, std::slice::from_ref(&member), &[Status::AwaitingPayment], Status::Abandoned, 1).await
        })
    }, |value, ()| async move { Ok(value) }).await;
    assert!(result.is_err());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn purchase_withdrawal_replay_and_expiry_are_atomic() {
    use crate::dao::items::parent_locks;
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), owner.character).await;
    let (sender, mut notifications) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender;
    let type_id = client.sde.types.iter().find(|ty| ty.base_price.is_some_and(|price| price >= 1_000_000.0 && price < 1_000_000_000.0)).unwrap().type_id;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let now = filetime_now();
    let life = client.sde.repairable_ship.max_life() / 2.0;
    db.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&owner.character]).await.unwrap();
    let (group_id, member_id, item_id, other_member, other_item) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let raw: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,4,1,true) RETURNING item_id", &[&type_id, &owner.character, &station]).await?.get("item_id");
        let raw_other: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,4,1,true) RETURNING item_id", &[&type_id, &owner.character, &station]).await?.get("item_id");
        let other_item = ItemId::new(raw_other).unwrap();
        let item = ItemId::new(raw).unwrap();
        let shape = custody::capture_many(tx, &[item]).await?.remove(0);
        let group = dao::insert_group(tx, &dao::NewGroup { loss: dao::LossSource { kind: dao::SourceKind::UnpilotedShip,
            beneficiary: owner.character, shape: shape.clone(), system: 1, lethal_at: now, sampled_position: [0.0; 3] },
            loss_id: None, destination_station: station, payment_expires_at: now + FILETIME_DAY }).await?;
        let locks = parent_locks::lock(tx, &[], &[LockRequest { item_id: raw, strength: LockStrength::Update }, LockRequest { item_id: raw_other, strength: LockStrength::Update }]).await?;
        let other_shape = custody::capture_many(tx, &[other_item]).await?.remove(0);
        assert!(custody::move_to_custody(tx, &locks, group, &[shape, other_shape]).await?);
        let member = dao::insert_items(tx, &[dao::NewItem { group, item, original_owner: owner.character,
            type_id, flag: 4, quantity: 1, singleton: true, last_service_life: life, repair_completed_at: now }]).await?[0];
        let other_member = dao::insert_items(tx, &[dao::NewItem { group, item: other_item, original_owner: owner.character,
            type_id, flag: 4, quantity: 1, singleton: true, last_service_life: life, repair_completed_at: now }]).await?[0];
        Ok::<_, crate::db::Error>((group, member, item, other_member, other_item))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let cleanup_db = db.clone();
    let outcome = tokio::spawn(async move {
        let identity = crate::services::identity::Identity::of_client(&client);
        let invalid = RecoveryItemId::new(infinity_core::consts::RECOVERY_ID_LAST).unwrap();
        let mixed = BTreeMap::from([(group_id, vec![member_id, other_member, invalid])]);
        assert!(purchase_for(identity, &db, &client.world, &client.sde, &client.notify_tx, mixed).await.is_err());
        assert_eq!(dao::get_member(&db, member_id).await.unwrap().unwrap().status, Status::AwaitingPayment);
        let before: i64 = db.query_one("SELECT count(*) AS n FROM game.wallet_journal WHERE owner_id1=$1 AND entry_type_id=$2", &[&owner.character, &REF_REHABILITATION_COSTS]).await.unwrap().get("n");
        assert_eq!(before, 0);
        let requested = BTreeMap::from([(group_id, vec![member_id, other_member])]);
        assert!(purchase_for(identity, &db, &client.world, &client.sde, &client.notify_tx, requested.clone()).await.is_err());
        assert_eq!(dao::get_member(&db, member_id).await.unwrap().unwrap().status, Status::AwaitingPayment);
        db.execute("UPDATE game.characters SET balance=1000000000000000 WHERE character_id=$1", &[&owner.character]).await.unwrap();
        purchase_for(identity, &db, &client.world, &client.sde, &client.notify_tx, requested.clone()).await.unwrap();
        let first = dao::get_member(&db, member_id).await.unwrap().unwrap();
        let first_receipt = db.query_one("SELECT paid_at, paid_amount::text AS paid_amount FROM game.repairable_recovery_items WHERE member_id = $1", &[&member_id.get()]).await.unwrap();
        let count: i64 = db.query_one("SELECT count(*) AS n FROM game.wallet_journal WHERE owner_id1=$1 AND entry_type_id=$2", &[&owner.character, &REF_REHABILITATION_COSTS]).await.unwrap().get("n");
        assert_eq!(count, 1);
        purchase_for(identity, &db, &client.world, &client.sde, &client.notify_tx, requested.clone()).await.unwrap();
        let again = dao::get_member(&db, member_id).await.unwrap().unwrap();
        let again_receipt = db.query_one("SELECT paid_at, paid_amount::text AS paid_amount FROM game.repairable_recovery_items WHERE member_id = $1", &[&member_id.get()]).await.unwrap();
        assert_eq!((first_receipt.get::<_, Option<i64>>("paid_at"), first.delivery_completed_at, first.withdraw_expires_at, first_receipt.get::<_, Option<String>>("paid_amount")),
            (again_receipt.get::<_, Option<i64>>("paid_at"), again.delivery_completed_at, again.withdraw_expires_at, again_receipt.get::<_, Option<String>>("paid_amount")));
        let after: i64 = db.query_one("SELECT count(*) AS n FROM game.wallet_journal WHERE owner_id1=$1 AND entry_type_id=$2", &[&owner.character, &REF_REHABILITATION_COSTS]).await.unwrap().get("n");
        assert_eq!(after, count);
        while notifications.try_recv().is_ok() {}
        let delivered = filetime_now() - 2;
        let expires = delivered + FILETIME_DAY;
        db.execute("UPDATE game.repairable_recovery_items SET repair_completed_at=$2,paid_at=$2,delivery_completed_at=$3,withdraw_expires_at=$4 WHERE member_id=$1",
            &[&other_member.get(), &(delivered - 1), &delivered, &expires]).await.unwrap();
        let picked = BTreeMap::from([(group_id, vec![other_member])]);
        withdraw_for(identity, &db, &client.world, &client.sde, &client.notify_tx, picked.clone()).await.unwrap();
        let created = dao::get_member(&db, other_member).await.unwrap().unwrap().withdrawn_item.unwrap();
        assert_ne!(created, other_item);
        assert_eq!(notifications.try_recv().unwrap().0, "OnItemChange");
        assert_eq!(notifications.try_recv().unwrap().0, "OnItemsWithdrawn");
        withdraw_for(identity, &db, &client.world, &client.sde, &client.notify_tx, picked).await.unwrap();
        assert_eq!(notifications.try_recv().unwrap().0, "OnItemsWithdrawn");
        assert!(notifications.try_recv().is_err());
        assert_eq!(dao::get_member(&db, other_member).await.unwrap().unwrap().withdrawn_item, Some(created));
        assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&created.get(), &owner.character]).await.unwrap().is_some());
        let requested = BTreeMap::from([(group_id, vec![member_id])]);
        let deadline = filetime_now() - 1;
        db.execute("UPDATE game.repairable_recovery_items SET repair_completed_at=$2,paid_at=$2,delivery_completed_at=$3,withdraw_expires_at=$4 WHERE member_id=$1",
            &[&member_id.get(), &(deadline - 2), &(deadline - 1), &deadline]).await.unwrap();
        let (withdrawal, expiry) = tokio::join!(
            withdraw_for(identity, &db, &client.world, &client.sde, &client.notify_tx, requested),
            deadlines::handle(db.clone(), client.world.clone(), deadlines::withdraw_job(member_id, deadline)),
        );
        assert!(withdrawal.is_err());
        expiry.unwrap();
        assert_eq!(dao::get_member(&db, member_id).await.unwrap().unwrap().status, Status::Expired);
    }).await;
    let db = cleanup_db;
    db.execute("DELETE FROM game.jobs WHERE job_id=ANY($1)", &[&[deadlines::job_id(deadlines::PAYMENT, member_id), deadlines::job_id(deadlines::DELIVERY, member_id), deadlines::job_id(deadlines::WITHDRAW, member_id), deadlines::job_id(deadlines::PAYMENT, other_member), deadlines::job_id(deadlines::DELIVERY, other_member), deadlines::job_id(deadlines::WITHDRAW, other_member)].as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=ANY($1)", &[&[item_id.get(), other_item.get()].as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.repairable_recovery_groups WHERE group_id=$1", &[&group_id.get()]).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
    outcome.unwrap();
}