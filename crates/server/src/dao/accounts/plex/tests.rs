//! Transactional coverage of vault persistence using rolled-back fixture rows.
#[cfg(any(feature = "tests-storage", feature = "tests-economy", feature = "tests-gateway", feature = "tests-inventory", feature = "tests-slash"))]
use super::*;
#[cfg(any(feature = "tests-storage", feature = "tests-economy", feature = "tests-gateway", feature = "tests-inventory", feature = "tests-slash"))]
use crate::db::Db;
#[cfg(feature = "tests-storage")]
use crate::{db::outbox::{Outbox, Effect, Notification}, services::{CallError, plex_vault::{change_in, VaultAudience, VaultChange, VaultContext, VaultKind}}};

#[cfg(feature = "tests-economy")]
pub(crate) async fn cleanup(db: &Db, user: i64) {
    db.execute("DELETE FROM game.plex_vault_transactions WHERE user_id = $1", &[&user]).await.unwrap();
}
#[cfg(any(feature = "tests-inventory", feature = "tests-slash", feature = "tests-economy", feature = "tests-storage"))]
pub(crate) async fn ledger_counts(db: &Db, user: i64) -> (i64, i64) {
    let row = db.query_one("SELECT count(*) AS total, count(*) FILTER (WHERE kind = 'opening') AS openings FROM game.plex_vault_transactions WHERE user_id = $1", &[&user]).await.unwrap();
    (row.get("total"), row.get("openings"))
}
#[cfg(any(feature = "tests-gateway", feature = "tests-storage"))]
pub(crate) async fn opening_id(db: &Db, user: i64) -> Option<i64> {
    db.query_opt("SELECT transaction_id FROM game.plex_vault_transactions WHERE user_id = $1 AND kind = 'opening'", &[&user]).await.unwrap().as_ref().map(|row| row.get("transaction_id"))
}

#[cfg(any(feature = "tests-storage", feature = "tests-economy", feature = "tests-gateway"))]
pub(crate) async fn set_balance(db: &Db, user: i64, balance: &str) -> Result<(), db::Error> {
    db.execute("UPDATE game.accounts SET plex_balance = $2::text::numeric WHERE user_id = $1", &[&user, &balance]).await?;
    Ok(())
}

#[cfg(any(feature = "tests-economy", feature = "tests-gateway", feature = "tests-inventory", feature = "tests-slash", feature = "tests-storage"))]
/// Isolated account and selected character for inventory conversion tests.
pub(crate) struct ConversionFixture {
    pub(crate) user: i64,
    pub(crate) character: i64,
    #[cfg(any(feature = "tests-economy", feature = "tests-inventory", feature = "tests-slash"))]
    pub(crate) station: i64,
    items: Vec<i64>,
}

