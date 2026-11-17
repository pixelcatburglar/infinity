use super::*;
use crate::db::outbox::{self, Outbox};
use crate::services::tests::fixture;
use crate::services::CallError;

const AGENT_CORP: i64 = 1000001;
const AGENT_FACTION: i64 = 500001;

#[test]
fn operation_validation_rejects_out_of_range_before_any_write() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -10.01, 10.01] {
        assert!(!valid(Operation::Set(value)), "{value} must be refused before any write");
    }
    for value in [-10.0, 0.0, 10.0] {
        assert!(valid(Operation::Set(value)));
    }
    for rate in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!valid(Operation::TowardLimit(rate)), "{rate} must be refused before any write");
    }
    for rate in [-10.0, 0.0, 10.0] {
        assert!(valid(Operation::TowardLimit(rate)));
    }
}

#[test]
fn mission_rate_scales_gains_and_preserves_penalties() {
    assert_eq!(mission_rate(0.5, 1.25), 0.625);
    for rate in [0.0, -0.5] {
        assert_eq!(mission_rate(rate, 1.25), rate, "zero and negative rates must not scale");
    }
    assert!(mission_rate(0.5, f64::NAN).is_nan());
    assert_eq!(mission_rate(0.5, f64::INFINITY), f64::INFINITY);
}

fn slash(actor: i64) -> Mutation<'static> {
    Mutation { occurred_at: crate::client::filetime_now(), cause: Cause::Slash { actor, reason: "" } }
}

/// Run `apply_tx` inside a real transaction; returns its changed count and the
/// number of `OnStandingSet` pilot notifications the outbox carried.
async fn applied(db: &crate::db::Db, character: i64, changes: &[Change], mutation: Mutation<'static>) -> Result<(i64, usize), CallError> {
    // Transaction closures must satisfy a 'static bound on retry; an Arc hands
    // each attempt a handle to the one change list.
    let changes = std::sync::Arc::new(changes.to_vec());
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let changes = changes.clone();
            Box::pin(async move { apply_tx(tx, outbox, character, &changes, mutation).await })
        },
        |changed, outbox| async move {
            let (effects, _) = outbox::tests::split(outbox);
            let notifications = effects.iter().filter(|e| matches!(e, outbox::Effect::Notification(outbox::Notification::Pilot { event, .. }) if event == "OnStandingSet")).count();
            Ok::<_, CallError>((changed, notifications))
        },
    ).await
}

/// A history row for `from`, newest first.
async fn journal(db: &crate::db::Db, character: i64, from: i64) -> Vec<(f64, f64)> {
    crate::dao::characters::standings::history::list(db, character, from).await.unwrap().iter().map(|r| (r.old_standing, r.new_standing)).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn multi_owner_batch_rolls_back_completely() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let changes = [Change { from_id: AGENT_FACTION, operation: Operation::Set(6.0) }, Change { from_id: AGENT_CORP, operation: Operation::Set(5.0) }];
    let mut dispatched = 0_usize;
    let result: Result<(), CallError> = db.transaction(
        |tx, outbox: &mut Outbox| {
            // `changes` is a Copy stack array; each retry owns its own copy.
            let changes = changes;
            Box::pin(async move {
                apply_tx(tx, outbox, f.character, &changes, slash(f.character)).await?;
                Err(CallError::Internal(anyhow::anyhow!("forced rollback after writes")))
            })
        },
        |(), outbox| {
            dispatched += 1;
            let (effects, _) = outbox::tests::split(outbox);
            assert!(effects.is_empty(), "rolled-back notifications must never dispatch");
            async { Ok::<_, CallError>(()) }
        },
    ).await;
    assert!(matches!(result, Err(CallError::Internal(_))));
    assert_eq!(dispatched, 0);
    assert_eq!(super::super::standing(&db, f.character, AGENT_CORP).await.unwrap(), 0.0);
    assert_eq!(super::super::standing(&db, f.character, AGENT_FACTION).await.unwrap(), 0.0);
    assert!(journal(&db, f.character, AGENT_CORP).await.is_empty());
    assert!(journal(&db, f.character, AGENT_FACTION).await.is_empty());
    db.execute("DELETE FROM game.standings WHERE char_id = $1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn concurrent_set_and_toward_limit_chain_through_history() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let one = applied(&db, f.character, &[Change { from_id: AGENT_CORP, operation: Operation::Set(4.0) }], slash(f.character));
    let two = applied(&db, f.character, &[Change { from_id: AGENT_CORP, operation: Operation::TowardLimit(0.5) }], slash(f.character));
    let (first, second) = tokio::join!(one, two);
    for result in [first, second] {
        let (changed, notifications) = result.unwrap();
        assert_eq!(changed, 1);
        assert_eq!(notifications, 1);
    }
    let events = journal(&db, f.character, AGENT_CORP).await;
    assert_eq!(events.len(), 2);
    // Each row's old_standing is the previous row's new_standing: the history
    // chain proves the two writers serialized on the character lock.
    assert_eq!(events[0].0, events[1].1);
    let (old_first, new_first) = events[1];
    assert_eq!(old_first, 0.0);
    // If the absolute set landed first, the rate moved from 4.0; otherwise the
    // rate moved first and the set overwrote it with 4.0.
    let expected = if new_first == 4.0 { new_first + (STANDING_LIMIT - new_first) * 0.5 } else { 4.0 };
    assert_eq!(events[0].1, expected);
    assert_eq!(super::super::standing(&db, f.character, AGENT_CORP).await.unwrap(), expected);
    db.execute("DELETE FROM game.standings WHERE char_id = $1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}
