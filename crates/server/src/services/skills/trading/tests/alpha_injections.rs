//! Focused real-SDE behavioral coverage for the daily Alpha injector.
use super::*;
use crate::{client::filetime_now, config::tests::test_database, db::Db, messages, services::{skills::alpha_injections, tests::{client, fixture}}, consts::FLAG_HANGAR};
use marshal::Value;
use std::sync::Arc;

const CONTAINED_POINTS_ATTRIBUTE: i32 = 2461;
const DAY: i64 = crate::consts::FILETIME_DAY;

async fn alpha_fixture() -> (Db, fixture::Fixture, crate::client::Client, i64, i64) {
    crate::tests::init_config();
    let db = Db::connect(&test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    db.execute("UPDATE game.account_clone_grades SET grade=0, omega_until=NULL, grace_until=NULL WHERE user_id=(SELECT user_id FROM game.characters WHERE character_id=$1)", &[&f.character]).await.unwrap();
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    let item = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&alpha_injections::TYPE_ID, &f.character, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
    let (c, _) = client(db.clone(), f.character).await;
    (db, f, c, station, item)
}

async fn cleanup(db: &Db, f: fixture::Fixture) {
    db.execute("DELETE FROM game.jobs WHERE kind=$1 AND job_id LIKE $2", &[&alpha_injections::KIND, &format!("alpha-injection-available:{}:%", f.character)]).await.unwrap();
    db.execute("DELETE FROM game.character_alpha_injections WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn alpha_injector_uses_full_sde_points_without_cap_or_booster_adjustment() {
    let (db, f, mut c, _, item) = alpha_fixture().await;
    let points = c.sde.types.attribute(alpha_injections::TYPE_ID, CONTAINED_POINTS_ATTRIBUTE).expect("SDE contained points");
    assert!(points.is_finite() && points > 0.0 && points.fract() == 0.0);
    assert_eq!(quote(&c, f.character, &[Value::Int(i64::from(alpha_injections::TYPE_ID)), Value::Int(1)]).await.unwrap(), Value::Int(points as i64));
    let before = crate::dao::characters::attributes::free_points(&db, f.character).await.unwrap();
    inject(&mut c, f.character, &[Value::Int(item), Value::Int(1)], false).await.unwrap();
    let after = crate::dao::characters::attributes::free_points(&db, f.character).await.unwrap();
    assert_eq!(after - before, points as i64);
    assert_eq!(crate::dao::trades::reserved(&db, &[item]).await.unwrap().len(), 0);
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn alpha_injector_qty_one_and_check_are_non_mutating() {
    let (db, f, mut c, _, item) = alpha_fixture().await;
    let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
    assert!(inject(&mut c, f.character, &[Value::Int(item), Value::Int(1)], true).await.is_ok());
    assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
    assert!(inject(&mut c, f.character, &[Value::Int(item), Value::Int(2)], true).await.is_err());
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn alpha_injector_rolling_deadline_is_exact_before_at_and_after() {
    let (db, f, _c, _, _) = alpha_fixture().await;
    let now = filetime_now();
    let deadline = now + DAY;
    crate::dao::skills::alpha_injections::claim(&db, f.character, now, deadline).await.unwrap();
    assert!(alpha_injections::check_tx(&db, f.character, crate::services::subscriptions::rules::ALPHA, deadline - 1).await.is_err());
    assert_eq!(alpha_injections::check_tx(&db, f.character, crate::services::subscriptions::rules::ALPHA, deadline).await.unwrap(), deadline + DAY);
    assert_eq!(alpha_injections::check_tx(&db, f.character, crate::services::subscriptions::rules::ALPHA, deadline + 1).await.unwrap(), deadline + 1 + DAY);
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn alpha_injector_refuses_effective_omega_with_custom_notify() {
    let (db, f, mut c, _, item) = alpha_fixture().await;
    db.execute("UPDATE game.account_clone_grades SET grade=1, omega_until=NULL, grace_until=NULL WHERE user_id=(SELECT user_id FROM game.characters WHERE character_id=$1)", &[&f.character]).await.unwrap();
    assert!(matches!(inject(&mut c, f.character, &[Value::Int(item), Value::Int(1)], false).await, Err(crate::services::CallError::User { msg, dict }) if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::skills::ALPHA_INJECTOR_REQUIRES_ALPHA))]));
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn alpha_injector_claim_is_atomic_under_concurrency_and_persists_reconnect() {
    let (db, f, _c, _, _) = alpha_fixture().await;
    let now = filetime_now();
    let deadline = now + DAY;
    let (a, b) = tokio::join!(alpha_injections::claim_tx(&db, f.character, now, deadline), alpha_injections::claim_tx(&db, f.character, now, deadline));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(crate::dao::skills::alpha_injections::next(&db, f.character).await.unwrap(), deadline);
    let reopened = Db::connect(&test_database()).await.unwrap();
    assert_eq!(crate::dao::skills::alpha_injections::next(&reopened, f.character).await.unwrap(), deadline);
    cleanup(&reopened, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn alpha_injector_availability_handler_notifies_once_for_stale_replay() {
    let (db, f, c, _, _) = alpha_fixture().await;
    let now = filetime_now();
    let expected = now - 1;
    crate::dao::skills::alpha_injections::claim(&db, f.character, expected - DAY, expected).await.unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let job = crate::scheduler::Job { id: format!("{}:{}:{}", alpha_injections::KIND, f.character, expected), kind: alpha_injections::KIND.into(), due_at: expected, payload: serde_json::json!({"char_id":f.character,"expected":expected}).to_string() };
    alpha_injections::handle(db.clone(), Arc::clone(&c.world), job.clone()).await.unwrap();
    alpha_injections::handle(db.clone(), Arc::clone(&c.world), job).await.unwrap();
    let mut availability_count = 0;
    while let Ok((event, _)) = receiver.try_recv() {
        if event == "OnAlphaInjectionAvailable" {
            availability_count += 1;
        }
    }
    assert_eq!(availability_count, 1);
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn alpha_injector_malformed_sde_case_requires_external_fixture() {
    // The production SDE loader is intentionally read-only; this test records that a malformed
    // native SDE row cannot be created without importing client data or adding a test hook.
    let (db, f, c, _, _) = alpha_fixture().await;
    assert!(c.sde.types.attribute(alpha_injections::TYPE_ID, CONTAINED_POINTS_ATTRIBUTE).is_some(), "malformed-SDE source unavailable in repository test database");
    cleanup(&db, f).await;
}