#[cfg(any(feature = "tests-economy", feature = "tests-gateway", feature = "tests-inventory", feature = "tests-slash", feature = "tests-storage"))]
impl ConversionFixture {
    #[cfg(any(feature = "tests-economy", feature = "tests-gateway", feature = "tests-inventory", feature = "tests-slash", feature = "tests-storage"))]
    pub(crate) async fn create(db: &Db, station: i64, balance: &str) -> Result<Self, db::Error> {
        let balance = balance.to_owned();
        db.transaction(
            |tx, _: &mut ()| {
                let balance = balance.clone();
                Box::pin(async move {
                let user = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role, plex_balance) VALUES ('plex-conversion-' || nextval('game.accounts_user_id_seq')::text, $1, 0, $2::text::numeric) RETURNING user_id", &[&&b"fixture"[..], &balance]).await?.get("user_id");
                let character = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id) VALUES ($1, 'plex-conversion-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2) RETURNING character_id", &[&user, &station]).await?.get("character_id");
                Ok(Self { user, character, #[cfg(any(feature = "tests-economy", feature = "tests-inventory", feature = "tests-slash"))] station, items: Vec::new() })
                })
            },
            |fixture, ()| async move { Ok(fixture) },
        ).await
    }
    #[cfg(any(feature = "tests-economy", feature = "tests-inventory", feature = "tests-slash"))]
    pub(crate) async fn another_character(&self, db: &Db) -> Result<i64, db::Error> {
        Ok(db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id) VALUES ($1, 'plex-conversion-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2) RETURNING character_id", &[&self.user, &self.station]).await?.get("character_id"))
    }

    #[cfg(feature = "tests-inventory")]
    /// Owner and location may differ from the selected character and station.
    pub(crate) async fn stack(&mut self, db: &Db, owner: i64, location: i64, flag: i32, quantity: i64, singleton: bool) -> Result<i64, db::Error> {
        let item = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, $6) RETURNING item_id", &[&infinity_core::consts::TYPE_PLEX, &owner, &location, &flag, &quantity, &singleton]).await?.get("item_id");
        self.items.push(item);
        Ok(item)
    }

    #[cfg(feature = "tests-inventory")]
    /// Put an existing personal item in a direct-trade reservation.
    pub(crate) async fn reserve(&self, db: &Db, item: i64, origin_location: i64, origin_flag: i32) -> Result<(), db::Error> {
        let station = self.station;
        let character = self.character;
        db.transaction(
            |tx, _: &mut ()| Box::pin(async move {
                let trade: i64 = tx.query_one("INSERT INTO game.trades (station_id, char_a, char_b, state, created_at, updated_at) VALUES ($1, $2, $2, 'open', 0, 0) RETURNING trade_id", &[&station, &character]).await?.get("trade_id");
                tx.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&item, &trade]).await?;
                tx.execute("INSERT INTO game.trade_items (item_id, trade_id, owner_id, origin_location, origin_flag) VALUES ($1, $2, $3, $4, $5)", &[&item, &trade, &character, &origin_location, &origin_flag]).await?;
                Ok(())
            }),
            |(), ()| async move { Ok(()) },
        ).await
    }

    #[cfg(feature = "tests-inventory")]
    /// Dormant custody requires the schema's implant-shaped singleton row.
    pub(crate) async fn dormant(&self, db: &Db, item: i64) -> Result<(), db::Error> {
        let station = self.station;
        let character = self.character;
        db.transaction(
            |tx, _: &mut ()| Box::pin(async move {
                let clone: i64 = tx.query_one("INSERT INTO game.jump_clones (owner_id, location_id) VALUES ($1, $2) RETURNING clone_id", &[&character, &station]).await?.get("clone_id");
                tx.execute("UPDATE game.items SET owner_id = $2, location_id = $2, flag_id = $3, quantity = 1, singleton = true, brain_slot = 1, jump_clone_id = $4 WHERE item_id = $1", &[&item, &character, &infinity_core::consts::FLAG_IMPLANT, &clone]).await?;
                Ok(())
            }),
            |(), ()| async move { Ok(()) },
        ).await
    }

    #[cfg(any(feature = "tests-economy", feature = "tests-gateway", feature = "tests-inventory", feature = "tests-slash", feature = "tests-storage"))]
    pub(crate) async fn cleanup(self, db: &Db) -> Result<(), db::Error> {
        db.transaction(
            |tx, _: &mut ()| {
                let items = self.items.clone();
                let character = self.character;
                let user = self.user;
                Box::pin(async move {
                tx.execute("DELETE FROM game.trade_items WHERE trade_id IN (SELECT trade_id FROM game.trades WHERE char_a = $1 OR char_b = $1)", &[&character]).await?;
                tx.execute("DELETE FROM game.trades WHERE char_a = $1 OR char_b = $1", &[&character]).await?;
                tx.execute("DELETE FROM game.plex_vault_transactions WHERE user_id = $1", &[&user]).await?;
                tx.execute("DELETE FROM game.items WHERE owner_id = $1 OR item_id = ANY($2)", &[&character, &&items[..]]).await?;
                tx.execute("DELETE FROM game.jump_clones WHERE owner_id = $1", &[&character]).await?;
                tx.execute("DELETE FROM game.characters WHERE user_id = $1", &[&user]).await?;
                tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&user]).await?;
                Ok(())
                })
            },
            |(), ()| async move { Ok(()) },
        ).await
    }
}

