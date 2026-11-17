#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn catalogue_snapshot_preserves_order_and_isolation() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let empty: bool = tx.query_one("SELECT NOT EXISTS (SELECT 1 FROM game.lp_offers) AS empty", &[]).await.unwrap().get("empty");
    assert_eq!(crate::sde::lp_offers::LpOffers::load(&tx).await.unwrap().is_empty(), empty);
    let mut issuers = Vec::new();
    while issuers.len() < 2 {
        let id: i64 = tx.query_one("SELECT nextval('game.corporations_corporation_id_seq') AS id", &[]).await.unwrap().get("id");
        if tx.query_opt("SELECT 1 FROM game.lp_offers WHERE corporation_id = $1 LIMIT 1", &[&id]).await.unwrap().is_none() {
            issuers.push(id);
        }
    }
    let issuer = issuers[0];
    let other_issuer = issuers[1];
    tx.execute("INSERT INTO game.lp_offers (corporation_id, offer_id, type_id, quantity, lp_cost, isk_cost, ak_cost, required_items) VALUES ($1, 2, 7, 3, 4, 5, 6, '[{\"type_id\":9,\"quantity\":2},{\"type_id\":8,\"quantity\":1},{\"type_id\":9,\"quantity\":3}]'), ($1, 1, 7, 1, 0, 0, 0, '[]'), ($2, 1, 8, 1, 0, 0, 0, '[]')", &[&issuer, &other_issuer]).await.unwrap();
    let offers = crate::sde::lp_offers::LpOffers::load(&tx).await.unwrap();
    assert!(!offers.is_empty());
    assert_eq!(offers.list(issuer).iter().map(|o| o.offer_id).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(offers.get(issuer, 2).unwrap(), &offers.list(issuer)[1]);
    assert_eq!(offers.get(issuer, 2).unwrap().required_items.iter().map(|r| (r.type_id, r.quantity)).collect::<Vec<_>>(), vec![(9, 2), (8, 1), (9, 3)]);
    assert!(offers.get(other_issuer, 2).is_none());
    tx.execute("UPDATE game.lp_offers SET required_items = '[{\"type_id\":9,\"quantity\":0}]' WHERE corporation_id = $1 AND offer_id = 2", &[&issuer]).await.unwrap();
    // The loaded snapshot is immutable; a fresh load reads the bad row and fails.
    assert_eq!(offers.get(issuer, 2).unwrap().required_items[0].quantity, 2);
    let Err(error) = crate::sde::lp_offers::LpOffers::load(&tx).await else { panic!("expected malformed required_items to fail") };
    let error = format!("{error:#}");
    assert!(error.contains(&format!("corporation_id={issuer}")) && error.contains("offer_id=2"), "{error}");
    tx.rollback().await.unwrap();
}
