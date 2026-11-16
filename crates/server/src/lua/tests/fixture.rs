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
            // Keys captured before the first delete scope the absence checks
            // below; cleanup never compares whole-table state.
            let owners = [fixture.account, id];
            let subjects = [id];
            let items: Vec<i64> = tx.query("SELECT item_id FROM game.items WHERE owner_id = $1 OR item_id = $2", &[&id, &fixture.ship_id]).await?.iter().map(|row| row.get("item_id")).collect();
            let events: Vec<String> = tx.query("SELECT event_id::text AS event_id FROM game.progress_events WHERE subject_character_id = ANY($2) OR (kind = 'WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint = ANY($1)) OR (kind IN ('DamageApplied', 'RemoteRepairApplied', 'WarpScrambleApplied') AND (payload->kind->'target'->>'character_id')::bigint = ANY($2))", &[&owners.as_slice(), &subjects.as_slice()]).await?.iter().map(|row| row.get("event_id")).collect();
            let kills: Vec<i64> = tx.query("SELECT kill_id FROM game.killmails WHERE victim_id = $1", &[&id]).await?.iter().map(|row| row.get("kill_id")).collect();
            crate::services::tests::fixture::cleanup_recovery(tx, fixture.char_id).await?;
            crate::services::tests::fixture::cleanup_progress(tx, &owners, &subjects).await?;
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
            tx.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id = $1)", &[&id]).await?;
            tx.execute("DELETE FROM game.wallet_accounts WHERE owner_id = $1", &[&id]).await?;
            tx.execute("DELETE FROM game.characters WHERE character_id = $1 AND user_id = $2", &[&id, &fixture.account]).await?;
            tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&fixture.account]).await?;
            assert!(crate::dao::tests::item_state(tx, &items).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.lua_store WHERE script = $1", &[&script]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.market_transactions WHERE buyer_id = $1 OR seller_id = $1", &[&id]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.market_order_history WHERE char_id = $1", &[&id]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.market_orders WHERE char_id = $1", &[&id]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.notifications WHERE receiver_id = $1", &[&id]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.killmails WHERE victim_id = $1", &[&id]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.bounty_payouts WHERE killmail_id = ANY($1)", &[&kills]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.insurance WHERE owner_id = $1 OR ship_id = ANY($2)", &[&id, &items]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.blueprints WHERE item_id = ANY($1)", &[&items]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.progress_events WHERE event_id = ANY(SELECT unnest($1::text[])::uuid)", &[&events]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.progress_deliveries WHERE event_id = ANY(SELECT unnest($1::text[])::uuid)", &[&events]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.characters WHERE character_id = $1", &[&id]).await?.is_empty());
            assert!(tx.query("SELECT 1 FROM game.accounts WHERE user_id = $1", &[&fixture.account]).await?.is_empty());
            Ok(())
            })
        },
        |(), ()| async move { Ok(()) },
    ).await
}

pub(super) async fn order_count(db: &Db, char_id: i64) -> Result<i64, Error> {
    crate::dao::market::trading::order_count(db, char_id).await
}

pub(super) async fn owned_item_count(db: &Db, char_id: i64) -> Result<i64, Error> {
    Ok(db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id = $1", &[&char_id]).await?.get("count"))
}