#[cfg(any(feature = "tests-inventory", feature = "tests-slash"))]
pub(crate) async fn ledger_references(db: &Db, user: i64) -> Result<Vec<Option<i64>>, db::Error> {
    Ok(db.query("SELECT reference FROM game.plex_vault_transactions WHERE user_id = $1 ORDER BY transaction_id", &[&user]).await?.iter().map(|row| row.get("reference")).collect())
}
#[cfg(feature = "tests-inventory")]
pub(crate) async fn store_purchase_count(db: &Db, user: i64, offer: i32) -> Result<i64, db::Error> {
    Ok(db.query_one("SELECT count(*) AS total FROM game.store_purchases WHERE user_id = $1 AND offer_id = $2", &[&user, &offer]).await?.get("total"))
}

#[cfg(feature = "tests-inventory")]
pub(crate) async fn cleanup_store(db: &Db, user: i64) -> Result<(), db::Error> {
    db.execute("DELETE FROM game.store_purchases WHERE user_id = $1", &[&user]).await?;
    db.execute("DELETE FROM game.redeem_tokens WHERE user_id = $1", &[&user]).await?;
    Ok(())
}

#[cfg(feature = "tests-storage")]
#[tokio::test]
#[ignore = "requires the test database"]
async fn opening_guard_exact_changes_and_rollback() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let result: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        let user: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role, plex_balance) VALUES ('vault-test-' || nextval('game.accounts_user_id_seq'), $1, 0, 999999999999999999999999999999999999) RETURNING user_id", &[&&b"fixture"[..]]).await.map_err(crate::services::db_err)?.get("user_id");
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'vault-test-' || nextval('game.characters_character_id_seq'), 1, 1, 1, 1) RETURNING character_id", &[&user]).await.map_err(crate::services::db_err)?.get("character_id");
        let context = VaultContext { kind: VaultKind::GmAdd, item_id: None, reference: None };
        assert!(change_in(tx, outbox, user, Some(character), VaultChange::Add(1), context, VaultAudience::Account).await?.is_none());
        let entry = change_in(tx, outbox, user, Some(character), VaultChange::Add(-1), context, VaultAudience::Account).await?.unwrap();
        assert_eq!(entry.balance, 999_999_999_999_999_999_999_999_999_999_999_998);
        assert!(change_in(tx, outbox, user, Some(character), VaultChange::Add(1), context, VaultAudience::Account).await?.is_some());
        let fill = VaultContext { kind: VaultKind::MarketFill { order_id: 7, trade_id: 9 }, item_id: None, reference: None };
        let refund = VaultContext { kind: VaultKind::MarketRefund { order_id: 7 }, item_id: None, reference: None };
        assert!(change_in(tx, outbox, user, Some(character), VaultChange::Add(-1), fill, VaultAudience::Account).await?.is_some());
        assert!(change_in(tx, outbox, user, Some(character), VaultChange::Add(1), refund, VaultAudience::Account).await?.is_some());
        let rows = tx.query("SELECT kind, order_id, trade_id FROM game.plex_vault_transactions WHERE user_id = $1 AND order_id = 7 ORDER BY transaction_id", &[&user]).await.map_err(crate::services::db_err)?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<_, String>("kind"), "market_fill");
        assert_eq!(rows[0].get::<_, Option<i64>>("trade_id"), Some(9));
        assert_eq!(rows[1].get::<_, String>("kind"), "market_refund");
        assert_eq!(rows[1].get::<_, Option<i64>>("trade_id"), None);
        let row = tx.query_one("SELECT count(*) AS total, count(*) FILTER (WHERE kind = 'opening') AS openings FROM game.plex_vault_transactions WHERE user_id = $1", &[&user]).await.map_err(crate::services::db_err)?;
        assert_eq!(row.get::<_, i64>("total"), 5);
        assert_eq!(row.get::<_, i64>("openings"), 1);
        Err(CallError::Internal(anyhow::anyhow!("rollback fixture")))
    }), |(), outbox: Outbox| async move {
        assert!(outbox.into_entries().iter().any(|e| matches!(e, Effect::Notification(Notification::Pilot { .. }))));
        Ok(())
    }).await;
    assert!(result.is_err());
}

