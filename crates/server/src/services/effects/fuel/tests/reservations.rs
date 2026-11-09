//! Real open-trade ancestry checked against the fuel planner.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn open_trade_ancestry_reserves_direct_and_nested_fuel() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&cfg).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let fuel = sde.types.iter().find(|ty| ty.group_id == Some(GROUP_ICE_PRODUCT)).expect("ice product in SDE").type_id;
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let trade = trades::create(&tx, 1, -1, -2, 0).await.unwrap();
        let owner = -10_i64;
        let insert = "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,$6) RETURNING item_id";
        // A piloted hull cannot legally be traded: this deliberately synthetic
        // enclosing hull exercises the actual SQL ancestor chain.
        let ship: i64 = tx.query_one(insert, &[&fuel, &owner, &trade.id, &FLAG_CARGO, &1_i64, &true]).await.unwrap().get("item_id");
        let stack: i64 = tx.query_one(insert, &[&fuel, &owner, &ship, &FLAG_CARGO, &3_i64, &false]).await.unwrap().get("item_id");
        let flags = [FLAG_CARGO];
        for nested in [false, true] {
            if nested {
                let container: i64 = tx.query_one(insert, &[&fuel, &owner, &trade.id, &FLAG_CARGO, &1_i64, &true]).await.unwrap().get("item_id");
                tx.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&ship, &container]).await.unwrap();
            }
            let items = load_items(&tx, &sde, ItemQuery::LocationOwnerFlags(ship, owner, &flags)).await.unwrap();
            let reserved = trades::reserved(&tx, &[stack]).await.unwrap();
            assert_eq!(reserved, vec![stack]);
            assert!(plan(&BTreeMap::from([(fuel, 1)]), &items, &reserved, &sde, owner).is_err());
            assert_eq!(load_items(&tx, &sde, ItemQuery::Id(stack)).await.unwrap()[0].quantity, 3);
        }
    }).await;
    let cleanup = tx.rollback().await;
    drop(client);
    let driver_result = driver.await;
    if let Err(original) = result {
        if cleanup.is_err() || driver_result.is_err() { eprintln!("trade fixture cleanup failed"); }
        std::panic::resume_unwind(original);
    }
    cleanup.unwrap();
    driver_result.unwrap();
}
