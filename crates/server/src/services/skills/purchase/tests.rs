//! Purchase argument and transactional settlement coverage.
use super::*;
use crate::{consts::ACCOUNT_CASH, dao::{skills::history, wallets::characters}, db::outbox::{Effect, Notification}, lua::events::GameEvent, services::tests::{client, fixture}};
use std::sync::atomic::{AtomicBool, Ordering};

async fn history_fingerprint(db: &crate::db::Db, character: i64) -> String {
    db.query_one("SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY history_id), '') AS state FROM game.skill_history h WHERE character_id=$1", &[&character]).await.unwrap().get("state")
}

fn sde_validate(sde: &Sde, ids: &[i64], known: &HashSet<i32>) -> Result<(BTreeSet<i32>, f64), CallError> {
    validate(ids, known,
        |id| i32::try_from(id).ok().and_then(|id| sde.types.get(id)).is_some_and(|ty| ty.category_id == Some(CATEGORY_SKILL)),
        |id| {
            let id = i32::try_from(id).ok()?;
            let ty = sde.types.get(id)?;
            purchase_price(ty.published, ty.group_id, sde.typelists.contains(TYPE_LIST_SKILLS_AVAILABLE_FOR_PURCHASE, id, ty.group_id, ty.category_id), ty.base_price)
        })
}

fn assert_error(result: Result<(BTreeSet<i32>, f64), CallError>, expected: &str, args: Vec<(String, Value)>) {
    match result {
        Err(CallError::User { msg, dict }) => { assert_eq!(msg, expected); assert_eq!(dict, args); }
        _ => panic!("unexpected validation result"),
    }
}

#[test]
fn validation_passes_are_global_and_preserve_error_arguments() {
    let known = HashSet::from([1]);
    let check = |ids: &[i64]| validate(ids, &known, |id| id == 1 || id == 2, |id| (id == 1).then_some(1.0));
    assert_error(check(&[i64::MAX, i64::MAX]), messages::MSG_SKILL_PURCHASE_UNKNOWN_ERROR, vec![]);
    assert_error(check(&[2, i64::MAX]), messages::MSG_ITEM_NOT_A_SKILL, vec![("skillName".into(), Value::Int(i64::MAX))]);
    assert_error(check(&[1, 2]), messages::MSG_SKILL_UNAVAILABLE_FOR_PURCHASE, vec![("type".into(), Value::Int(2))]);
    assert_error(check(&[1]), messages::MSG_CHARACTER_ALREADY_KNOWS_SKILL, vec![("skillName".into(), Value::Int(1))]);
}

#[test]
fn prices_are_rounded_per_skill_and_unavailable_data_is_rejected() {
    let price = |base| purchase_price(Some(true), None, true, base);
    assert_eq!(price(Some(0.005)), Some(0.01));
    assert_eq!(price(Some(0.0)), Some(0.0));
    for base in [None, Some(-1.0), Some(f64::NAN), Some(f64::INFINITY), Some(f64::NEG_INFINITY), Some(f64::MAX)] {
        assert_eq!(price(base), None);
    }
    for group in [GROUP_CLOUD_COMPUTING, GROUP_SHIP_ADAPTION, GROUP_CHARACTER_ENHANCEMENT] {
        assert_eq!(purchase_price(Some(true), Some(group), true, Some(1.0)), None);
    }
    assert_eq!(purchase_price(Some(false), None, true, Some(1.0)), None);
    assert_eq!(purchase_price(None, None, true, Some(1.0)), None);
    assert_eq!(purchase_price(Some(true), None, false, Some(1.0)), None);
    let (_, total) = validate(&[1, 2], &HashSet::new(), |_| true, |_| price(Some(0.005))).unwrap();
    assert_eq!(total, 0.02);
}

#[test]
fn validation_normalizes_the_sum_of_rounded_prices() {
    let (_, total) = validate(&[1, 2], &HashSet::new(), |_| true, |id| Some(if id == 1 { 0.1 } else { 0.2 })).unwrap();
    assert_eq!(total, 0.3);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn normalized_total_debits_exact_balance_and_zero_has_no_effects() {
    crate::tests::init_config();
    let _market = market::lock().await;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (_, total) = validate(&[1, 2], &HashSet::new(), |_| true, |id| Some(if id == 1 { 0.1 } else { 0.2 })).unwrap();
    let balance = 0.3_f64;
    db.execute("UPDATE game.characters SET balance=$2::float8::numeric WHERE character_id=$1", &[&f.character, &balance]).await.unwrap();
    let effects = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        Box::pin(async move { wallets::debit(tx, outbox, f.character, 0.0).await })
    }, |(), outbox| async move { Ok::<_, CallError>(outbox.into_entries()) }).await.unwrap();
    assert!(effects.is_empty());
    assert_eq!(characters::balance(&db, f.character).await.unwrap(), balance);
    let effects = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        Box::pin(async move { wallets::debit(tx, outbox, f.character, total).await })
    }, |(), outbox| async move { Ok::<_, CallError>(outbox.into_entries()) }).await.unwrap();
    assert_eq!(characters::balance(&db, f.character).await.unwrap(), 0.0);
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Event(GameEvent::WalletChanged { owner_id, account_key, balance, delta }) if *owner_id == f.character && *account_key == ACCOUNT_CASH && *balance == 0.0 && *delta == -0.3)));
    fixture::cleanup(&db, f).await.unwrap();
}

