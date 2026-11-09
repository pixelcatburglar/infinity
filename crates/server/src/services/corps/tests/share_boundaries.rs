//! Transactional share authorization, guarded writes and startup backfill boundaries.
use super::fixtures;
use crate::{dao::corporations::shares as ledger, db::outbox::Outbox, services::corps::shares::transfer_core};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn company_and_personal_sources_use_actual_authority() {
    let mut client = fixtures::connect().await;
    let mut tx = client.transaction().await.unwrap();
    let issuer_ceo = fixtures::character(&tx).await;
    let issuer_director = fixtures::character(&tx).await;
    let foreign_ceo = fixtures::character(&tx).await;
    let foreign_director = fixtures::character(&tx).await;
    let member = fixtures::character(&tx).await;
    let outsider = fixtures::character(&tx).await;
    let target = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, issuer_ceo).await;
    let foreign = fixtures::corporation(&tx, foreign_ceo).await;
    let outsider_corp: i64 = tx.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&outsider]).await.unwrap().get("corporation_id");
    for (character, corp, roles) in [(issuer_director, issuer, infinity_core::consts::ROLE_DIRECTOR), (foreign_director, foreign, infinity_core::consts::ROLE_DIRECTOR), (member, foreign, 0)] {
        tx.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=$3 WHERE character_id=$2", &[&corp, &character, &roles]).await.unwrap();
    }
    assert!(ledger::initialize(&tx, issuer, 1000).await.unwrap());
    for (actor, bound, to, quantity, company) in [
        (issuer_ceo, issuer, foreign, 400, true),
        (issuer_director, issuer, outsider, 100, true),
        (foreign_ceo, foreign, target, 50, true),
        (foreign_director, foreign, target, 25, true),
        (outsider, outsider_corp, target, 20, false),
    ] {
        let op = ledger::operation_id(&tx).await.unwrap();
        let mut effects = Outbox::<()>::default();
        transfer_core(&tx, &mut effects, op, actor, bound, issuer, to, quantity, company, 1).await.unwrap();
        assert!(!effects.into_entries().is_empty());
    }
    assert_eq!(ledger::balance(&tx, issuer, issuer).await.unwrap(), 500);
    assert_eq!(ledger::balance(&tx, issuer, foreign).await.unwrap(), 325);
    assert_eq!(ledger::balance(&tx, issuer, outsider).await.unwrap(), 80);
    assert_eq!(ledger::balance(&tx, issuer, target).await.unwrap(), 95);
    for (actor, bound) in [(member, foreign), (foreign_director, issuer), (issuer_director, foreign)] {
        let op = ledger::operation_id(&tx).await.unwrap();
        let savepoint = tx.transaction().await.unwrap();
        let mut effects = Outbox::<()>::default();
        assert!(transfer_core(&savepoint, &mut effects, op, actor, bound, issuer, target, 1, true, 1).await.is_err());
        assert!(effects.into_entries().is_empty());
        savepoint.rollback().await.unwrap();
        let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.corp_share_transfers WHERE operation_id=$1", &[&op]).await.unwrap().get("count");
        assert_eq!(count, 0);
    }
    assert_eq!(ledger::balance(&tx, issuer, issuer).await.unwrap(), 500);
    assert_eq!(ledger::balance(&tx, issuer, foreign).await.unwrap(), 325);
    assert_eq!(ledger::balance(&tx, issuer, target).await.unwrap(), 95);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn invalid_issuers_and_targets_refuse_without_writes() {
    let mut client = fixtures::connect().await;
    let mut tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let deleted_ceo = fixtures::character(&tx).await;
    let target = fixtures::character(&tx).await;
    let deleted_target = fixtures::character(&tx).await;
    let npc: i64 = tx.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&target]).await.unwrap().get("corporation_id");
    let issuer = fixtures::corporation(&tx, ceo).await;
    let deleted_issuer = fixtures::corporation(&tx, deleted_ceo).await;
    assert!(ledger::initialize(&tx, issuer, 1000).await.unwrap());
    assert!(ledger::initialize(&tx, deleted_issuer, 1000).await.unwrap());
    tx.execute("UPDATE game.corporations SET deleted=true WHERE corporation_id=$1", &[&deleted_issuer]).await.unwrap();
    tx.execute("UPDATE game.characters SET deleted=true WHERE character_id=$1", &[&deleted_target]).await.unwrap();
    let missing: i64 = tx.query_one("SELECT nextval('game.characters_character_id_seq') AS id", &[]).await.unwrap().get("id");
    let missing_issuer: i64 = tx.query_one("SELECT nextval('game.corporations_corporation_id_seq') AS id", &[]).await.unwrap().get("id");
    for (bad_issuer, destination) in [(missing_issuer, target), (deleted_issuer, target), (npc, target), (issuer, missing), (issuer, deleted_target)] {
        let op = ledger::operation_id(&tx).await.unwrap();
        let savepoint = tx.transaction().await.unwrap();
        let mut effects = Outbox::<()>::default();
        assert!(transfer_core(&savepoint, &mut effects, op, ceo, issuer, bad_issuer, destination, 1, true, 1).await.is_err());
        assert!(effects.into_entries().is_empty());
        savepoint.rollback().await.unwrap();
        let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.corp_share_transfers WHERE operation_id=$1", &[&op]).await.unwrap().get("count");
        assert_eq!(count, 0);
    }
    for bad_target in [missing, deleted_target] {
        assert!(!ledger::transfer(&tx, issuer, issuer, bad_target, 1).await.unwrap());
    }
    assert_eq!(ledger::balance(&tx, issuer, issuer).await.unwrap(), 1000);
    assert_eq!(ledger::balance(&tx, issuer, target).await.unwrap(), 0);
    assert_eq!(ledger::balance(&tx, deleted_issuer, deleted_issuer).await.unwrap(), 1000);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn ledger_rejects_overflow_underflow_and_invalid_quantities() {
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let target = fixtures::character(&tx).await;
    let empty_target = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(ledger::initialize(&tx, issuer, 1000).await.unwrap());
    // Deliberately inconsistent total isolates the target's bigint overflow guard.
    tx.execute("INSERT INTO game.corp_shares (corporation_id,shareholder_id,shares) VALUES ($1,$2,$3)", &[&issuer, &target, &(i64::MAX - 1)]).await.unwrap();
    for (to, quantity) in [(target, 2), (empty_target, 1001), (issuer, 1), (empty_target, 0), (empty_target, -1)] {
        assert!(!ledger::transfer(&tx, issuer, issuer, to, quantity).await.unwrap());
        assert_eq!(ledger::balance(&tx, issuer, issuer).await.unwrap(), 1000);
        assert_eq!(ledger::balance(&tx, issuer, target).await.unwrap(), i64::MAX - 1);
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn backfill_includes_deleted_once_and_does_not_restore_treasury() {
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let deleted_ceo = fixtures::character(&tx).await;
    let holder = fixtures::character(&tx).await;
    let active = fixtures::corporation(&tx, ceo).await;
    let deleted = fixtures::corporation(&tx, deleted_ceo).await;
    tx.execute("UPDATE game.corporations SET deleted=true WHERE corporation_id=$1", &[&deleted]).await.unwrap();
    assert!(ledger::initialize_existing(&tx, 1000).await.unwrap().is_some());
    for corp in [active, deleted] {
        assert_eq!(ledger::issued_total(&tx, corp).await.unwrap(), Some(1000));
        assert_eq!(ledger::balance(&tx, corp, corp).await.unwrap(), 1000);
    }
    assert!(ledger::transfer(&tx, active, active, holder, 1000).await.unwrap());
    assert!(ledger::initialize_existing(&tx, 2000).await.unwrap().is_some());
    assert_eq!(ledger::issued_total(&tx, active).await.unwrap(), Some(1000));
    assert_eq!(ledger::issued_total(&tx, deleted).await.unwrap(), Some(1000));
    assert_eq!(ledger::balance(&tx, active, active).await.unwrap(), 0);
    assert_eq!(ledger::balance(&tx, active, holder).await.unwrap(), 1000);
    assert_eq!(ledger::balance(&tx, deleted, deleted).await.unwrap(), 1000);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn external_holdings_block_all_null_backfill_even_at_zero() {
    let mut client = fixtures::connect().await;
    let mut tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let other_ceo = fixtures::character(&tx).await;
    let holder = fixtures::character(&tx).await;
    let inconsistent = fixtures::corporation(&tx, ceo).await;
    let untouched = fixtures::corporation(&tx, other_ceo).await;
    for shares in [0_i64, 1] {
        let savepoint = tx.transaction().await.unwrap();
        savepoint.execute("INSERT INTO game.corp_shares (corporation_id,shareholder_id,shares) VALUES ($1,$2,$3)", &[&inconsistent, &holder, &shares]).await.unwrap();
        let before = savepoint.query("SELECT c.corporation_id, s.shareholder_id, s.shares FROM game.corporations c LEFT JOIN game.corp_shares s ON s.corporation_id=c.corporation_id WHERE c.issued_shares IS NULL ORDER BY c.corporation_id,s.shareholder_id", &[]).await.unwrap().iter().map(|r| (r.get::<_, i64>("corporation_id"), r.get::<_, Option<i64>>("shareholder_id"), r.get::<_, Option<i64>>("shares"))).collect::<Vec<_>>();
        assert!(ledger::initialize_existing(&savepoint, 1000).await.unwrap().is_none());
        let after = savepoint.query("SELECT c.corporation_id, s.shareholder_id, s.shares FROM game.corporations c LEFT JOIN game.corp_shares s ON s.corporation_id=c.corporation_id WHERE c.issued_shares IS NULL ORDER BY c.corporation_id,s.shareholder_id", &[]).await.unwrap().iter().map(|r| (r.get::<_, i64>("corporation_id"), r.get::<_, Option<i64>>("shareholder_id"), r.get::<_, Option<i64>>("shares"))).collect::<Vec<_>>();
        assert_eq!(before, after);
        assert_eq!(ledger::issued_total(&savepoint, inconsistent).await.unwrap(), None);
        assert_eq!(ledger::issued_total(&savepoint, untouched).await.unwrap(), None);
        savepoint.rollback().await.unwrap();
    }
    tx.rollback().await.unwrap();
}
