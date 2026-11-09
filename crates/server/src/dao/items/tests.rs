//! Test-only isolated game rows for Lua mutation integration tests.
use crate::db::{Db, Error};

#[derive(Clone, Copy)]
pub(super) struct Fixture { pub account: i64, pub chars: [i64; 3] }

pub(super) async fn create(db: &Db) -> Result<Fixture, Error> {
    db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('lua-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let mut chars = [0; 3];
            for id in &mut chars {
                *id = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'lua-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
            }
            Ok(Fixture { account, chars })
        }),
        |fixture, ()| async move { Ok(fixture) },
    ).await
}

pub(super) async fn cleanup(db: &Db, fixture: Fixture) -> Result<(), Error> {
    db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            crate::services::tests::fixture::cleanup_progress(tx, &fixture.chars, &fixture.chars).await?;
            tx.execute("DELETE FROM game.mail_status WHERE message_id IN (SELECT message_id FROM game.mail_messages WHERE sender_id = ANY($1))", &[&&fixture.chars[..]]).await?;
            tx.execute("DELETE FROM game.mail_messages WHERE sender_id = ANY($1)", &[&&fixture.chars[..]]).await?;
            tx.execute("DELETE FROM game.standings WHERE char_id = ANY($1)", &[&&fixture.chars[..]]).await?;
            tx.execute("DELETE FROM game.items WHERE owner_id = ANY($1)", &[&&fixture.chars[..]]).await?;
            tx.execute("DELETE FROM game.characters WHERE user_id = $1", &[&fixture.account]).await?;
            tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&fixture.account]).await?;
            Ok(())
        }),
        |(), ()| async move { Ok(()) },
    ).await
}

pub(super) async fn hold_character(
    db: &Db,
    character: i64,
    ready: tokio::sync::oneshot::Sender<i32>,
    release: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), Error> {
    let state = std::sync::Arc::new(tokio::sync::Mutex::new(Some((ready, release))));
    db.transaction(
        |tx, _: &mut ()| {
            let state = state.clone();
            Box::pin(async move {
                let row = tx.query_one("SELECT pg_backend_pid() AS blocker_pid FROM game.characters WHERE character_id = $1 FOR UPDATE", &[&character]).await?;
                let (ready, release) = state.lock().await.take().expect("fixture lock transaction retried");
                let _ = ready.send(row.get("blocker_pid"));
                let _ = release.await;
                Ok(())
            })
        },
        |(), ()| async move { Ok(()) },
    ).await
}

pub(super) async fn wallet_update_waiting(db: &Db, blocker_pid: i32) -> Result<bool, Error> {
    let row = db.query_one(
        "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND $1 = ANY(pg_blocking_pids(pid))) AS waiting",
        &[&blocker_pid],
    ).await?;
    Ok(row.get("waiting"))
}

#[tokio::test]
#[ignore]
async fn charge_quantities_round_trip_above_i32() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship: i64 = tx.query_one("SELECT nextval('game.items_item_id_seq') AS id", &[]).await.unwrap().get("id");
    let quantity = i64::from(i32::MAX) + 2;
    crate::dao::items::combat::apply_loads(&tx, ship, &[(0, 1, 1, quantity, 0), (0, 2, 1, 0, 0)]).await.unwrap();
    assert_eq!(crate::dao::items::charges::loaded(&tx, ship, 1).await.unwrap().unwrap().quantity, quantity);
    assert_eq!(crate::dao::items::charges::all(&tx, ship).await.unwrap().len(), 2);
    assert_eq!(crate::dao::items::combat::consume_charge(&tx, ship, 1).await.unwrap().unwrap().quantity, quantity - 1);
    let reserved = std::collections::BTreeMap::from([(1, (1, quantity - 1, 0))]);
    assert!(crate::dao::items::combat::delete_reserved(&tx, ship, &reserved).await.unwrap());
    assert_eq!(crate::dao::items::charges::loaded(&tx, ship, 2).await.unwrap().unwrap().quantity, 0);
    tx.rollback().await.unwrap();
}