#[cfg(feature = "tests-storage")]
#[tokio::test]
#[ignore = "requires the test database"]
async fn logged_reads_append_returning_and_statistics() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = ConversionFixture::create(&db, 60000001, "0").await.unwrap();
    let other = ConversionFixture::create(&db, 60000001, "0").await.unwrap();
    let user = fixture.user;
    // Empty account: aggregate fallbacks yield zero sums, zero count, no earliest.
    let empty = statistics(&db, user).await.unwrap();
    assert_eq!(empty.incomes, "0");
    assert_eq!(empty.expenses, "0");
    assert_eq!(empty.transactions_count, 0);
    assert_eq!(empty.earliest_created, None);
    assert!(logged_ids(&db, user).await.unwrap().is_empty());
    // The opening baseline is internal and never reaches the read APIs.
    opening(&db, user, "10", 50).await.unwrap();
    assert!(logged_ids(&db, user).await.unwrap().is_empty());
    // Signed plus zero deltas; gain and loss share one created to pin tie ordering.
    let gain = append(&db, user, None, "5", "15", "gm_add", 200, None, None, None, None).await.unwrap();
    let loss = append(&db, user, None, "-3", "12", "gm_remove", 200, None, None, None, None).await.unwrap();
    let zero = append(&db, user, None, "0", "12", "gm_add", 100, None, None, None, None).await.unwrap();
    assert!(gain > 0 && gain < loss && loss < zero);
    let foreign = append(&db, other.user, None, "7", "7", "gm_add", 300, None, None, None, None).await.unwrap();
    // A wider-than-i64 ledger row proves delta text stays exact end to end.
    let big = append(&db, other.user, None, "12345678901234567890123456789012345", "12345678901234567890123456789012345", "gm_add", 400, None, None, None, None).await.unwrap();
    assert_eq!(ledger_counts(&db, user).await, (4, 1));
    // IDs list nonopening rows only, created DESC then transaction_id DESC.
    assert_eq!(logged_ids(&db, user).await.unwrap(), vec![loss, gain, zero]);
    assert_eq!(logged_ids(&db, other.user).await.unwrap(), vec![big, foreign]);
    // Detail returns exact numeric text and is scoped to the owning user.
    let row = logged_transaction(&db, user, gain).await.unwrap().unwrap();
    assert_eq!(row.created, 200);
    assert_eq!(row.delta, "5");
    assert_eq!(row.balance_after, "15");
    let row = logged_transaction(&db, user, loss).await.unwrap().unwrap();
    assert_eq!(row.delta, "-3");
    assert_eq!(row.balance_after, "12");
    let row = logged_transaction(&db, user, zero).await.unwrap().unwrap();
    assert_eq!(row.created, 100);
    assert_eq!(row.delta, "0");
    assert_eq!(row.balance_after, "12");
    let row = logged_transaction(&db, other.user, big).await.unwrap().unwrap();
    assert_eq!(row.delta, "12345678901234567890123456789012345");
    assert_eq!(row.balance_after, "12345678901234567890123456789012345");
    assert_eq!(row.created, 400);
    // Missing, foreign-owned and opening rows all read as absent.
    assert!(logged_transaction(&db, user, i64::MAX).await.unwrap().is_none());
    assert!(logged_transaction(&db, user, foreign).await.unwrap().is_none());
    assert!(logged_transaction(&db, other.user, gain).await.unwrap().is_none());
    let baseline = opening_id(&db, user).await.unwrap();
    assert!(logged_transaction(&db, user, baseline).await.unwrap().is_none());
    // Statistics exclude opening, count the zero delta and keep signed exact sums.
    let stats = statistics(&db, user).await.unwrap();
    assert_eq!(stats.incomes, "5");
    assert_eq!(stats.expenses, "-3");
    assert_eq!(stats.transactions_count, 3);
    assert_eq!(stats.earliest_created, Some(100));
    let stats = statistics(&db, other.user).await.unwrap();
    assert_eq!(stats.incomes, "12345678901234567890123456789012352");
    assert_eq!(stats.expenses, "0");
    assert_eq!(stats.transactions_count, 2);
    assert_eq!(stats.earliest_created, Some(300));
    fixture.cleanup(&db).await.unwrap();
    other.cleanup(&db).await.unwrap();
}
