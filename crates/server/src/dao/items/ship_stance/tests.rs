//! Real PostgreSQL coverage of stance timing persistence and its guarded write.
use super::*;
use crate::{config::deployment::DatabaseConfig, db::Db};

async fn database() -> Db {
    crate::tests::init_config();
    Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap()
}

struct Fixture { db: Db, owner: i64, ship: i64 }
impl Fixture {
    async fn new(db: Db) -> Self {
        let (owner, ship) = db.transaction::<(i64, i64), (i64, i64), Error, (), _, _, _>(move |tx, _| Box::pin(async move {
            let owner: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('ship_stance_test_' || nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
            let ship: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES (1,$1,$1,4,1,true) RETURNING item_id", &[&owner]).await?.get("item_id");
            Ok((owner, ship))
        }), |value, ()| async move { Ok(value) }).await.unwrap();
        Self { db, owner, ship }
    }
    async fn cleanup(self) {
        self.db.execute("DELETE FROM game.ship_stances WHERE ship_id=$1", &[&self.ship]).await.unwrap();
        self.db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&self.owner]).await.unwrap();
        self.db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&self.owner]).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn timing_insert_is_idempotent_replace_is_guarded_and_clear_removes() {
    let fixture = Fixture::new(database().await).await;
    let result = crate::tests::catch_scenario(async {
        assert!(timing(&fixture.db, fixture.ship).await.unwrap().is_none());
        assert!(insert_timing(&fixture.db, fixture.ship).await.unwrap());
        assert!(!insert_timing(&fixture.db, fixture.ship).await.unwrap(), "a second initialization must not reset timing");
        let initial = timing(&fixture.db, fixture.ship).await.unwrap().unwrap();
        assert_eq!(initial, Timing { previous_stance_id: None, switched_at: 0, ready_at: 0 });
        let next = Timing { previous_stance_id: Some(1), switched_at: 10, ready_at: 20 };
        let wrong = Timing { switched_at: 9, ..next.clone() };
        assert!(!replace_timing(&fixture.db, fixture.ship, &wrong, &next).await.unwrap());
        assert_eq!(timing(&fixture.db, fixture.ship).await.unwrap().unwrap(), initial, "a missed guard rewrote timing");
        assert!(replace_timing(&fixture.db, fixture.ship, &initial, &next).await.unwrap());
        assert_eq!(timing(&fixture.db, fixture.ship).await.unwrap().unwrap(), next);
        clear(&fixture.db, &[fixture.ship]).await.unwrap();
        assert!(timing(&fixture.db, fixture.ship).await.unwrap().is_none());
        clear(&fixture.db, &[]).await.unwrap();
    }).await;
    fixture.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
