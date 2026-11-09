//! Guarded fuel writes racing a concurrent inventory update.
use super::*;
use std::{sync::Arc, time::Duration};
use infinity_core::{consts::{CATEGORY_MODULE, CATEGORY_SHIP}};
use crate::{dao::items::dogma, db::Db, sde::Sde, space::World};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn concurrent_later_stack_change_rolls_back_prefix() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&cfg).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).expect("ship in SDE").type_id;
    let module_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)).expect("module in SDE").type_id;
    let fuel = sde.types.iter().find(|ty| ty.group_id == Some(GROUP_ICE_PRODUCT)).expect("ice product in SDE").type_id;
    let (account, owner): (i64, i64) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('fuel-race-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let owner = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'fuel-race-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        Ok::<_, crate::db::Error>((account, owner))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let mut task = None;
    let mut blocker_client = None;
    let mut driver = None;
    let result = crate::tests::catch_scenario(async {
        let insert = "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,$5,$6,true) RETURNING item_id";
        let ship: i64 = db.query_one(insert, &[&ship_type, &owner, &1_i64, &0_i32, &1_i64, &true]).await.unwrap().get("item_id");
        let flag = 27_i32;
        let module: i64 = db.query_one(insert, &[&module_type, &owner, &ship, &flag, &1_i64, &true]).await.unwrap().get("item_id");
        dogma::set_override(&db, module, ATTR_CONSUMPTION_TYPE, f64::from(fuel)).await.unwrap();
        dogma::set_override(&db, module, ATTR_CONSUMPTION_QUANTITY, 4.0).await.unwrap();
        let first: i64 = db.query_one(insert, &[&fuel, &owner, &ship, &FLAG_CARGO, &2_i64, &false]).await.unwrap().get("item_id");
        let second: i64 = db.query_one(insert, &[&fuel, &owner, &ship, &FLAG_CARGO, &3_i64, &false]).await.unwrap().get("item_id");
        let fit = Fit::load(&db, sde.clone(), owner, ship).await.unwrap();
        let world = World::new(db.clone(), sde.clone(), 1);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let cx = Ctx { db: db.clone(), world, notify: sender, char_id: owner, ship_id: ship, system_id: 1 };
        let (client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        driver = Some(tokio::spawn(async move { connection.await.unwrap(); }));
        blocker_client = Some(client);
        let client = blocker_client.as_mut().unwrap();
        let tx = client.transaction().await.unwrap();
        let pid: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&first]).await.unwrap();
        task = Some(tokio::spawn(async move {
            consume_cycle(&cx, &fit, &[Module { id: module, type_id: module_type, flag }]).await
        }));
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let waiting: bool = db.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) AND query LIKE '%game.items%' AND pid <> pg_backend_pid()) AS waiting", &[&pid]).await.unwrap().get("waiting");
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("fuel transaction did not block on the first stack");
        assert!(receiver.try_recv().is_err());
        tx.execute("UPDATE game.items SET quantity=quantity+1 WHERE item_id=$1", &[&second]).await.unwrap();
        tx.commit().await.unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(10), task.as_mut().unwrap()).await.expect("fuel transaction did not finish").unwrap();
        task = None;
        assert!(outcome.is_err());
        assert_eq!(load_items(&db, &sde, ItemQuery::Id(first)).await.unwrap()[0].quantity, 2);
        assert_eq!(load_items(&db, &sde, ItemQuery::Id(second)).await.unwrap()[0].quantity, 4);
        assert!(receiver.try_recv().is_err());
    }).await;
    if let Some(task) = task { task.abort(); let _ = task.await; }
    drop(blocker_client);
    if let Some(driver) = driver { let _ = driver.await; }
    let cleanup = crate::tests::catch_scenario(async {
        db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&owner]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&owner]).await.unwrap();
        db.execute("DELETE FROM game.characters WHERE character_id=$1 AND user_id=$2", &[&owner, &account]).await.unwrap();
        db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    }).await;
    if let Err(cleanup) = cleanup {
        if let Err(original) = result { eprintln!("fuel race fixture cleanup also panicked"); std::panic::resume_unwind(original); }
        std::panic::resume_unwind(cleanup);
    }
    if let Err(original) = result { std::panic::resume_unwind(original); }
}
