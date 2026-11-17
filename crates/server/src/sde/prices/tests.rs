use super::*;

fn fixture() -> Prices {
    Prices::from_rows(vec![
        Price { type_id: 30, average: f64::NAN, adjusted: 3.0 },
        Price { type_id: 10, average: 5000.0, adjusted: 4000.0 },
        Price { type_id: 20, average: 1000.0, adjusted: 900.0 },
        Price { type_id: 40, average: 0.0, adjusted: 0.0 },
        Price { type_id: 50, average: 900.0, adjusted: 800.0 },
    ])
}

#[test]
fn lookup_iteration_and_missing_keys_preserve_row_order() {
    let prices = fixture();
    assert!(prices.get(99).is_none());
    assert_eq!(prices.get(10).unwrap().adjusted, 4000.0);
    assert_eq!(prices.get(40).unwrap().average, 0.0);
    assert!(prices.get(30).unwrap().average.is_nan());
    assert_eq!(prices.iter().map(|p| p.type_id).collect::<Vec<_>>(), [30, 10, 20, 40, 50]);
}

#[test]
fn average_between_finite_bounds_and_nan_maximum() {
    let prices = fixture();
    let ids = |min: f64, max: f64| prices.average_between(min, max).map(|p| p.type_id).collect::<Vec<_>>();
    assert_eq!(ids(LOOT_MINIMUM_AVERAGE, 5000.0), [10, 20]);
    assert_eq!(ids(LOOT_MINIMUM_AVERAGE, f64::NAN), [30, 10, 20]);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn startup_upsert_retains_unlisted_prices() {
    let (mut client, connection) = crate::config::tests::test_database().to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let (legacy, upserted) = (-420_000_001_i32, -420_000_002_i32);
    tx.execute("INSERT INTO game.retail_prices (type_id, average, adjusted) VALUES ($1, $2, $3)", &[&legacy, &-5.5_f64, &-2.25_f64]).await.unwrap();
    crate::dao::market::startup::retail_prices(&tx, &[upserted], &[1.0], &[2.0]).await.unwrap();
    crate::dao::market::startup::retail_prices(&tx, &[upserted], &[123.5], &[67.25]).await.unwrap();
    let prices = Prices::from_rows(crate::dao::sde::prices::load(&tx).await.unwrap());
    // The unlisted legacy row survives the upsert; both negative fixture IDs are
    // absent from the SDE yet returned, so the loader applies no type filter.
    assert_eq!(prices.get(legacy).map(|p| (p.average, p.adjusted)), Some((-5.5, -2.25)));
    assert_eq!(prices.get(upserted).map(|p| (p.average, p.adjusted)), Some((123.5, 67.25)));
    tx.rollback().await.unwrap();
    drop(client);
    driver.await.unwrap();
}
