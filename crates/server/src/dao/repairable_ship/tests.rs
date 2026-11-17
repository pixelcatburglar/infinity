//! Recovery identity bounds, status decoding and the guarded recovery
//! lifecycle on a dedicated PostgreSQL database.

use super::*;

#[test]
fn recovery_id_bands_separate_physical_and_business_identities() {
    for id in [RECOVERY_ID_FIRST, RECOVERY_ID_FIRST + 1, RECOVERY_ID_LAST] {
        assert!(RecoveryGroupId::new(id).is_some() && RecoveryItemId::new(id).is_some());
        assert!(ItemId::new(id).is_none());
    }
    for id in [0, -1, RECOVERY_ID_FIRST - 1, RECOVERY_ID_LAST + 1] {
        assert!(RecoveryGroupId::new(id).is_none() && RecoveryItemId::new(id).is_none());
    }
    assert!(ItemId::new(RECOVERY_ID_FIRST - 1).is_some() && ItemId::new(RECOVERY_ID_LAST + 1).is_some() && ItemId::new(0).is_none());
}
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn guarded_recovery_lifecycle_round_trip() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let source = LossSource {
        kind: SourceKind::PilotedShip, beneficiary: 90000001, system: 30000142, lethal_at: 100,
        sampled_position: [1.0, 2.0, 3.0],
        shape: SourceShape {
            item: ItemId::new(1_000_000_000_001).unwrap(), owner: 90000001, type_id: 587, location: 30000142,
            flag: 0, quantity: 1, singleton: true,
            x: None, y: None, z: None, brain_slot: None, jump_clone: None,
        },
    };
    let group = insert_group(&tx, &NewGroup {
        loss: source.clone(), loss_id: Some(42), destination_station: 60003760, payment_expires_at: 200,
    }).await.unwrap();
    let save = tx.savepoint("duplicate_source").await.unwrap();
    assert!(insert_group(&save, &NewGroup {
        loss: source.clone(), loss_id: Some(43), destination_station: 60003760, payment_expires_at: 200,
    }).await.is_err());
    save.rollback().await.unwrap();
    let member = insert_items(&tx, &[NewItem {
        group, item: ItemId::new(1_000_000_000_002).unwrap(), original_owner: 90000001, type_id: 11400,
        flag: 27, quantity: 1, singleton: true, last_service_life: 75.5, repair_completed_at: 150,
    }]).await.unwrap()[0];
    let save = tx.savepoint("zero_quantity").await.unwrap();
    assert!(insert_items(&save, &[NewItem {
        group, item: ItemId::new(1_000_000_000_004).unwrap(), original_owner: 90000001, type_id: 11400,
        flag: 27, quantity: 0, singleton: true, last_service_life: 75.5, repair_completed_at: 150,
    }]).await.is_err());
    save.rollback().await.unwrap();
    let owned = list_owned(&tx, 90000001).await.unwrap();
    assert_eq!((owned.len(), owned[0].items.len(), owned[0].items[0].status), (1, 1, Status::AwaitingPayment));
    let amount = IskAmount::from_whole_isk(5).unwrap();
    assert_eq!(start_delivery(&tx, 160, 1, 1, &[Payment { member, amount }]).await.unwrap(), vec![member]);
    assert!(start_delivery(&tx, 160, 1, 1, &[Payment { member, amount }]).await.unwrap().is_empty());
    assert!(mark_ready(&tx, &[member], 160).await.unwrap().is_empty());
    assert_eq!(mark_ready(&tx, &[member], 161).await.unwrap(), vec![member]);
    // The schema invariant rejects a withdrawn transition without the new inventory mapping.
    let save = tx.savepoint("unmapped_withdraw").await.unwrap();
    assert!(terminalize(&save, &[(member, None)], &[Status::Ready], Status::Withdrawn, 300).await.is_err());
    save.rollback().await.unwrap();
    assert_eq!(
        terminalize(&tx, &[(member, Some(ItemId::new(1_000_000_000_003).unwrap()))], &[Status::Ready], Status::Withdrawn, 300).await.unwrap(),
        vec![member],
    );
    assert!(terminalize(&tx, &[(member, None)], &[Status::Ready], Status::Withdrawn, 300).await.unwrap().is_empty());
    assert!(list_owned(&tx, 90000001).await.unwrap().is_empty());
    let proof = source_proof(&tx, &source).await.unwrap();
    assert_eq!(proof.group.map(|group| group.id), Some(group));
    assert_eq!(proof.members.len(), 1);
    assert_eq!((proof.members[0].status, proof.members[0].withdrawn_item),
        (Status::Withdrawn, Some(ItemId::new(1_000_000_000_003).unwrap())));
    let receipt = tx.query_one("SELECT paid_amount::text AS paid_amount FROM game.repairable_recovery_items WHERE member_id = $1", &[&member.get()]).await.unwrap();
    let paid_amount: Option<String> = receipt.get("paid_amount");
    assert_eq!(paid_amount.map(|text| IskAmount::parse_decimal(&text).unwrap()), Some(amount));
    tx.rollback().await.unwrap();
}
