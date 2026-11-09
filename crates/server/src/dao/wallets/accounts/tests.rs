use super::*;
use infinity_core::consts::ACCOUNT_CASH;
use crate::db::{Db, Error};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn ensure_preserves_account_identity_per_owner_and_key() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner:i64 = -8_861_203_001;
    let result: Result<(), Error> = db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let count:i64 = tx.query_one("SELECT count(*) AS count FROM game.wallet_accounts WHERE owner_id=$1", &[&owner]).await?.get("count");
            assert_eq!(count,0);
            let a=ensure(tx,owner,ACCOUNT_CASH).await?;
            assert_eq!(a,ensure(tx,owner,ACCOUNT_CASH).await?);
            let b=ensure(tx,owner,1001).await?;
            assert_ne!(a,b);
            let rows = tx.query("SELECT account_id, account_key FROM game.wallet_accounts WHERE owner_id=$1 ORDER BY account_key", &[&owner]).await?;
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].get::<_, i32>("account_key"), ACCOUNT_CASH);
            assert_eq!(rows[0].get::<_, i64>("account_id"), a.id);
            assert_eq!(rows[1].get::<_, i32>("account_key"), 1001);
            assert_eq!(rows[1].get::<_, i64>("account_id"), b.id);
            tx.execute("DELETE FROM game.wallet_accounts WHERE owner_id=$1", &[&owner]).await?;
            Ok(())
        }),
        |(), ()| async { Ok(()) },
    ).await;
    result.unwrap();
}
