//! Real PostgreSQL regressions for batched durable condition checkpoints.
use super::*;
use infinity_core::{consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE, ATTR_SHIELD_CHARGE}};
use crate::{config::deployment::DatabaseConfig, db::{Db, Error}};

const LOCATION: i64 = 7_000_000_001;
const OTHER_LOCATION: i64 = 7_000_000_002;
const IDS: [i32; 3] = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];

async fn database() -> Db {
    crate::tests::init_config();
    Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap()
}

struct Fixture { db: Db, owner: i64, items: Vec<i64> }
impl Fixture {
    async fn new(db: Db, count: usize) -> Self {
        let (owner, items) = db.transaction::<(i64, Vec<i64>), (i64, Vec<i64>), Error, (), _, _, _>(move |tx, _| Box::pin(async move {
            let owner: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('condition_batch_test_' || nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES (1,$1,$2,1,1,true) RETURNING item_id", &[&owner, &LOCATION]).await?.get("item_id"));
            }
            Ok((owner, items))
        }), |value, ()| async move { Ok(value) }).await.unwrap();
        Self { db, owner, items }
    }
    async fn cleanup(self) {
        // Owner-scoped cleanup also covers an extra item inserted just before a panic.
        let attrs = self.db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&self.owner]).await;
        let items = self.db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&self.owner]).await;
        let account = self.db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&self.owner]).await;
        attrs.unwrap(); items.unwrap(); account.unwrap();
    }
}

