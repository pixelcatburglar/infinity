//! Wide market quantities across order mutations and price history.
use super::*;
use crate::{dao::market::{listings, orders::{self, CloseReason, MarketScope, Order}, trading::{self, NewTransaction}, versions}, isk::IskAmount, services::tests::{client, fixture}};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn wide_orders_and_history_preserve_exact_quantities() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, b) = isolated_stations(&db, &probe.sde).await;
    let person = fixture::create(&db).await.unwrap();
    let work = db.clone();
    let result = tokio::spawn(async move {
        let now = filetime_now();
        let price = IskAmount::parse_decimal("1").unwrap();
        for quantity in [1_i64 << 31, 1_i64 << 32, (1_i64 << 53) + 1, i64::MAX] {
            let mut order = Order { id: orders::next_order_id(&work).await.unwrap(), type_id: TYPE_PLEX, char_id: Some(person.character), at: a,
                corporation_id: None, wallet_key: 1000, source_location_id: None, source_flag_id: None, price,
                vol_entered: quantity, vol_remaining: quantity, min_volume: quantity, bid: false, range: -1, duration: 30,
                issued: Some(now), escrow: IskAmount::ZERO, scope: MarketScope::GlobalPlex, vault_user_id: Some(person.account),
                plex_escrow: quantity, created_at: Some(now), expires_at: Some(now + FILETIME_DAY) };
            orders::insert(&work, &order).await.unwrap();
            let snapshot = versions::snapshot(&work, MarketScope::GlobalPlex, TYPE_PLEX, b.region, now, FILETIME_DAY).await.unwrap();
            let stored = snapshot.rows.iter().find(|row| row.id == order.id).unwrap();
            assert_eq!((stored.vol_entered, stored.vol_remaining, stored.min_volume, stored.plex_escrow), (quantity, quantity, quantity, quantity));
            assert_eq!(trading::consume(&work, &order, quantity - 1, IskAmount::ZERO, quantity - 1).await.unwrap(), 1);
            order.vol_remaining -= 1;
            order.plex_escrow -= 1;
            assert_eq!(trading::close(&work, &order, order.vol_remaining, IskAmount::ZERO, order.plex_escrow, 0, CloseReason::Cancelled, now).await.unwrap(), 1);
            let row = work.query_one("SELECT vol_entered,vol_remaining,min_volume,plex_escrow FROM game.market_order_history WHERE order_id=$1", &[&order.id]).await.unwrap();
            assert_eq!((row.get::<_, i64>("vol_entered"), row.get::<_, i64>("vol_remaining"), row.get::<_, i64>("min_volume"), row.get::<_, i64>("plex_escrow")), (quantity, quantity - 1, quantity, quantity - 1));
        }
        for station in [a, b] {
            trading::record(&work, &NewTransaction { type_id: TYPE_PLEX, quantity: i64::MAX, price, buyer: person.character, seller: person.character,
                station: station.id, region: station.region, date: now, buyer_account_id: None, seller_account_id: None, order_id: None,
                scope: MarketScope::GlobalPlex, buy_order_id: None, sell_order_id: None, buyer_vault_user_id: None, seller_vault_user_id: None }).await.unwrap();
        }
        let rows = listings::price_history(&work, a.region, &[TYPE_PLEX], FILETIME_DAY, now - 1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].volume, rows[0].count), (i128::from(i64::MAX) * 2, 2));
        assert_eq!(IskAmount::parse_decimal(&rows[0].average).unwrap(), price);
    }).await;
    cleanup(&db, &[person]).await;
    result.unwrap();
}
