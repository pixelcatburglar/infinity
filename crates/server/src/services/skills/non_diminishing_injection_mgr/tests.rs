//! Read-only injector credit RPC regressions.
use super::*;
use infinity_core::{consts::{dogma::ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, FLAG_BOOSTER, FILETIME_MINUTE}};
use crate::{services::tests::{client, fixture}};

async fn fingerprint(db: &crate::db::Db, character: i64) -> String {
    let job = format!("skills.complete:{character}");
    db.query_one("SELECT (SELECT row_to_json(c)::text FROM game.characters c WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(a)::text, '' ORDER BY a.item_id,a.attribute_id), '') FROM game.item_attributes a JOIN game.items i ON i.item_id=a.item_id WHERE i.owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY history_id), '') FROM game.skill_history h WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(j)::text, '' ORDER BY job_id), '') FROM game.jobs j WHERE job_id=$2) AS fingerprint", &[&character, &job]).await.unwrap().get("fingerprint")
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn rpc_reads_active_overrides_and_defaults_without_materializing_expiry() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let booster = c.sde.types.iter().find(|t| t.attribute(ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).is_some_and(|v| v >= 1.0 && v.fract() == 0.0)).expect("credit type in SDE");
    let type_id = booster.type_id;
    let default = booster.attribute(ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).unwrap() as i64;
    let now = filetime_now();
    let future = now + FILETIME_MINUTE;
    let past = now - FILETIME_MINUTE;
    for (expiry, override_value) in [(future, None), (future, Some(2.0_f64)), (future, Some(0.0)), (past, Some(9.0))] {
        let item: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_expires_at,brain_effect_ids) VALUES ($1,$2,$2,$3,true,$4,ARRAY[]::integer[]) RETURNING item_id", &[&type_id, &f.character, &FLAG_BOOSTER, &expiry]).await.unwrap().get("item_id");
        if let Some(value) = override_value { db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&item, &ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, &value]).await.unwrap(); }
    }
    let before = fingerprint(&db, f.character).await;
    let kwargs = Value::None;
    let call = Call { service: "nonDiminishingInjectionMgr", method: "GetAvailableNonDiminishingInjections", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let value = dispatch(&mut c, &call).await.expect("handled").unwrap_or_else(|_| panic!("credit RPC failed"));
    assert_eq!(value.as_int(), Some(default + 2));
    assert_eq!(value.as_int(), Some(trading::available_credits(&db, &c.sde, f.character, now).await.unwrap_or_else(|_| panic!("credit read failed"))));
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(c.outbox.is_empty());
    let other = Call { service: "nonDiminishingInjectionMgr", method: "Other", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert!(dispatch(&mut c, &other).await.is_none());
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn empty_session_bypasses_market_lock_and_full_dispatch_returns_zero() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let before = fingerprint(&db, f.character).await;
    let kwargs = Value::None;
    let call = Call { service: "nonDiminishingInjectionMgr", method: "GetAvailableNonDiminishingInjections", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    c.session = infinity_core::session::Session::new(1);
    assert!(!c.session.attributes().any(|(key, _)| key == "charid"));
    let lock = crate::services::market::lock().await;
    let reply = tokio::time::timeout(std::time::Duration::from_secs(2), crate::services::dispatch(&mut c, &call)).await.expect("empty session waited for market lock").unwrap();
    assert_eq!(reply, Value::Int(0));
    c.session.apply(vec![("charid", Value::Int(f.character))]);
    c.session.apply(vec![("charid", Value::None)]);
    assert!(c.session.attributes().any(|(key, value)| key == "charid" && *value == Value::None));
    let reply = tokio::time::timeout(std::time::Duration::from_secs(2), dispatch(&mut c, &call)).await.expect("cleared session waited for market lock").expect("handled").unwrap();
    assert_eq!(reply, Value::Int(0));
    let other = Call { service: "nonDiminishingInjectionMgr", method: "Other", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert!(dispatch(&mut c, &other).await.is_none());
    drop(lock);
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(c.outbox.is_empty());
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn character_switches_refresh_credits_and_invalid_identities_still_fail() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let first = fixture::create(&db).await.unwrap();
    let second = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), first.character).await;
    let type_id = c.sde.types.iter().find(|t| t.attribute(ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).is_some_and(|v| v >= 1.0 && v.fract() == 0.0)).expect("credit type in SDE").type_id;
    let expiry = filetime_now() + FILETIME_MINUTE;
    for (character, credits) in [(first.character, 2.0_f64), (second.character, 5.0_f64)] {
        let item: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_expires_at,brain_effect_ids) VALUES ($1,$2,$2,$3,true,$4,ARRAY[]::integer[]) RETURNING item_id", &[&type_id, &character, &FLAG_BOOSTER, &expiry]).await.unwrap().get("item_id");
        db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&item, &ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, &credits]).await.unwrap();
    }
    let before_first = fingerprint(&db, first.character).await;
    let before_second = fingerprint(&db, second.character).await;
    let kwargs = Value::None;
    let call = Call { service: "nonDiminishingInjectionMgr", method: "GetAvailableNonDiminishingInjections", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    for (identity, expected) in [(Value::Int(first.character), 2), (Value::None, 0), (Value::Int(first.character), 2), (Value::None, 0), (Value::Int(second.character), 5)] {
        c.session.apply(vec![("charid", identity)]);
        assert_eq!(dispatch(&mut c, &call).await.expect("handled").unwrap(), Value::Int(expected));
    }
    for identity in [Value::Int(i64::MAX), Value::str("malformed")] {
        c.session.apply(vec![("charid", identity)]);
        assert!(dispatch(&mut c, &call).await.expect("handled").is_err());
    }
    assert_eq!(fingerprint(&db, first.character).await, before_first);
    assert_eq!(fingerprint(&db, second.character).await, before_second);
    assert!(c.outbox.is_empty());
    fixture::cleanup(&db, second).await.unwrap();
    fixture::cleanup(&db, first).await.unwrap();
}