#[test]
fn request_requires_one_integer_list_and_empty_keywords() {
    let kwargs = Value::Dict(vec![]);
    assert!(parse(&[Value::list([])], &kwargs).unwrap_or_else(|_| panic!("empty list rejected")).is_empty());
    assert_eq!(parse(&[Value::list([Value::Int(i64::MAX)])], &kwargs).unwrap_or_else(|_| panic!("integer rejected")), vec![i64::MAX]);
    for args in [vec![], vec![Value::tuple([])], vec![Value::Dict(vec![])], vec![Value::Bool(true)], vec![Value::unicode("1")], vec![Value::list([Value::Float(1.0)])], vec![Value::list([Value::Bool(true)])], vec![Value::list([Value::unicode("1")])], vec![Value::list([Value::list([])])], vec![Value::list([]), Value::None]] {
        assert!(parse(&args, &kwargs).is_err());
    }
    assert!(parse(&[Value::list([])], &Value::dict([("x", Value::Int(1))])).is_err());
    assert!(parse(&[Value::list([])], &Value::Bool(false)).is_err());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn purchase_settlement_and_rollback() {
    crate::tests::init_config();
    let _market = market::lock().await;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let dependent = sde.types.iter().find(|ty| {
        let id = i64::from(ty.type_id);
        sde_validate(&sde, &[id], &HashSet::new()).ok().is_some_and(|(_, price)| price > 0.0 && price < 1_000_000_000.0)
            && super::super::requirements::requirements(&sde, ty.type_id).any(|(_, level)| level > 0)
    }).expect("purchasable skill with prerequisites").type_id;
    let other = sde.types.iter().find(|ty| {
        ty.type_id != dependent && sde_validate(&sde, &[i64::from(ty.type_id)], &HashSet::new()).ok().is_some_and(|(_, price)| price > 0.0 && price < 1_000_000_000.0)
    }).expect("second purchasable skill").type_id;
    let mut ids = vec![i64::from(dependent), i64::from(other)];
    ids.sort_unstable_by(|a, b| b.cmp(a));
    let (_, total) = sde_validate(&sde, &ids, &HashSet::new()).unwrap_or_else(|_| panic!("valid skills rejected"));
    let unavailable = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SKILL) && sde_validate(&sde, &[i64::from(ty.type_id)], &HashSet::new()).is_err()).map(|ty| i64::from(ty.type_id)).expect("unavailable skill in SDE");
    let invalid = i64::MAX;
    let known = HashSet::from([ids[0] as i32]);
    for (request, expected, argument, value) in [
        (vec![invalid, invalid], messages::MSG_SKILL_PURCHASE_UNKNOWN_ERROR, "", invalid),
        (vec![unavailable, invalid], messages::MSG_ITEM_NOT_A_SKILL, "skillName", invalid),
        (vec![ids[0], unavailable], messages::MSG_SKILL_UNAVAILABLE_FOR_PURCHASE, "type", unavailable),
        (vec![ids[0]], messages::MSG_CHARACTER_ALREADY_KNOWS_SKILL, "skillName", ids[0]),
    ] {
        match sde_validate(&sde, &request, &known) {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, expected);
                if argument.is_empty() { assert!(dict.is_empty()); }
                else { assert_eq!(dict, vec![(argument.into(), Value::Int(value))]); }
            }
            _ => panic!("unexpected validation result"),
        }
    }
    let funded = total + 100.0;
    db.execute("UPDATE game.characters SET balance=$2::float8::numeric WHERE character_id=$1", &[&f.character, &funded]).await.unwrap();
    let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
    let now = filetime_now();
    let dispatched = Arc::new(AtomicBool::new(false));
    let zero = 0.0_f64;
    db.execute("UPDATE game.characters SET balance=$2::float8::numeric WHERE character_id=$1", &[&f.character, &zero]).await.unwrap();
    let history_before = history_fingerprint(&db, f.character).await;
    let flag = Arc::new(AtomicBool::new(false));
    let dispatched_refusal = flag.clone();
    let refused: Result<Value, CallError> = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone(); let ids = ids.clone();
        Box::pin(async move { purchase_tx(tx, outbox, &sde, f.character, &ids, filetime_now()).await })
    }, |value, _| async move { dispatched_refusal.store(true, Ordering::SeqCst); Ok(value) }).await;
    assert!(matches!(refused, Err(CallError::User { msg, dict }) if msg == messages::MSG_NOT_ENOUGH_MONEY && dict == vec![("balance".into(), Value::Float(0.0)), ("amount".into(), Value::Float(total))]));
    assert!(!flag.load(Ordering::SeqCst));
    assert_eq!(characters::balance(&db, f.character).await.unwrap(), zero);
    assert_eq!(before, fixture::skill_fingerprint(&db, f.character).await.unwrap());
    assert_eq!(history_fingerprint(&db, f.character).await, history_before);
    assert!(history::list(&db, f.character, 50).await.unwrap().is_empty());
    db.execute("UPDATE game.characters SET balance=$2::float8::numeric WHERE character_id=$1", &[&f.character, &funded]).await.unwrap();
    let flag = dispatched.clone();
    let failed: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone(); let ids = ids.clone();
        Box::pin(async move {
            purchase_tx(tx, outbox, &sde, f.character, &ids, now).await?;
            tx.query_one("SELECT 1 / 0", &[]).await.map_err(|e| db_err(crate::db::Error::from(e)))?;
            Ok(())
        })
    }, |(), _| async move { flag.store(true, Ordering::SeqCst); Ok(()) }).await;
    fixture::assert_division_by_zero(failed);
    assert!(!dispatched.load(Ordering::SeqCst));
    assert_eq!(before, fixture::skill_fingerprint(&db, f.character).await.unwrap());
    assert_eq!(characters::balance(&db, f.character).await.unwrap(), funded);
    assert!(history::list(&db, f.character, 50).await.unwrap().is_empty());
    let (result, effects) = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone(); let ids = ids.clone();
        Box::pin(async move { purchase_tx(tx, outbox, &sde, f.character, &ids, now).await })
    }, |result, outbox| async move { Ok::<_, CallError>((result, outbox.into_entries())) }).await.unwrap_or_else(|_| panic!("purchase failed"));
    assert_eq!(result, Value::list(ids.iter().copied().map(Value::Int)));
    assert_eq!(marshal::loads(&marshal::dumps(&result)).unwrap(), result);
    assert!((characters::balance(&db, f.character).await.unwrap() - 100.0).abs() < 0.001);
    let skills = training::current(&db, f.character).await.unwrap();
    assert_eq!(skills.len(), 2);
    assert!(skills.iter().all(|skill| skill.level == 0 && skill.skill_points == 0));
    let history = history::list(&db, f.character, 50).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(history.iter().all(|entry| entry.event_type_id == super::super::history::INJECTED && entry.absolute_points == 0));
    let expected_balance = characters::balance(&db, f.character).await.unwrap();
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.character && event == "OnAccountChange" && *args == vec![Value::str("cash"), Value::Int(f.character), Value::Float(expected_balance)])));
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.character && event == crate::client::BRAIN_REFRESH && args.is_empty())));
    let expected_infos = Value::Dict(skills.iter().map(|skill| (Value::Int(i64::from(skill.type_id)), super::super::training::skill_info(&sde, skill.type_id, 0, 0, now))).collect());
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.character && event == "OnServerSkillsChanged" && *args == vec![expected_infos.clone(), Value::None, Value::Int(now)])));
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Event(GameEvent::WalletChanged { owner_id, account_key, balance, delta }) if *owner_id == f.character && *account_key == ACCOUNT_CASH && *balance == expected_balance && *delta == -total)));
    let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
    let hidden = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SKILL) && ty.group_id.is_some_and(|group| [GROUP_CLOUD_COMPUTING, GROUP_SHIP_ADAPTION, GROUP_CHARACTER_ENHANCEMENT].contains(&group))).expect("hidden skill in SDE").type_id;
    let not_listed = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SKILL) && !sde.typelists.contains(TYPE_LIST_SKILLS_AVAILABLE_FOR_PURCHASE, ty.type_id, ty.group_id, ty.category_id)).expect("unlisted skill in SDE").type_id;
    let mut unavailable_types = vec![hidden, not_listed];
    if let Some(ty) = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SKILL) && ty.published != Some(true)) { unavailable_types.push(ty.type_id); }
    if let Some(ty) = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SKILL) && ty.base_price.is_none()) { unavailable_types.push(ty.type_id); }
    for id in unavailable_types {
        for (request, expected, argument, bad) in [
            (vec![i64::from(id)], messages::MSG_SKILL_UNAVAILABLE_FOR_PURCHASE, "type", i64::from(id)),
            (vec![i64::from(id), i64::MAX], messages::MSG_ITEM_NOT_A_SKILL, "skillName", i64::MAX),
            (vec![ids[0], i64::from(id)], messages::MSG_SKILL_UNAVAILABLE_FOR_PURCHASE, "type", i64::from(id)),
        ] {
            let flag = Arc::new(AtomicBool::new(false));
            let dispatched = flag.clone();
            let result: Result<Value, CallError> = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
                let sde = sde.clone(); let request = request.clone();
                Box::pin(async move { purchase_tx(tx, outbox, &sde, f.character, &request, now).await })
            }, |value, _| async move { dispatched.store(true, Ordering::SeqCst); Ok(value) }).await;
            assert!(matches!(result, Err(CallError::User { msg, dict }) if msg == expected && dict == vec![(argument.into(), Value::Int(bad))]));
            assert!(!flag.load(Ordering::SeqCst));
        }
    }
    let history_before = history_fingerprint(&db, f.character).await;
    let balance_before = characters::balance(&db, f.character).await.unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let dispatched_refusal = flag.clone();
    let failed: Result<Value, CallError> = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone(); let ids = ids.clone();
        Box::pin(async move { purchase_tx(tx, outbox, &sde, f.character, &ids, now).await })
    }, |value, _| async move { dispatched_refusal.store(true, Ordering::SeqCst); Ok(value) }).await;
    assert!(matches!(failed, Err(CallError::User { msg, dict }) if msg == messages::MSG_CHARACTER_ALREADY_KNOWS_SKILL && dict == vec![("skillName".into(), Value::Int(ids[0]))]));
    assert!(!flag.load(Ordering::SeqCst));
    assert_eq!(characters::balance(&db, f.character).await.unwrap(), balance_before);
    assert_eq!(history_fingerprint(&db, f.character).await, history_before);
    assert_eq!(before, fixture::skill_fingerprint(&db, f.character).await.unwrap());
    crate::scheduler::cancel(&db, &format!("{}:{}", super::super::completion::KIND, f.character)).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn bound_route_empty_and_purchase_roundtrip() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let skill = c.sde.types.iter().find(|ty| sde_validate(&c.sde, &[i64::from(ty.type_id)], &HashSet::new()).ok().is_some_and(|(_, price)| price > 0.0 && price < 1_000_000_000.0)).expect("purchasable skill").type_id;
    let (_, price) = sde_validate(&c.sde, &[i64::from(skill)], &HashSet::new()).unwrap();
    let funded = price + 100.0;
    db.execute("UPDATE game.characters SET balance=$2::float8::numeric WHERE character_id=$1", &[&f.character, &funded]).await.unwrap();
    let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
    let history_before = history_fingerprint(&db, f.character).await;
    let job = format!("{}:{}", super::super::completion::KIND, f.character);
    let jobs_before: i64 = db.query_one("SELECT count(*) AS n FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap().get("n");
    let kwargs = Value::None;
    let empty = [Value::list([])];
    let call = crate::services::Call { service: super::super::SKILL_HANDLER, method: "PurchaseSkills", args: &empty, kwargs: &kwargs, bound: true, bind_params: None };
    let result = super::super::dispatch(&mut c, &call).await.unwrap().unwrap();
    assert_eq!(result, Value::list([]));
    assert_eq!(marshal::loads(&marshal::dumps(&result)).unwrap(), result);
    assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
    assert_eq!(history_fingerprint(&db, f.character).await, history_before);
    assert_eq!(characters::balance(&db, f.character).await.unwrap(), funded);
    assert_eq!(db.query_one("SELECT count(*) AS n FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap().get::<_, i64>("n"), jobs_before);
    let args = [Value::list([Value::Int(i64::from(skill))])];
    let call = crate::services::Call { args: &args, ..call };
    let result = super::super::dispatch(&mut c, &call).await.unwrap().unwrap();
    assert_eq!(result, Value::list([Value::Int(i64::from(skill))]));
    assert_eq!(marshal::loads(&marshal::dumps(&result)).unwrap(), result);
    assert!(training::current(&db, f.character).await.unwrap().iter().any(|row| row.type_id == skill && row.level == 0 && row.skill_points == 0));
    assert!((characters::balance(&db, f.character).await.unwrap() - 100.0).abs() < 0.001);
    crate::scheduler::cancel(&db, &format!("{}:{}", super::super::completion::KIND, f.character)).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}