async fn finish(fixture: Fixture, result: std::thread::Result<()>) {
    let cleanup = crate::tests::catch_scenario(fixture.cleanup()).await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
    if let Err(panic) = cleanup { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn empty_condition_batch_issues_no_sql_even_in_aborted_transaction() {
    let db = database().await;
    let result = db.transaction::<(), (), Error, (), _, _, _>(|tx, _| Box::pin(async move {
        let error: Error = tx.execute("SELECT condition_batch_test_missing_relation", &[]).await.unwrap_err().into();
        assert!(save_conditions(tx, LOCATION, &IDS, &[]).await.unwrap().is_empty());
        Err(error)
    }), |(), ()| async { Ok(()) }).await;
    assert!(result.is_err());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn condition_batch_filters_items_and_updates_existing_rows() {
    let db = database().await;
    let fixture = Fixture::new(db.clone(), 4).await;
    let [first, second, moved, stack] = fixture.items[..] else { unreachable!() };
    let result = crate::tests::catch_scenario(async {
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&moved, &OTHER_LOCATION]).await.unwrap();
        db.execute("UPDATE game.items SET singleton=false WHERE item_id=$1", &[&stack]).await.unwrap();
        // Allocate and delete a real ID to guarantee the missing row belongs to this fixture.
        let missing: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES (1,$1,$2,1,1,true) RETURNING item_id", &[&fixture.owner, &LOCATION]).await.unwrap().get("item_id");
        db.execute("DELETE FROM game.items WHERE item_id=$1", &[&missing]).await.unwrap();
        let rows = [(second,[20.0,30.0,40.0],200), (stack,[21.0,31.0,41.0],201), (missing,[22.0,32.0,42.0],202), (first,[10.0,11.0,12.0],100), (moved,[23.0,33.0,43.0],203)];
        assert_eq!(save_conditions(&db, LOCATION, &IDS, &rows).await.unwrap(), vec![first, second]);
        let attrs = db.query("SELECT item_id,attribute_id,value,updated_at FROM game.item_attributes WHERE item_id=ANY($1)", &[&fixture.items]).await.unwrap();
        assert_eq!(attrs.len(), 6);
        for (item, values, time) in [(first,[10.0,11.0,12.0],100_i64), (second,[20.0,30.0,40.0],200)] {
            for (id, value) in IDS.into_iter().zip(values) {
                let row = attrs.iter().find(|r| r.get::<_,i64>("item_id") == item && r.get::<_,i32>("attribute_id") == id).unwrap();
                assert_eq!(row.get::<_,f64>("value"), value);
                assert_eq!(row.get::<_,i64>("updated_at"), if id == ATTR_SHIELD_CHARGE { time } else { 0 });
            }
        }
        assert_eq!(save_conditions(&db, LOCATION, &IDS, &[(second,[25.0,35.0,45.0],250)]).await.unwrap(), vec![second]);
        let attrs = db.query("SELECT attribute_id,value,updated_at FROM game.item_attributes WHERE item_id=$1", &[&second]).await.unwrap();
        assert_eq!(attrs.len(), 3);
        for (id, value) in IDS.into_iter().zip([25.0,35.0,45.0]) {
            let row = attrs.iter().find(|r| r.get::<_,i32>("attribute_id") == id).unwrap();
            assert_eq!(row.get::<_,f64>("value"), value);
            assert_eq!(row.get::<_,i64>("updated_at"), if id == ATTR_SHIELD_CHARGE { 250 } else { 0 });
        }
    }).await;
    finish(fixture, result).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn condition_batch_rollback_restores_old_and_removes_new_rows() {
    let db = database().await;
    let fixture = Fixture::new(db.clone(), 2).await;
    let (existing, new_item) = (fixture.items[0], fixture.items[1]);
    let result = crate::tests::catch_scenario(async {
        db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value,updated_at) VALUES ($1,$2,1.0,11),($1,$3,2.0,0),($1,$4,3.0,0)", &[&existing,&IDS[0],&IDS[1],&IDS[2]]).await.unwrap();
        let result = db.transaction::<(), (), Error, (), _, _, _>(move |tx, _| Box::pin(async move {
            assert_eq!(save_conditions(tx, LOCATION, &IDS, &[(existing,[10.0,20.0,30.0],99),(new_item,[4.0,5.0,6.0],100)]).await.unwrap(), vec![existing,new_item]);
            Err(Error::from(tx.execute("SELECT condition_batch_test_rollback_error", &[]).await.unwrap_err()))
        }), |(), ()| async { Ok(()) }).await;
        assert!(result.is_err());
        let attrs = db.query("SELECT item_id,attribute_id,value,updated_at FROM game.item_attributes WHERE item_id=ANY($1)", &[&fixture.items]).await.unwrap();
        assert_eq!(attrs.len(), 3);
        for (id, value) in IDS.into_iter().zip([1.0,2.0,3.0]) {
            let row = attrs.iter().find(|r| r.get::<_,i64>("item_id")==existing && r.get::<_,i32>("attribute_id")==id).unwrap();
            assert_eq!(row.get::<_,f64>("value"), value);
            assert_eq!(row.get::<_,i64>("updated_at"), if id==ATTR_SHIELD_CHARGE {11} else {0});
        }
    }).await;
    finish(fixture, result).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn condition_batch_is_one_insert_statement() {
    let db = database().await;
    let fixture = Fixture::new(db.clone(), 2).await;
    let (a,b) = (fixture.items[0], fixture.items[1]);
    let result = crate::tests::catch_scenario(async {
        let result = db.transaction::<(), (), Error, (), _, _, _>(move |tx, _| Box::pin(async move {
            tx.batch_execute("CREATE TEMP TABLE condition_batch_test_counter (inserts integer NOT NULL); INSERT INTO condition_batch_test_counter VALUES (0); CREATE FUNCTION pg_temp.condition_batch_test_count() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN UPDATE condition_batch_test_counter SET inserts=inserts+1; RETURN NULL; END $$; CREATE TRIGGER condition_batch_test_trigger AFTER INSERT ON game.item_attributes FOR EACH STATEMENT EXECUTE FUNCTION pg_temp.condition_batch_test_count();").await?;
            assert_eq!(save_conditions(tx,LOCATION,&IDS,&[(a,[1.0,2.0,3.0],1),(b,[4.0,5.0,6.0],2)]).await?, vec![a,b]);
            let count: i32 = tx.query_one("SELECT inserts FROM condition_batch_test_counter", &[]).await?.get("inserts");
            assert_eq!(count, 1);
            Err(Error::from(tx.execute("SELECT condition_batch_test_rollback_ddl", &[]).await.unwrap_err()))
        }), |(), ()| async { Ok(()) }).await;
        assert!(result.is_err());
    }).await;
    finish(fixture, result).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn stale_batch_does_not_restore_moved_item() { race(false).await; }
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn stale_batch_does_not_restore_deleted_item() { race(true).await; }

async fn race(delete: bool) {
    let db = database().await;
    let fixture = Fixture::new(db.clone(), 1).await;
    let item = fixture.items[0];
    let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let worker_db = db.clone();
    let mut worker = tokio::spawn(async move {
        let mut pid_tx = Some(pid_tx);
        let mut release_rx = Some(release_rx);
        worker_db.transaction::<(), (), Error, (), _, _, _>(move |tx, _| {
            let pid_tx = pid_tx.take().unwrap();
            let release_rx = release_rx.take().unwrap();
            Box::pin(async move {
                let pid: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
                tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&item]).await?;
                pid_tx.send(pid).unwrap();
                release_rx.await.unwrap();
                if delete { tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await?; }
                else { tx.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&item, &OTHER_LOCATION]).await?; }
                Ok(())
            })
        }, |(), ()| async { Ok(()) }).await
    });
    let mut worker_joined = false;
    let mut batch_task = None;
    let mut batch_joined = false;
    let result = crate::tests::catch_scenario(async {
        let blocker = pid_rx.await.unwrap();
        let batch_db = db.clone();
        batch_task = Some(tokio::spawn(async move {
            save_conditions(&batch_db, LOCATION, &IDS, &[(item, [1.0, 2.0, 3.0], 1)]).await
        }));
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let waiting: bool = db.query_one(
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) AND pid <> pg_backend_pid() AND query LIKE '%game.item_attributes%') AS waiting",
                    &[&blocker],
                ).await.unwrap().get("waiting");
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        release_tx.send(()).unwrap();
        let worker_result = (&mut worker).await;
        worker_joined = true;
        assert!(worker_result.unwrap().is_ok());
        let batch_result = batch_task.as_mut().unwrap().await;
        batch_joined = true;
        assert!(batch_result.unwrap().unwrap().is_empty());
        assert!(db.query("SELECT item_id FROM game.item_attributes WHERE item_id=$1", &[&item]).await.unwrap().is_empty());
    }).await;
    if !worker_joined { worker.abort(); let _ = worker.await; }
    if !batch_joined { let batch = batch_task.take().unwrap(); batch.abort(); let _ = batch.await; }
    finish(fixture, result).await;
}
