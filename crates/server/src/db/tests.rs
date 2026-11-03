//! Real-database coverage for the reserved service account and character selection.

use super::*;
use crate::dao::{accounts::selection as account_selection, characters::selection as character_selection};
use crate::db::transaction::TransactionError;

#[derive(Debug)]
enum Rollback {
    Database(Error),
    Observed { collisions: Vec<bool>, isolated: bool },
}
impl From<Error> for Rollback {
    fn from(error: Error) -> Self { Self::Database(error) }
}
impl From<tokio_postgres::Error> for Rollback {
    fn from(error: tokio_postgres::Error) -> Self { Self::Database(error.into()) }
}
impl TransactionError for Rollback {
    fn is_retryable_transaction(&self) -> bool {
        match self { Self::Database(error) => error.is_retryable_transaction(), Self::Observed { .. } => false }
    }
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn simulated_account_startup_login_collision_and_selection() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    assert!(simulated::ensure(&db).await.unwrap());
    let reserved = db.query_one("SELECT user_id, role, password_hash, isfinite(created_at) AS finite, last_login::text AS last_login FROM game.accounts WHERE lower(user_name) = lower($1)", &[&simulated::ACCOUNT_NAME]).await.unwrap();
    let reserved_id: i64 = reserved.get("user_id");
    let last_login: Option<String> = reserved.get("last_login");
    assert_ne!(reserved.get::<_, i64>("role") & ROLE_SERVICE, 0);
    assert!(reserved.get::<_, Vec<u8>>("password_hash").is_empty());
    assert!(reserved.get::<_, bool>("finite"));
    account_selection::creation_date(&db, reserved_id, crate::consts::FILETIME_SECOND, crate::consts::FILETIME_UNIX_EPOCH).await.unwrap();
    assert!(simulated::ensure(&db).await.unwrap());
    let again = db.query_one("SELECT user_id FROM game.accounts WHERE lower(user_name) = lower($1)", &[&simulated::ACCOUNT_NAME]).await.unwrap();
    assert_eq!(again.get::<_, i64>("user_id"), reserved_id);

    for enabled in [false, true] {
        let policy = DevelopmentConfig { auto_register_accounts: enabled, ..Default::default() };
        for name in [simulated::ACCOUNT_NAME, "INFINITY-SIMULATED-PILOTS", "InFiNiTy-SiMuLaTeD-PiLoTs"] {
            for password in [&b""[..], &b"fixture"[..]] {
                assert!(db.login(name, password, &policy).await.unwrap().is_none());
            }
        }
    }
    let after: Option<String> = db.query_one("SELECT last_login::text AS last_login FROM game.accounts WHERE user_id = $1", &[&reserved_id]).await.unwrap().get("last_login");
    assert_eq!(last_login, after);
    assert_eq!((ROLE_PLAYER_LOGIN | GM_ROLES) & ROLE_SERVICE, 0);

    let row = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('simulated-test-' || nextval('game.accounts_user_id_seq')::text, $1, $2) RETURNING user_id, user_name", &[&&b"fixture"[..], &ROLE_PLAYER_LOGIN]).await.unwrap();
    let user_id: i64 = row.get("user_id");
    let name: String = row.get("user_name");
    let result: anyhow::Result<()> = async {
        let account = db.login(&name, b"fixture", &DevelopmentConfig::default()).await?;
        anyhow::ensure!(account.is_some_and(|account| account.user_id == user_id), "ordinary login failed");
        db.execute("UPDATE game.accounts SET role = $2 WHERE user_id = $1", &[&user_id, &ROLE_SERVICE]).await?;
        anyhow::ensure!(db.login(&name, b"fixture", &DevelopmentConfig::default()).await?.is_none(), "renamed service account logged in");
        Ok(())
    }.await;
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&user_id]).await.unwrap();
    result.unwrap();

    let outcome: Result<(), Rollback> = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let scratch: String = tx.query_one("SELECT 'simulated-scratch-' || nextval('game.accounts_user_id_seq')::text AS name", &[]).await?.get("name");
        tx.execute("UPDATE game.accounts SET user_name = $2 WHERE user_id = $1", &[&reserved_id, &scratch]).await?;
        let mut collisions = Vec::new();
        for password in [&b""[..], &b"fixture"[..]] {
            let inserted = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ($1, $2, $3) RETURNING user_id", &[&simulated::ACCOUNT_NAME, &password, &ROLE_PLAYER_LOGIN]).await?;
            let id: i64 = inserted.get("user_id");
            let accepted = simulated::ensure(tx).await?;
            let row = tx.query_one("SELECT user_id, password_hash, role FROM game.accounts WHERE user_id = $1", &[&id]).await?;
            collisions.push(!accepted && row.get::<_, i64>("user_id") == id && row.get::<_, Vec<u8>>("password_hash") == password && row.get::<_, i64>("role") == ROLE_PLAYER_LOGIN);
            tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&id]).await?;
        }
        let ordinary: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('simulated-selection-' || nextval('game.accounts_user_id_seq')::text, $1, $2) RETURNING user_id", &[&&b"fixture"[..], &ROLE_PLAYER_LOGIN]).await?.get("user_id");
        let insert_character = "INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, solar_system_id, constellation_id, region_id) VALUES ($1, 'simulated-selection-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, 1, 1, 1) RETURNING character_id";
        let service_character: i64 = tx.query_one(insert_character, &[&reserved_id]).await?.get("character_id");
        let ordinary_character: i64 = tx.query_one(insert_character, &[&ordinary]).await?.get("character_id");
        let characters = character_selection::characters(tx, ordinary, crate::consts::FILETIME_SECOND, crate::consts::FILETIME_UNIX_EPOCH).await?;
        let owns = character_selection::owns(tx, service_character, ordinary).await?;
        let selected = character_selection::selected(tx, service_character, ordinary).await?;
        let isolated = characters.iter().any(|character| character.character_id == ordinary_character)
            && characters.iter().all(|character| character.character_id != service_character)
            && !owns && selected.is_none();
        Err(Rollback::Observed { collisions, isolated })
    }), |(), (): ()| async { Ok(()) }).await;
    match outcome {
        Err(Rollback::Observed { collisions, isolated }) => { assert_eq!(collisions, vec![true, true]); assert!(isolated); }
        other => panic!("collision transaction failed: {other:?}"),
    }
    let restored: i64 = db.query_one("SELECT user_id FROM game.accounts WHERE lower(user_name) = lower($1)", &[&simulated::ACCOUNT_NAME]).await.unwrap().get("user_id");
    assert_eq!(restored, reserved_id);
}
