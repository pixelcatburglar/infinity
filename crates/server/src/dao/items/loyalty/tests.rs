use super::*;
use crate::{dao::items::quantities::{self, QuantityChange}, db::Db};
use std::collections::BTreeMap;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn requirements_only_include_ordered_hangar_stacks_and_guard_changes() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&cfg).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let owner = -9001_i64;
    let station = -9002_i64;
    let insert = "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, $6) RETURNING item_id";
    let first: i64 = tx.query_one(insert, &[&11_i32, &owner, &station, &FLAG_HANGAR, &5_i64, &false]).await.unwrap().get("item_id");
    let second: i64 = tx.query_one(insert, &[&11_i32, &owner, &station, &FLAG_HANGAR, &7_i64, &false]).await.unwrap().get("item_id");
    for (o, l, f, singleton, ty) in [(owner, station, FLAG_HANGAR, true, 11_i32), (owner, station, FLAG_HANGAR + 1, false, 11), (owner - 1, station, FLAG_HANGAR, false, 11), (owner, station - 1, FLAG_HANGAR, false, 11), (owner, station, FLAG_HANGAR, false, 12)] {
        tx.query_one(insert, &[&ty, &o, &l, &f, &1_i64, &singleton]).await.unwrap();
    }
    let stacks = requirement_stacks(&tx, &sde, owner, station, &[11]).await.unwrap();
    assert_eq!(stacks.iter().map(|item| item.item_id).collect::<Vec<_>>(), vec![first, second]);
    assert!(requirement_stacks(&tx, &sde, owner, station, &[]).await.unwrap().is_empty());
    let changes = BTreeMap::from([(first, QuantityChange::new(&stacks[0], Some(2))), (second, QuantityChange::new(&stacks[1], None))]);
    let inner = tx.transaction().await.unwrap();
    assert!(quantities::apply(&inner, &changes).await.unwrap());
    assert!(!quantities::apply(&inner, &changes).await.unwrap());
    let remaining = requirement_stacks(&inner, &sde, owner, station, &[11]).await.unwrap();
    assert_eq!(remaining.iter().map(|item| (item.item_id, item.quantity)).collect::<Vec<_>>(), vec![(first, 2)]);
    inner.rollback().await.unwrap();
    let restored = requirement_stacks(&tx, &sde, owner, station, &[11]).await.unwrap();
    assert_eq!(restored.iter().map(|item| (item.item_id, item.quantity)).collect::<Vec<_>>(), vec![(first, 5), (second, 7)]);
    tx.rollback().await.unwrap();
}
