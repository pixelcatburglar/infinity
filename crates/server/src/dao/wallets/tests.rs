use super::*;
use crate::db::{Db, Error};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn guarded_mutations_preserve_exact_values_and_report_outcomes() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = -8_861_204_001i64;
    let result: Result<(), Error> = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let one = IskAmount::from_cents(1).unwrap();
        let max = IskAmount::parse_decimal("9999999999999999999999999999999999.99").unwrap();
        assert_eq!(characters::credit(tx, owner, one).await.unwrap(), WalletMutation::Missing);
        assert_eq!(characters::debit(tx, owner, one).await.unwrap(), WalletMutation::Missing);
        assert_eq!(corporations::balance(tx, owner, 1000).await.unwrap(), IskAmount::ZERO);
        assert_eq!(corporations::debit(tx, owner, 1000, one).await.unwrap(), WalletMutation::Insufficient(IskAmount::ZERO));
        assert_eq!(corporations::credit(tx, owner, 1000, max).await.unwrap(), WalletMutation::Applied(max));
        assert_eq!(corporations::credit(tx, owner, 1000, one).await.unwrap(), WalletMutation::Overflow);
        assert_eq!(corporations::debit(tx, owner, 1000, one).await.unwrap(), WalletMutation::Applied(max.checked_sub(one).unwrap()));
        assert_eq!(corporations::debit(tx, owner, 1000, max).await.unwrap(), WalletMutation::Insufficient(max.checked_sub(one).unwrap()));
        tx.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=1000", &[&owner]).await?;
        Ok(())
    }), |(), ()| async { Ok(()) }).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn character_mutations_keep_cents_and_distinguish_refusals() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let one = IskAmount::from_cents(1).unwrap();
    let max = IskAmount::parse_decimal("9999999999999999999999999999999999.99").unwrap();
    assert_eq!(characters::set(&db, fixture.character, max).await.unwrap(), WalletMutation::Applied(max));
    assert!(characters::covers(&db, fixture.character, max).await.unwrap());
    assert_eq!(characters::credit(&db, fixture.character, one).await.unwrap(), WalletMutation::Overflow);
    let after = max.checked_sub(one).unwrap();
    assert_eq!(characters::debit(&db, fixture.character, one).await.unwrap(), WalletMutation::Applied(after));
    assert_eq!(characters::balance(&db, fixture.character).await.unwrap(), after);
    assert_eq!(characters::debit(&db, fixture.character, max).await.unwrap(), WalletMutation::Insufficient(after));
    assert!(!characters::covers(&db, fixture.character, max).await.unwrap());
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
