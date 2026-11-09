use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_postgres::{types::ToSql, Row, ToStatement};

struct CountingExecutor(AtomicUsize);

impl Executor for CountingExecutor {
    async fn query<T: ?Sized + ToStatement + Sync>(&self, _: &T, _: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, crate::db::Error> { panic!("unexpected query") }
    async fn query_one<T: ?Sized + ToStatement + Sync>(&self, _: &T, _: &[&(dyn ToSql + Sync)]) -> Result<Row, crate::db::Error> { panic!("unexpected query") }
    async fn query_opt<T: ?Sized + ToStatement + Sync>(&self, _: &T, _: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>, crate::db::Error> { panic!("unexpected query") }
    async fn execute<T: ?Sized + ToStatement + Sync>(&self, _: &T, _: &[&(dyn ToSql + Sync)]) -> Result<u64, crate::db::Error> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(1)
    }
}

#[tokio::test]
async fn absolute_standing_checks_range_before_writing() {
    let db = CountingExecutor(AtomicUsize::new(0));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -10.01, 10.01] {
        assert!(set(&db, 1, 2, value).await.is_err());
    }
    assert_eq!(db.0.load(Ordering::Relaxed), 0);
    for value in [-10.0, 0.0, 10.0] {
        assert!(set(&db, 1, 2, value).await.is_ok());
    }
    assert_eq!(db.0.load(Ordering::Relaxed), 3);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn loyalty_fact_and_balance_commit_and_rollback_together() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner: i64 = db.query_one("SELECT nextval('game.characters_character_id_seq') AS id", &[]).await.unwrap().get("id");
    let issuer = 1_i64;
    let mutation = || LoyaltyMutation {
        reason: LoyaltyReason::MissionReward,
        occurred_at: crate::client::filetime_now(),
        context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Natural },
    };
    let failed: Result<(), CallError> = db.transaction(
        |tx, outbox: &mut Outbox<()>| Box::pin(async move {
            add_lp(tx, outbox, owner, issuer, 5, mutation()).await?;
            Err(CallError::Internal(anyhow::anyhow!("rollback fixture")))
        }),
        |(), _: Outbox<()>| async { Ok(()) },
    ).await;
    assert!(failed.is_err());
    assert_eq!(lp_balance(&db, owner, issuer).await.unwrap(), 0);
    assert!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", owner).await.is_empty());
    db.transaction(
        |tx, outbox: &mut Outbox<()>| Box::pin(async move {
            add_lp(tx, outbox, owner, issuer, 5, mutation()).await
        }),
        |(), _: Outbox<()>| async { Ok::<_, CallError>(()) },
    ).await.unwrap();
    assert_eq!(lp_balance(&db, owner, issuer).await.unwrap(), 5);
    let events = crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", owner).await;
    assert!(matches!(events.as_slice(), [GameEvent::LoyaltyPointsEarned { amount: 5, .. }]));
    for (amount, reason, origin, corporation, issuer_id, expected) in [
        (0, LoyaltyReason::MissionReward, FactOrigin::Natural, false, issuer, 5),
        (-2, LoyaltyReason::StorePurchase, FactOrigin::Natural, false, issuer, 3),
        (2, LoyaltyReason::GmAdjustment, FactOrigin::Gm, false, issuer, 5),
        (2, LoyaltyReason::MissionReward, FactOrigin::Qa, false, issuer, 7),
        (2, LoyaltyReason::MissionReward, FactOrigin::Gm, false, issuer, 9),
        (2, LoyaltyReason::MissionReward, FactOrigin::Natural, true, issuer, 11),
        (2, LoyaltyReason::MissionReward, FactOrigin::Natural, false, infinity_core::consts::CORP_HERALDRY, 2),
    ] {
        db.transaction(
            |tx, outbox: &mut Outbox<()>| Box::pin(async move {
                let mut m = mutation();
                m.reason = reason;
                m.context.origin = origin;
                change_lp(tx, outbox, owner, issuer_id, amount, corporation, m).await
            }),
            |(), _: Outbox<()>| async { Ok::<_, CallError>(()) },
        ).await.unwrap();
        assert_eq!(lp_balance(&db, owner, issuer_id).await.unwrap(), expected);
        assert_eq!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", owner).await.len(), 1);
    }
    for amount in [-12, i64::MAX] {
        let failed = db.transaction(
            |tx, outbox: &mut Outbox<()>| Box::pin(async move { add_lp(tx, outbox, owner, issuer, amount, mutation()).await }),
            |(), _: Outbox<()>| async { Ok::<_, CallError>(()) },
        ).await;
        assert!(failed.is_err());
        assert_eq!(lp_balance(&db, owner, issuer).await.unwrap(), 11);
        assert_eq!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", owner).await.len(), 1);
    }
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_progress(tx, &[owner], &[owner]).await?;
        tx.execute("DELETE FROM game.lp_balances WHERE char_id = $1", &[&owner]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async { Ok(()) }).await.unwrap();
}
