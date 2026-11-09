//! Isolated game rows for Lua event integration tests; no client API is mirrored.
//! Covers accounts, characters, items, Lua store, market and ship-loss side effects.
//! It does not restore changes to rows belonging to other players or NPCs.
use crate::db::{Db, Error};

#[derive(Clone, Copy)]
pub(super) struct Fixture {
    pub account: i64,
    pub char_id: i64,
    pub ship_id: i64,
}

pub(super) async fn create(db: &Db, ship_type: i32, system: i64) -> Result<Fixture, Error> {
    db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('lua-events-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let char_id: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, solar_system_id, station_id) VALUES ($1, 'lua-events-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, NULL) RETURNING character_id", &[&account, &system]).await?.get("character_id");
            let ship_id = crate::dao::items::creation::positioned(tx, ship_type, char_id, system, [0.0, 0.0, 0.0]).await?;
            tx.execute("UPDATE game.characters SET ship_id = $2 WHERE character_id = $1", &[&char_id, &ship_id]).await?;
            Ok(Fixture { account, char_id, ship_id })
        }),
        |fixture, ()| async move { Ok(fixture) },
    ).await
}

pub(super) async fn cleanup(db: &Db, fixture: Fixture, script: &str) -> Result<(), Error> {
    let script = script.to_owned();
    db.transaction(
        |tx, _: &mut ()| {
            let script = script.clone();
            Box::pin(async move {
            let id = fixture.char_id;
            crate::services::tests::fixture::cleanup_progress(tx, &[fixture.account, id], &[id]).await?;
            let kills: Vec<i64> = tx.query("SELECT kill_id FROM game.killmails WHERE victim_id = $1", &[&id]).await?.iter().map(|row| row.get("kill_id")).collect();
            crate::services::tests::fixture::cleanup_killmail_rewards(tx, &kills).await?;
            tx.execute("DELETE FROM game.lua_store WHERE script = $1", &[&script]).await?;
            tx.execute("DELETE FROM game.market_transactions WHERE buyer_id = $1 OR seller_id = $1", &[&id]).await?;
            tx.execute("DELETE FROM game.market_order_history WHERE char_id = $1", &[&id]).await?;
            tx.execute("DELETE FROM game.market_orders WHERE char_id = $1", &[&id]).await?;
            tx.execute("DELETE FROM game.notifications WHERE receiver_id = $1", &[&id]).await?;
            tx.execute("DELETE FROM game.killmails WHERE victim_id = $1", &[&id]).await?;
            tx.execute("DELETE FROM game.insurance WHERE owner_id = $1 OR ship_id = $2", &[&id, &fixture.ship_id]).await?;
            tx.execute("DELETE FROM game.module_charges WHERE ship_id = $2 OR ship_id IN (SELECT item_id FROM game.items WHERE owner_id = $1)", &[&id, &fixture.ship_id]).await?;
            tx.execute("DELETE FROM game.pending_reloads WHERE owner_id = $1", &[&id]).await?;
            tx.execute("DELETE FROM game.weapon_banks WHERE ship_id = $2 OR ship_id IN (SELECT item_id FROM game.items WHERE owner_id = $1)", &[&id, &fixture.ship_id]).await?;
            tx.execute("DELETE FROM game.blueprints WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id = $1)", &[&id]).await?;
            tx.execute("DELETE FROM game.items WHERE owner_id = $1", &[&id]).await?;
            tx.execute("DELETE FROM game.characters WHERE character_id = $1 AND user_id = $2", &[&id, &fixture.account]).await?;
            tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&fixture.account]).await?;
            Ok(())
            })
        },
        |(), ()| async move { Ok(()) },
    ).await
}

pub(super) async fn fingerprint(db: &Db) -> Result<Vec<Vec<String>>, Error> {
    let mut fingerprints = Vec::new();
    for table in [
        "accounts", "characters", "items", "item_attributes", "lua_store",
        "market_orders", "market_order_history", "market_transactions", "killmails",
        "insurance", "notifications", "module_charges", "pending_reloads",
        "weapon_banks", "blueprints", "progress_events", "progress_deliveries", "bounty_payouts",
    ] {
        fingerprints.push(crate::services::drones::tests::fixture::table_fingerprint(db, table).await?);
    }
    Ok(fingerprints)
}

pub(super) async fn order_count(db: &Db, char_id: i64) -> Result<i64, Error> {
    crate::dao::market::trading::order_count(db, char_id).await
}

pub(super) async fn owned_item_count(db: &Db, char_id: i64) -> Result<i64, Error> {
    Ok(db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id = $1", &[&char_id]).await?.get("count"))
}
