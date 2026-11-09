//! Database-backed tax settlement boundaries and transactional rollback.
use super::*;
use infinity_core::consts::ACCOUNT_CASH;
use crate::{db::Db, services::{tests::fixture, wallets}};

fn mutation(reason: crate::lua::events::resources::LoyaltyReason, origin: crate::lua::events::resources::FactOrigin) -> crate::services::standings::LoyaltyMutation {
    crate::services::standings::LoyaltyMutation {
        reason, occurred_at: crate::client::filetime_now(),
        context: crate::lua::events::resources::FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin },
    }
}

async fn settle(db: &Db, recipient: i64, gross: f64, lp: i64, issuer: i64) -> Result<Outbox<()>, CallError> {
    db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
        settle_reward(tx, outbox, recipient, gross, Some(LoyaltyReward { issuer, amount: lp, mutation: mutation(crate::lua::events::resources::LoyaltyReason::MissionReward, crate::lua::events::resources::FactOrigin::Natural) })).await
    }), |(), outbox| async move { Ok(outbox) }).await
}
async fn reset(db: &Db, member: i64, corp: i64, issuer: i64) {
    db.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&member]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.lp_balances WHERE char_id=ANY($1) AND corp_id=$2", &[&vec![member,corp], &issuer]).await.unwrap();
}
async fn cash(db: &Db, member: i64, corp: i64) -> (crate::isk::IskAmount, crate::isk::IskAmount) {
    (wallets::balance(db, member).await.unwrap(), wallets::corp_balance(db, corp, ACCOUNT_CASH).await.unwrap())
}
fn cents(value: i128) -> crate::isk::IskAmount { crate::isk::IskAmount::from_cents(value).unwrap() }
async fn lp(db: &Db, owner: i64, issuer: i64) -> i64 {
    crate::dao::wallets::loyalty::balance(db, owner, issuer).await.unwrap().unwrap_or(0)
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn taxable_reward_boundaries_exact_lp_and_rollback() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, member.character, 60000001).await.unwrap();
    let issuer = 1000001i64;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        db.execute("UPDATE game.characters SET corporation_id=$2 WHERE character_id=$1", &[&member.character, &corp]).await.unwrap();
        db.execute("UPDATE game.corporations SET tax_rate=0.1,lp_tax_rate=0.1 WHERE corporation_id=$1", &[&corp]).await.unwrap();
        for (gross, personal, corporate) in [(99999.99,9_999_999,0), (100000.0,10_000_000,0), (100000.01,9_000_001,1_000_000), (200000.0,18_000_000,2_000_000)] {
            reset(&db, member.character, corp, issuer).await;
            let before = crate::db::outbox::tests::wallet_events(&db, member.character).await.len();
            settle(&db, member.character, gross, 0, issuer).await.unwrap();
            let events = crate::db::outbox::tests::wallet_events(&db, member.character).await;
            assert_eq!(events.len(), before + 1);
            assert!(matches!(&events[before], crate::lua::events::GameEvent::WalletChanged { owner_id, delta, .. } if *owner_id == member.character && *delta == cents(personal)));
            assert_eq!(cash(&db, member.character, corp).await, (cents(personal), cents(corporate)));
        }
        reset(&db, member.character, corp, issuer).await;
        db.execute("UPDATE game.corporations SET tax_rate=0 WHERE corporation_id=$1", &[&corp]).await.unwrap();
        db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
            settle_whole_reward(tx, outbox, member.character, 9_007_199_254_740_993, None).await
        }), |(), _| async { Ok(()) }).await.unwrap();
        assert_eq!(cash(&db, member.character, corp).await, (cents(900_719_925_474_099_300), cents(0)));
        db.execute("UPDATE game.corporations SET tax_rate=0.1 WHERE corporation_id=$1", &[&corp]).await.unwrap();
        reset(&db, member.character, corp, issuer).await;
        for _ in 0..2 { settle(&db, member.character, 60000.0, 0, issuer).await.unwrap(); }
        assert_eq!(cash(&db, member.character, corp).await, (cents(12_000_000), cents(0)));
        for (amount, tax) in [(9,0), (10,1), (19,1)] {
            reset(&db, member.character, corp, issuer).await;
            settle(&db, member.character, 0.0, amount, issuer).await.unwrap();
            assert_eq!((lp(&db, member.character, issuer).await, lp(&db, corp, issuer).await), (amount-tax,tax));
        }
        for rate in [0.0f64, 1.0] {
            reset(&db, member.character, corp, issuer).await;
            db.execute("UPDATE game.corporations SET tax_rate=$2,lp_tax_rate=$2 WHERE corporation_id=$1", &[&corp, &rate]).await.unwrap();
            settle(&db, member.character, 200000.0, 10, issuer).await.unwrap();
            assert_eq!(cash(&db, member.character, corp).await, if rate == 0.0 { (cents(20_000_000), cents(0)) } else { (cents(0), cents(20_000_000)) });
            assert_eq!((lp(&db, member.character, issuer).await, lp(&db, corp, issuer).await), ((10.0*(1.0-rate)) as i64, (10.0*rate) as i64));
            reset(&db, member.character, corp, issuer).await;
            settle(&db, member.character, 100000.0, 0, issuer).await.unwrap();
            assert_eq!(cash(&db, member.character, corp).await, (cents(10_000_000), cents(0)));
        }
        reset(&db, member.character, corp, issuer).await;
        db.execute("UPDATE game.corporations SET tax_rate=0.1,lp_tax_rate=0.5 WHERE corporation_id=$1", &[&corp]).await.unwrap();
        settle(&db, member.character, 0.0, i64::MAX, issuer).await.unwrap();
        assert_eq!((lp(&db, member.character, issuer).await, lp(&db, corp, issuer).await), (4611686018427387904,4611686018427387903));
        reset(&db, member.character, corp, issuer).await;
        db.execute("UPDATE game.corporations SET lp_tax_rate='NaN'::float8 WHERE corporation_id=$1", &[&corp]).await.unwrap();
        assert!(settle(&db, member.character, 200000.0, 10, issuer).await.is_err());
        assert_eq!(cash(&db, member.character, corp).await, (cents(0), cents(0)));
        assert_eq!((lp(&db, member.character, issuer).await, lp(&db, corp, issuer).await), (0,0));
        db.execute("UPDATE game.corporations SET lp_tax_rate=0.1 WHERE corporation_id=$1", &[&corp]).await.unwrap();
        db.execute("INSERT INTO game.lp_balances(char_id,corp_id,amount) VALUES ($1,$2,$3)", &[&corp, &issuer, &i64::MAX]).await.unwrap();
        let dispatched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = dispatched.clone();
        let error = db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
            settle_reward(tx, outbox, member.character, 200000.0, Some(LoyaltyReward { issuer, amount: 10, mutation: mutation(crate::lua::events::resources::LoyaltyReason::MissionReward, crate::lua::events::resources::FactOrigin::Natural) })).await
        }), move |(), _| async move { flag.store(true, std::sync::atomic::Ordering::SeqCst); Ok(()) }).await;
        assert!(error.is_err());
        assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(cash(&db, member.character, corp).await, (cents(0), cents(0)));
        assert_eq!(lp(&db, member.character, issuer).await, 0);
        assert_eq!(lp(&db, corp, issuer).await, i64::MAX);
        reset(&db, member.character, corp, issuer).await;
        db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&member.character]).await.unwrap();
        settle(&db, member.character, 200000.0, 10, issuer).await.unwrap();
        assert_eq!(cash(&db, member.character, corp).await, (cents(20_000_000), cents(0)));
        assert_eq!(lp(&db, member.character, issuer).await, 10);
        reset(&db, member.character, corp, issuer).await;
        db.execute("UPDATE game.characters SET corporation_id=$2 WHERE character_id=$1", &[&member.character, &corp]).await.unwrap();
        db.execute("UPDATE game.corporations SET deleted=true WHERE corporation_id=$1", &[&corp]).await.unwrap();
        settle(&db, member.character, 200000.0, 10, issuer).await.unwrap();
        assert_eq!(cash(&db, member.character, corp).await, (cents(20_000_000), cents(0)));
        assert_eq!((lp(&db, member.character, issuer).await, lp(&db, corp, issuer).await), (10,0));
        assert_eq!(crate::dao::wallets::loyalty::balance(&db, member.character, issuer).await.unwrap(), Some(10));
        reset(&db, member.character, corp, issuer).await;
        db.execute("UPDATE game.corporations SET deleted=false WHERE corporation_id=$1", &[&corp]).await.unwrap();
        db.execute("UPDATE game.characters SET corporation_id=$2 WHERE character_id=$1", &[&member.character, &corp]).await.unwrap();
        db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
            wallets::credit(tx, outbox, member.character, cents(20_000_000)).await?;
            crate::services::standings::add_lp(tx, outbox, member.character, issuer, 10, mutation(crate::lua::events::resources::LoyaltyReason::GmAdjustment, crate::lua::events::resources::FactOrigin::Gm)).await
        }), |(), _| async { Ok(()) }).await.unwrap();
        assert_eq!(cash(&db, member.character, corp).await, (cents(20_000_000), cents(0)));
        assert_eq!((lp(&db, member.character, issuer).await, lp(&db, corp, issuer).await), (10,0));
    }).await;
    reset(&db, member.character, corp, issuer).await;
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        fixture::cleanup_progress(tx, &[corp], &[corp]).await
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&member.character]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, member).await.unwrap();
    result.unwrap();
}
