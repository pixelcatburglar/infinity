//! Transaction rollback after real market, wallet and inventory mutations.
use super::*;
use infinity_core::consts::FLAG_HANGAR;
use crate::dao::{items::quantities::{self, QuantityChange}, market::trading::{self, NewTransaction}};
use crate::services::{tests::{client, fixture}, wallets::{journal, ownership::FinancialOwner}};
use std::{collections::BTreeMap, sync::{Arc, atomic::{AtomicBool, Ordering}}};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn failed_market_transaction_rolls_back_every_mutation_and_notifications() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(test_db.clone(), f.character).await;
        let at = market_station(c.sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some() && s.owner_id.is_some()).unwrap());
        let ty = c.sde.types.iter().find(|t| t.type_id > 0).unwrap().type_id;
        test_db.execute("UPDATE game.characters SET balance=10000000 WHERE character_id=$1", &[&f.character]).await.unwrap();
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&ty,&f.character,&at.id,&FLAG_HANGAR]).await.unwrap().get("item_id");
        let order = Order { id: orders::next_order_id(&test_db).await.unwrap(), type_id: ty, char_id: Some(f.character), corporation_id: None, wallet_key: ACCOUNT_CASH, source_location_id: None, source_flag_id: None, at, price: crate::isk::IskAmount::from_whole_isk(100000).unwrap(), vol_entered: 2, vol_remaining: 2, min_volume: 1, bid: true, range: RANGE_STATION, duration: 1, issued: Some(filetime_now()), escrow: crate::isk::IskAmount::from_whole_isk(200000).unwrap(), scope: orders::MarketScope::for_type(ty), vault_user_id: None, plex_escrow: 0, created_at: None, expires_at: None };
        let order_id = orders::insert(&test_db, &order).await.unwrap();
        let order = Order { id: order_id, ..order };
        let dispatched = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&dispatched);
        let now = filetime_now();
        let _lock = super::super::lock().await;
        let outcome: Result<(), CallError> = test_db.transaction(|tx, outbox: &mut Outbox<()>| { let order = order.clone(); Box::pin(async move {
            let owner = FinancialOwner::cash(f.character);
            let entry = journal_entry(f.character, REF_MARKET_ESCROW, f.character, OWNER_SCC, Some(order_id), None, now, 0);
            journal::debit(tx, outbox, owner, crate::isk::IskAmount::from_cents(10_000_000).unwrap(), &entry).await?;
            let account: i64 = tx.query_one("SELECT account_id FROM game.wallet_accounts WHERE owner_id = $1 AND account_key = $2", &[&f.character, &ACCOUNT_CASH]).await.map_err(|e| db_err(crate::db::Error::from(e)))?.get("account_id");
            let fill = trading::record(tx, &NewTransaction { type_id: ty, quantity: 1, price: crate::isk::IskAmount::from_whole_isk(100000).unwrap(), buyer: f.character, seller: at.owner, station: at.id, region: at.region, date: now, buyer_account_id: Some(account), seller_account_id: None, order_id: Some(order_id), scope: order.scope, buy_order_id: Some(order_id), sell_order_id: None, buyer_vault_user_id: None, seller_vault_user_id: None }).await.map_err(db_err)?;
            let sale = journal_entry(f.character, REF_MARKET_TRANSACTION, f.character, at.owner, Some(order_id), Some(fill), now, 1);
            journal::credit_sale(tx, outbox, owner, account, crate::isk::IskAmount::from_cents(10_000_000).unwrap(), crate::isk::IskAmount::from_cents(9_500_000).unwrap(), &sale).await?;
            let change = QuantityChange { type_id: ty, expected_quantity: 2, quantity: Some(1), owner_id: f.character, location_id: at.id, flag_id: FLAG_HANGAR, singleton: false };
            assert!(quantities::apply(tx, &BTreeMap::from([(item, change)])).await.map_err(db_err)?);
            assert_eq!(trading::consume(tx, &order, 1, crate::isk::IskAmount::from_whole_isk(100000).unwrap(), 0).await.map_err(db_err)?, 1);
            outbox.notify_pilot(f.character, "OnOwnOrdersChanged", vec![]);
            tx.query_one("SELECT 1 / $1::integer AS failure", &[&0_i32]).await.map_err(|e| db_err(crate::db::Error::from(e)))?;
            Ok(())
        }) }, move |(), _outbox| async move { flag.store(true, Ordering::SeqCst); Ok(()) }).await;
        fixture::assert_division_by_zero(outcome);
        assert!(!dispatched.load(Ordering::SeqCst));
        let row = test_db.query_one("SELECT c.balance::text AS balance, (SELECT quantity FROM game.items WHERE item_id=$2) AS quantity, (SELECT vol_remaining FROM game.market_orders WHERE order_id=$3) AS remaining, (SELECT escrow::text FROM game.market_orders WHERE order_id=$3) AS escrow, (SELECT count(*) FROM game.wallet_accounts WHERE owner_id=$1) AS accounts, (SELECT count(*) FROM game.wallet_journal WHERE actor_id=$1) AS journals, (SELECT count(*) FROM game.market_transactions WHERE order_id=$3) AS fills FROM game.characters c WHERE c.character_id=$1", &[&f.character,&item,&order_id]).await.unwrap();
        assert_eq!(row.get::<_, String>("balance"), "10000000.00");
        assert_eq!(row.get::<_, i64>("quantity"), 2);
        assert_eq!(row.get::<_, i64>("remaining"), 2);
        assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("escrow")).unwrap(), crate::isk::IskAmount::from_whole_isk(200000).unwrap());
        assert_eq!(row.get::<_, i64>("accounts"), 0);
        assert_eq!(row.get::<_, i64>("journals"), 0);
        assert_eq!(row.get::<_, i64>("fills"), 0);
    }).await;
    cleanup_market_fixture(&db, f).await.unwrap();
    result.unwrap();
}
