//! Isolated rows for the capacitor real-database smoke test.
use crate::{dao::items::creation, db::{Db, Error}};

#[derive(Clone, Copy)]
pub(crate) struct Fixture { pub account: i64, pub character: i64, pub ship: i64 }

pub(crate) async fn create(db: &Db, ship_type: i32, system: i64) -> Result<Fixture, Error> {
    db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('capacitor-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let character = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'capacitor-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
            let ship = creation::positioned(tx, ship_type, character, system, [0.0; 3]).await?;
            Ok(Fixture { account, character, ship })
        }),
        |fixture, ()| async move { Ok(fixture) },
    ).await
}

pub(crate) async fn cleanup(db: &Db, fixture: Fixture) -> Result<(), Error> {
    db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            crate::services::tests::fixture::cleanup_progress(tx, &[fixture.account, fixture.character], &[fixture.character]).await?;
            tx.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2", &[&fixture.ship, &fixture.character]).await?;
            tx.execute("DELETE FROM game.characters WHERE character_id = $1 AND user_id = $2", &[&fixture.character, &fixture.account]).await?;
            tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&fixture.account]).await?;
            Ok(())
        }),
        |(), ()| async move { Ok(()) },
    ).await
}

pub(crate) async fn fingerprint(db: &Db) -> Result<Vec<String>, Error> {
    let mut result = Vec::new();
    for sql in [
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY user_id), '')) AS fingerprint FROM game.accounts t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id), '')) AS fingerprint FROM game.characters t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id), '')) AS fingerprint FROM game.items t",
    ] { result.push(db.query_one(sql, &[]).await?.get("fingerprint")); }
    Ok(result)
}
