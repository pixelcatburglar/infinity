//! Clone authority initialization at the simulated account boundary.
use super::*;
use infinity_core::consts::ROLE_PLAYER_LOGIN;
use crate::{dao::accounts::{authentication, subscriptions}, services::subscriptions::rules};

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn initial_authority_matches_registration_and_preserves_existing_accounts() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    assert!(ensure(&db).await.unwrap());
    let reserved = account_id(&db).await.unwrap();
    let outcome: Result<(), db::Error> = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        // Keep the real reserved account and every fixture change inside a transaction
        // that always rolls back, including on assertion failure.
        let scratch: String = tx.query_one("SELECT 'authority-scratch-' || nextval('game.accounts_user_id_seq')::text AS name", &[]).await?.get("name");
        tx.execute("UPDATE game.accounts SET user_name=$2 WHERE user_id=$1", &[&reserved, &scratch]).await?;
        let name = format!("{scratch}-player");
        let registered = authentication::register(tx, &name, b"fixture", ROLE_PLAYER_LOGIN, 0).await?;
        let normal = subscriptions::by_user(tx, registered.user_id).await?;
        assert_eq!((normal.grade, normal.omega_until, normal.grace_until, normal.revision), (rules::OMEGA, None, None, 0));

        assert!(ensure(tx).await?);
        let fresh = account_id(tx).await?;
        // The daily assignment path reads this same fail-closed grade authority.
        assert_eq!(rules::current(tx, fresh, 0).await.unwrap(), rules::OMEGA);
        let row = subscriptions::by_user(tx, fresh).await?;
        assert_eq!((row.grade, row.omega_until, row.grace_until, row.revision), (normal.grade, normal.omega_until, normal.grace_until, normal.revision));
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&fresh]).await?;

        let password: &[u8] = b"existing";
        let existing: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ($1,$2,$3) RETURNING user_id", &[&ACCOUNT_NAME, &password, &ROLE_SERVICE]).await?.get("user_id");
        assert!(subscriptions::by_user(tx, existing).await.is_err());
        assert!(ensure(tx).await?);
        assert_eq!(account_id(tx).await?, existing);
        assert_eq!(rules::current(tx, existing, 0).await.unwrap(), rules::OMEGA);
        let row = subscriptions::by_user(tx, existing).await?;
        assert_eq!((row.grade, row.omega_until, row.grace_until, row.revision), (rules::OMEGA, None, None, 0));

        for (grade, omega, grace, revision) in [(rules::ALPHA, None, None, 7_i64), (rules::OMEGA, Some(100_i64), Some(200_i64), 9_i64)] {
            tx.execute("UPDATE game.account_clone_grades SET grade=$2,omega_until=$3,grace_until=$4,revision=$5 WHERE user_id=$1", &[&existing, &grade, &omega, &grace, &revision]).await?;
            assert!(ensure(tx).await?);
            let row = subscriptions::by_user(tx, existing).await?;
            assert_eq!((row.grade, row.omega_until, row.grace_until, row.revision), (grade, omega, grace, revision));
        }
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&existing]).await?;

        let player: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ($1,$2,$3) RETURNING user_id", &[&ACCOUNT_NAME, &password, &ROLE_PLAYER_LOGIN]).await?.get("user_id");
        assert!(!ensure(tx).await?);
        let unchanged = tx.query_one("SELECT user_name,password_hash,role FROM game.accounts WHERE user_id=$1", &[&player]).await?;
        assert_eq!(unchanged.get::<_, String>("user_name"), ACCOUNT_NAME);
        assert_eq!(unchanged.get::<_, Vec<u8>>("password_hash"), password);
        assert_eq!(unchanged.get::<_, i64>("role"), ROLE_PLAYER_LOGIN);
        assert!(subscriptions::by_user(tx, player).await.is_err());

        // Force rollback of the complete fixture, including the original rename.
        tx.query_one("SELECT 1 / 0", &[]).await?;
        Ok(())
    }), |(), ()| async move { Ok(()) }).await;
    assert!(outcome.is_err());
    assert_eq!(account_id(&db).await.unwrap(), reserved);
}
