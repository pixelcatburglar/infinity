//! Isolated corporate market owners and cleanup of durable market effects.
use super::*;
use crate::services::tests::fixture::{self, Fixture};

#[derive(Clone, Copy)]
pub(super) struct CorporateFixture {
    pub issuer: Fixture,
    pub counterparty: Fixture,
    pub corporation: i64,
    pub office: i64,
    pub at: Station,
    pub type_id: i32,
}

impl CorporateFixture {
    pub async fn create(db: &Db) -> Self {
        let issuer = fixture::create(db).await.unwrap();
        let counterparty = fixture::create(db).await.unwrap();
        let (c, _peer) = crate::services::tests::client(db.clone(), issuer.character).await;
        let stations: Vec<_> = fixture::isolated_market_stations(db, &c.sde).await.unwrap().collect();
        let at = stations.iter().copied().find(|s| stations.iter().any(|other| other.id != s.id && other.system == s.system && other.region == s.region)).expect("isolated NPC system with two stations");
        let type_id = fixture::market_type_ids(&c.sde).next().expect("tradable market SDE type");
        let corporation = fixture::create_corporation(db, issuer.character, at.id).await.unwrap();
        fixture::assign_corporation(db, issuer.character, corporation, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
        // SDE type 27 is the corporate office item.
        let office = crate::dao::items::creation::rookie_ship(db, 27, corporation, at.id, 0).await.unwrap();
        fixture::create_office(db, office, corporation, at.id).await.unwrap();
        db.execute("UPDATE game.characters SET balance=10000000 WHERE character_id=ANY($1)", &[&vec![issuer.character, counterparty.character]]).await.unwrap();
        db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,10000000) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&corporation, &(ACCOUNT_CASH + 1)]).await.unwrap();
        Self { issuer, counterparty, corporation, office, at, type_id }
    }

    pub async fn fingerprint(self, db: &Db) -> String {
        fixture::market_fingerprint(db, &[self.corporation], &[self.issuer.character, self.counterparty.character]).await.unwrap()
    }

    pub async fn cleanup(self, db: &Db) {
        let people = vec![self.issuer.character, self.counterparty.character];
        let owners = vec![self.issuer.character, self.counterparty.character, self.corporation];
        db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE (kind='MarketOrderFilled' AND ((payload->'MarketOrderFilled'->>'buyer_id')::bigint=ANY($1) OR (payload->'MarketOrderFilled'->>'seller_id')::bigint=ANY($1))) OR (kind='MarketOrderPlaced' AND (payload->'MarketOrderPlaced'->>'char_id')::bigint=ANY($1)))", &[&people]).await.unwrap();
        db.execute("DELETE FROM game.progress_events WHERE (kind='MarketOrderFilled' AND ((payload->'MarketOrderFilled'->>'buyer_id')::bigint=ANY($1) OR (payload->'MarketOrderFilled'->>'seller_id')::bigint=ANY($1))) OR (kind='MarketOrderPlaced' AND (payload->'MarketOrderPlaced'->>'char_id')::bigint=ANY($1))", &[&people]).await.unwrap();
        db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=ANY($1)) OR market_transaction_id IN (SELECT transaction_id FROM game.market_transactions WHERE buyer_id=ANY($2) OR seller_id=ANY($2))", &[&owners, &people]).await.unwrap();
        db.execute("DELETE FROM game.market_transactions WHERE buyer_id=ANY($1) OR seller_id=ANY($1)", &[&people]).await.unwrap();
        db.execute("DELETE FROM game.market_orders WHERE char_id=ANY($1) OR corporation_id=$2", &[&people, &self.corporation]).await.unwrap();
        db.execute("DELETE FROM game.market_order_history WHERE char_id=ANY($1) OR corporation_id=$2", &[&people, &self.corporation]).await.unwrap();
        db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=ANY($1)", &[&owners]).await.unwrap();
        db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&self.corporation]).await.unwrap();
        db.execute("DELETE FROM game.plex_vault_transactions WHERE user_id IN (SELECT user_id FROM game.characters WHERE character_id = ANY($1::bigint[]))", &[&people]).await.unwrap();
        fixture::cleanup_corporation(db, self.corporation, &people, self.office).await.unwrap();
        fixture::cleanup(db, self.counterparty).await.unwrap();
        fixture::cleanup(db, self.issuer).await.unwrap();
    }
}
