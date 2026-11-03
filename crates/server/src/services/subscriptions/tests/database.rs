use super::*;
use crate::{config::deployment::DatabaseConfig, dao::accounts::{authentication, subscriptions as dao}, db::Db};

async fn db() -> Db {
    crate::tests::init_config();
    Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL"]
async fn register_creates_unlimited_authority_atomically() {
    let db = db().await;
    let seq: i64 = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await.unwrap().get("id");
    let name = format!("clone-authority-register-{seq}");
    let registered = authentication::register(&db, &name, b"fixture", 0, 0).await.unwrap();
    let row = dao::by_user(&db, registered.user_id).await.unwrap();
    assert_eq!((row.grade, row.omega_until, row.grace_until), (rules::OMEGA, None, None));
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&registered.user_id]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL"]
async fn register_transaction_rolls_back_account_and_authority_on_later_failure() {
    let db = db().await;
    let seq: i64 = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await.unwrap().get("id");
    let name = format!("clone-authority-rollback-{seq}");
    let tx_name = name.clone();
    let result = db.transaction(|tx, _: &mut ()| { let tx_name = tx_name.clone(); Box::pin(async move {
        let registered = authentication::register(tx, &tx_name, b"fixture", 0, 0).await?;
        tx.execute("SELECT 1 / 0", &[]).await?;
        Ok::<i64, crate::db::Error>(registered.user_id)
    }) }, |_, ()| async move { Ok(()) }).await;
    assert!(result.is_err());
    assert!(db.query_opt("SELECT user_id FROM game.accounts WHERE user_name=$1", &[&name]).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL"]
async fn missing_authority_row_is_an_error_not_an_implicit_default() {
    let db = db().await;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    db.execute("DELETE FROM game.account_clone_grades WHERE user_id=$1", &[&fixture.account]).await.unwrap();
    assert!(dao::by_user(&db, fixture.account).await.is_err());
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL"]
async fn reapplying_schema_preserves_established_alpha_and_finite_rows() {
    let db = db().await;
    let alpha = crate::services::tests::fixture::create(&db).await.unwrap();
    let finite = crate::services::tests::fixture::create(&db).await.unwrap();
    let missing = crate::services::tests::fixture::create(&db).await.unwrap();
    let expiry = crate::client::filetime_now() + crate::consts::FILETIME_SECOND;
    db.execute("UPDATE game.account_clone_grades SET grade=0 WHERE user_id=$1", &[&alpha.account]).await.unwrap();
    db.execute("UPDATE game.account_clone_grades SET omega_until=$2, grace_until=$3 WHERE user_id=$1", &[&finite.account, &expiry, &(expiry + crate::consts::FILETIME_SECOND)]).await.unwrap();
    db.execute("DELETE FROM game.account_clone_grades WHERE user_id=$1", &[&missing.account]).await.unwrap();
    Db::connect(&DatabaseConfig { schema_on_start: true, ..crate::config::tests::test_database() }).await.unwrap();
    let row = dao::by_user(&db, finite.account).await.unwrap();
    assert_eq!((row.grade, row.omega_until, row.grace_until), (1, Some(expiry), Some(expiry + crate::consts::FILETIME_SECOND)));
    assert_eq!(dao::by_user(&db, alpha.account).await.unwrap().grade, rules::ALPHA);
    assert_eq!(dao::by_user(&db, missing.account).await.unwrap().grade, rules::OMEGA);
    for fixture in [alpha, finite, missing] { crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap(); }
}

fn job(row: &dao::CloneGrade) -> crate::scheduler::Job {
    let deadline = row.grace_until.unwrap();
    crate::scheduler::Job {
        id: format!("{}:{}", deadlines::KIND, row.user_id),
        kind: deadlines::KIND.into(), due_at: deadline,
        payload: serde_json::json!({"user": row.user_id, "revision": row.revision, "deadline": deadline}).to_string(),
    }
}

async fn arrange(db: &Db, user: i64, omega: i64, grace: i64) -> dao::CloneGrade {
    let before = dao::by_user(db, user).await.unwrap();
    assert!(dao::set_grade(db, user, rules::OMEGA, Some(omega), Some(grace), before.revision).await.unwrap());
    dao::by_user(db, user).await.unwrap()
}

async fn audit_count(db: &Db, user: i64) -> i64 {
    db.query_one("SELECT count(*) AS n FROM game.account_clone_grade_audit WHERE user_id=$1", &[&user]).await.unwrap().get("n")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn deadline_handler_replay_renewal_future_and_recovery() {
    let db = db().await;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), 0).await;
    let registration = crate::live_sessions::register(fixture.account, false).unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let _guard = registration.attach_sender(sender).unwrap();
    let now = crate::client::filetime_now();
    let second = crate::consts::FILETIME_SECOND;
    let expired = arrange(&db, fixture.account, now - 3 * second, now - second).await;
    let old = job(&expired);
    deadlines::schedule(&db, &expired).await.unwrap();
    deadlines::handle(db.clone(), client.world.clone(), old.clone()).await.unwrap();
    assert_eq!(dao::by_user(&db, fixture.account).await.unwrap().grade, rules::ALPHA);
    assert_eq!(receiver.try_recv().unwrap(), ("OnSubscriptionChangedServer".into(), vec![marshal::Value::Int(0)]));
    assert_eq!(receiver.try_recv().unwrap(), ("OnSubscriptionTimeUpdateServer".into(), vec![marshal::Value::Int(0)]));
    assert_eq!(audit_count(&db, fixture.account).await, 1);
    deadlines::handle(db.clone(), client.world.clone(), old.clone()).await.unwrap();
    assert!(receiver.try_recv().is_err());
    assert_eq!(audit_count(&db, fixture.account).await, 1);
    let renewed = arrange(&db, fixture.account, now + 600 * second, now + 900 * second).await;
    deadlines::handle(db.clone(), client.world.clone(), old).await.unwrap();
    deadlines::handle(db.clone(), client.world.clone(), job(&renewed)).await.unwrap();
    assert_eq!(dao::by_user(&db, fixture.account).await.unwrap().revision, renewed.revision);
    assert_eq!(audit_count(&db, fixture.account).await, 1);
    let id = job(&renewed).id;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap();
    let restarted = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    deadlines::recover(&restarted).await.unwrap();
    let recovered = crate::dao::jobs::lock_job(&db, &id).await.unwrap().unwrap();
    assert_eq!(recovered.due_at, renewed.grace_until.unwrap());
    let overdue = arrange(&db, fixture.account, now - 3 * second, now - second).await;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap();
    deadlines::recover(&restarted).await.unwrap();
    assert_eq!(crate::dao::jobs::lock_job(&db, &id).await.unwrap().unwrap().due_at, overdue.grace_until.unwrap());
    drop(_guard);
    drop(registration);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn management_grants_are_atomic_and_notify_account_in_order() {
    use crate::consts::GM_ROLES;
    use marshal::Value;
    let db = db().await;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), 0).await;
    let registration = crate::live_sessions::register(fixture.account, false).unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let _guard = registration.attach_sender(sender).unwrap();
    let initial = dao::by_user(&db, fixture.account).await.unwrap();
    assert!(management::set(&db, &client.world, fixture.account, 0, fixture.account, rules::ALPHA, None, 0).await.is_err());
    assert!(management::set(&db, &client.world, fixture.account, GM_ROLES, fixture.account, rules::OMEGA, Some(0), 0).await.is_err());
    assert!(management::set(&db, &client.world, fixture.account, GM_ROLES, -1, rules::ALPHA, None, 0).await.is_err());
    assert_eq!(dao::by_user(&db, fixture.account).await.unwrap().revision, initial.revision);
    assert_eq!(audit_count(&db, fixture.account).await, 0);
    assert!(receiver.try_recv().is_err());
    assert!(crate::dao::jobs::lock_job(&db, &format!("{}:{}", deadlines::KIND, fixture.account)).await.unwrap().is_none());
    management::set(&db, &client.world, fixture.account, GM_ROLES, fixture.account, rules::ALPHA, None, 0).await.unwrap();
    assert_eq!(receiver.try_recv().unwrap(), ("OnSubscriptionChangedServer".into(), vec![Value::Int(0)]));
    assert_eq!(receiver.try_recv().unwrap(), ("OnSubscriptionTimeUpdateServer".into(), vec![Value::Int(0)]));
    let alpha = dao::by_user(&db, fixture.account).await.unwrap();
    assert_eq!(alpha.revision, initial.revision + 1);
    management::set(&db, &client.world, fixture.account, GM_ROLES, fixture.account, rules::OMEGA, Some(600), 120).await.unwrap();
    let finite = dao::by_user(&db, fixture.account).await.unwrap();
    assert_eq!(finite.revision, alpha.revision + 1);
    assert_eq!(finite.grace_until.unwrap() - finite.omega_until.unwrap(), 120 * crate::consts::FILETIME_SECOND);
    assert_eq!(receiver.try_recv().unwrap(), ("OnSubscriptionChangedServer".into(), vec![Value::Int(1)]));
    let time = crate::client::unix_ns_from_filetime(finite.omega_until.unwrap()).unwrap();
    assert_eq!(receiver.try_recv().unwrap(), ("OnSubscriptionTimeUpdateServer".into(), vec![Value::Int(time)]));
    let audit = db.query_one("SELECT before_grade,after_grade,after_omega_until,after_grace_until FROM game.account_clone_grade_audit WHERE user_id=$1 AND after_omega_until IS NOT NULL", &[&fixture.account]).await.unwrap();
    assert_eq!((audit.get::<_, i16>("before_grade"), audit.get::<_, i16>("after_grade")), (0, 1));
    assert_eq!(audit.get::<_, Option<i64>>("after_omega_until"), finite.omega_until);
    assert_eq!(audit.get::<_, Option<i64>>("after_grace_until"), finite.grace_until);
    management::set(&db, &client.world, fixture.account, GM_ROLES, fixture.account, rules::OMEGA, Some(1200), 0).await.unwrap();
    let changed = dao::by_user(&db, fixture.account).await.unwrap();
    assert_eq!(receiver.try_recv().unwrap(), ("OnSubscriptionTimeUpdateServer".into(), vec![Value::Int(crate::client::unix_ns_from_filetime(changed.omega_until.unwrap()).unwrap())]));
    assert!(receiver.try_recv().is_err());
    management::set(&db, &client.world, fixture.account, GM_ROLES, fixture.account, rules::OMEGA, None, 0).await.unwrap();
    assert_eq!(receiver.try_recv().unwrap(), ("OnSubscriptionTimeUpdateServer".into(), vec![Value::Int(crate::client::unix_ns_from_filetime(crate::consts::FAR_FUTURE).unwrap())]));
    assert!(crate::dao::jobs::lock_job(&db, &format!("{}:{}", deadlines::KIND, fixture.account)).await.unwrap().is_none());
    let now = crate::client::filetime_now();
    arrange(&db, fixture.account, now - 2 * crate::consts::FILETIME_SECOND, now - crate::consts::FILETIME_SECOND).await;
    management::set(&db, &client.world, fixture.account, GM_ROLES, fixture.account, rules::ALPHA, None, 0).await.unwrap();
    assert_eq!(receiver.try_recv().unwrap(), ("OnSubscriptionChangedServer".into(), vec![Value::Int(0)]));
    assert_eq!(receiver.try_recv().unwrap(), ("OnSubscriptionTimeUpdateServer".into(), vec![Value::Int(0)]));
    drop(_guard);
    drop(registration);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn rpc_grade_and_subscription_time_share_grace_authority() {
    fn call<'a>(service: &'a str, method: &'a str, kwargs: &'a marshal::Value) -> crate::services::Call<'a> {
        crate::services::Call { service, method, args: &[], kwargs, bound: false, bind_params: None }
    }
    use marshal::Value;
    let db = db().await;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), 0).await;
    client.login.user_id = fixture.account;
    let now = crate::client::filetime_now();
    let second = crate::consts::FILETIME_SECOND;
    let finite = arrange(&db, fixture.account, now - second, now + 600 * second).await;
    let kwargs = Value::Dict(vec![]);
    let grade = crate::services::dispatch(&mut client, &call("subscriptionMgr", "GetCloneGrade", &kwargs)).await.unwrap();
    let reward_grade = crate::services::dispatch(&mut client, &call("loginRewardFacilities", "get_my_clone_grade", &kwargs)).await.unwrap();
    let time = crate::services::dispatch(&mut client, &call("subscriptionMgr", "GetSubscriptionTime", &kwargs)).await.unwrap();
    assert_eq!(grade, Value::Int(1));
    assert_eq!(reward_grade, grade);
    assert_eq!(time, Value::Int(crate::client::unix_ns_from_filetime(finite.omega_until.unwrap()).unwrap()));
    assert!(super::super::dispatch(&mut client, &call("subscriptionMgr", "NotHandled", &kwargs)).await.is_none());
    arrange(&db, fixture.account, now - 3 * second, now - second).await;
    let grade = crate::services::dispatch(&mut client, &call("subscriptionMgr", "GetCloneGrade", &kwargs)).await.unwrap();
    let reward_grade = crate::services::dispatch(&mut client, &call("loginRewardFacilities", "get_my_clone_grade", &kwargs)).await.unwrap();
    assert_eq!(grade, Value::Int(0));
    assert_eq!(reward_grade, grade);
    assert_eq!(crate::services::dispatch(&mut client, &call("subscriptionMgr", "GetSubscriptionTime", &kwargs)).await.unwrap(), Value::Int(0));
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
