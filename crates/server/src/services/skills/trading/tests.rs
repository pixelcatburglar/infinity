//! Skill trading boundaries and database settlement regressions.
use super::*;
mod reconciliation;
mod alpha_injections;

async fn fingerprint(db: &crate::db::Db, character: i64) -> String {
    let job = format!("skills.complete:{character}");
    db.query_one("SELECT (SELECT row_to_json(c)::text FROM game.characters c WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(a)::text, '' ORDER BY a.item_id,a.attribute_id), '') FROM game.item_attributes a JOIN game.items i ON i.item_id=a.item_id WHERE i.owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY history_id), '') FROM game.skill_history h WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(j)::text, '' ORDER BY job_id), '') FROM game.jobs j WHERE job_id=$2) AS fingerprint", &[&character, &job]).await.unwrap().get("fingerprint")
}

fn notification_events(outbox: Outbox<OnlineChanged>) -> Vec<String> {
    use crate::db::outbox::{Effect, Notification};
    outbox.into_entries().into_iter().filter_map(|entry| match entry {
        Effect::Notification(Notification::DirectClient { event, .. } | Notification::Pilot { event, .. }) => Some(event),
        Effect::Notification(Notification::Sender { .. } | Notification::Gateway(..)) => None,
        Effect::Event(crate::lua::events::GameEvent::SkillTrained { .. }) => panic!("manual item operation trained a skill"),
        Effect::Event(_) => None,
        Effect::Operation(_) => panic!("unexpected online reconciliation operation"),
    }).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn injector_rates_crossings_credits_and_limits() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let calc = |ty, qty, total, credits| injector_yield(&c.sde, ty, qty, total, credits, false).unwrap_or_else(|_| panic!("unexpected refusal"));
    assert_eq!(calc(FULL_INJECTOR, 2, FREE_ZONE - 1, 1), (1_000_000, 1));
    assert_eq!(calc(FULL_INJECTOR, 2, LOW_BOUNDARY - 1, 0), (700_000, 0));
    assert_eq!(calc(FULL_INJECTOR, 2, MEDIUM_BOUNDARY - 1, 0), (450_000, 0));
    assert!(injector_yield(&c.sde, FULL_INJECTOR, MAX_STACK + 1, 0, 0, false).unwrap().0 > 0);
    assert!(matches!(checked_injector_yield(&c.sde, FULL_INJECTOR, MAX_STACK + 1, 0, 0), Err(CallError::User { msg, .. }) if msg == messages::MSG_CUSTOM_NOTIFY));
    assert!(injector_yield(&c.sde, FULL_INJECTOR, 0, 0, 0, false).is_err());
    if let (Some(points), Some(limit)) = (c.sde.types.attribute(MINI_INJECTOR, CONTAINED_POINTS), c.sde.types.attribute(MINI_INJECTOR, MAX_CHARACTER_POINTS)) {
        assert_eq!(calc(MINI_INJECTOR, 1, 0, 1), (points as i64, 0));
        assert_eq!(calc(MINI_INJECTOR, 1, limit as i64, 0), (points as i64, 0));
        for (quantity, total) in [(1, limit as i64), (2, limit as i64 - 1)] {
            assert!(matches!(checked_injector_yield(&c.sde, MINI_INJECTOR, quantity, total, 0),
                Err(CallError::User { msg, dict }) if msg == messages::MSG_INJECTOR_SKILL_POINT_LIMIT_REACHED
                    && dict == vec![("typeID".into(), Value::Int(i64::from(MINI_INJECTOR))), ("limit".into(), Value::Int(limit as i64))]));
        }
    }
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn extraction_failure_rolls_back_and_success_consumes_once() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let skill = c.sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, t.type_id).rank, i64::from(crate::consts::dogma::MAX_SKILL_LEVEL)) >= BUCKET).expect("published skill").type_id;
    let filler = c.sde.types.iter().find(|t| t.type_id != skill && t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&c.sde, t.type_id).next().is_none()).expect("filler skill").type_id;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,5,$3),($1,$4,5,$5)", &[&f.character, &skill, &BUCKET, &filler, &FREE_ZONE]).await.unwrap();
    let item = transfers::insert(&db, EXTRACTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    let args = vec![Value::Dict(vec![(Value::Int(i64::from(skill)), Value::Int(BUCKET))]), Value::Int(item), Value::Int(1)];
    let invalid = vec![Value::Dict(vec![(Value::Int(i64::from(skill)), Value::Int(BUCKET - 1))]), Value::Int(item), Value::Int(1)];
    let before_invalid = fingerprint(&db, f.character).await;
    assert!(extract(&mut c, f.character, &invalid).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before_invalid);
    assert!(receiver.try_recv().is_err());
    db.execute("UPDATE game.skills SET skill_points=$3 WHERE character_id=$1 AND type_id=$2", &[&f.character, &filler, &(FREE_ZONE - 1)]).await.unwrap();
    assert!(extract(&mut c, f.character, &args).await.is_err());
    db.execute("UPDATE game.skills SET skill_points=$3 WHERE character_id=$1 AND type_id=$2", &[&f.character, &filler, &FREE_ZONE]).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp) VALUES ($1,0,$2,1,0,100)", &[&f.character, &skill]).await.unwrap();
    assert!(extract(&mut c, f.character, &args).await.is_err());
    db.execute("DELETE FROM game.skill_queue WHERE character_id=$1", &[&f.character]).await.unwrap();
    let before = fingerprint(&db, f.character).await;
    let sde = c.sde.clone();
    let selected = BTreeMap::from([(skill, BUCKET)]);
    let now = crate::client::filetime_now();
    let market_guard = crate::services::market::lock().await;
    let failed: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone();
        let selected = selected.clone();
        Box::pin(async move {
            extract_tx(tx, outbox, &sde, f.character, None, item, now, &selected).await?;
            tx.query_one("SELECT 1 / 0", &[]).await.map_err(|error| db_err(crate::db::Error::from(error)))?;
            Ok(())
        })
    }, |(), _| async move { panic!("rolled-back extraction dispatched") }).await;
    drop(market_guard);
    fixture::assert_division_by_zero(failed);
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.try_recv().is_err());
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i32>("quantity"), 1);
    assert!(crate::dao::skills::history::list(&db, f.character, 50).await.unwrap().is_empty());
    let mut buffer = [0_u8; 1];
    assert!(matches!(peer.try_read(&mut buffer), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    let output: i64 = db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.character, &FULL_INJECTOR]).await.unwrap().get("count");
    assert_eq!(output, 0);
    let sde = c.sde.clone();
    let selected = BTreeMap::from([(skill, BUCKET)]);
    let market_guard = crate::services::market::lock().await;
    let events = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone();
        let selected = selected.clone();
        Box::pin(async move { extract_tx(tx, outbox, &sde, f.character, None, item, crate::client::filetime_now(), &selected).await })
    }, |_, outbox| { let c = &mut c; async move {
        use crate::db::outbox::{Effect, Notification};
        let mut events = Vec::new();
        for entry in outbox.into_entries() {
            match entry {
                Effect::Notification(notification) => {
                    match &notification {
                        Notification::DirectClient { event, .. } | Notification::Pilot { event, .. } => events.push(event.clone()),
                        Notification::Sender { .. } | Notification::Gateway(..) => {}
                    }
                    notification.dispatch(&c.world.clone(), Some(&mut *c))?;
                }
                Effect::Event(crate::lua::events::GameEvent::SkillTrained { .. }) => panic!("manual extraction trained a skill"),
                Effect::Event(_) => {}
                Effect::Operation(change) => online::apply_online_changed(change),
            }
        }
        Ok(events)
    }}).await.unwrap_or_else(|_| panic!("extraction failed"));
    drop(market_guard);
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnItemChange").count(), 2);
    let mut events = Vec::new();
    while let Ok((event, _)) = receiver.try_recv() { events.push(event); }
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnServerSkillsChanged").count(), 1);
    assert_eq!(events.iter().filter(|event| event.as_str() == crate::client::BRAIN_REFRESH).count(), 1);
    assert!(extract(&mut c, f.character, &args).await.is_err());
    let output: i64 = db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.character, &FULL_INJECTOR]).await.unwrap().get("count");
    assert_eq!(output, 1);
    assert_eq!(crate::dao::skills::history::list(&db, f.character, 50).await.unwrap().len(), 1);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn conversion_and_injection_conserve_items_and_refuse_wrong_sources() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.notify_tx = sender.clone();
    c.world.register_pilot(f.character, sender);
    fixture::seed_skill(&db, f.character).await.unwrap();
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let item = transfers::insert(&db, FULL_INJECTOR, f.character, station, FLAG_HANGAR, 2).await.unwrap();
    let args = vec![Value::Int(item), Value::Int(1)];
    let large = transfers::insert(&db, FULL_INJECTOR, f.character, station, FLAG_HANGAR, (MAX_STACK + 1) as i32).await.unwrap();
    assert!(inject(&mut c, f.character, &[Value::Int(item), Value::Int(3)], false).await.is_err());
    assert!(inject(&mut c, f.character, &[Value::Int(large), Value::Int(MAX_STACK + 1)], true).await.is_err());
    assert!(inject(&mut c, f.character, &[Value::Int(large), Value::Int(MAX_STACK + 1)], false).await.is_err());
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&large]).await.unwrap();
    convert(&mut c, f.character, &args, true).await.unwrap_or_else(|_| panic!("split failed"));
    let mut events = Vec::new();
    while let Ok((event, _)) = receiver.try_recv() { events.push(event); }
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnServerSkillsChanged").count(), 1);
    assert_eq!(events.iter().filter(|event| event.as_str() == crate::client::BRAIN_REFRESH).count(), 1);
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnNewSkillQueueSaved").count(), 1);
    assert!(crate::dao::skills::queue::list(&db, f.character).await.unwrap().is_empty());
    let small: i64 = db.query_one("SELECT item_id FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.character, &SMALL_INJECTOR]).await.unwrap().get("item_id");
    let before = fingerprint(&db, f.character).await;
    assert!(matches!(convert(&mut c, f.character, &[Value::Int(small), Value::Int(4)], false).await, Err(CallError::User { msg, .. }) if msg == messages::MSG_COMBINE_SKILL_INJECTOR_TOO_FEW_INJECTORS));
    assert_eq!(fingerprint(&db, f.character).await, before);
    convert(&mut c, f.character, &[Value::Int(small), Value::Int(5)], false).await.unwrap_or_else(|_| panic!("combine failed"));
    let quantity: i64 = db.query_one("SELECT sum(quantity)::bigint AS quantity FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.character, &FULL_INJECTOR]).await.unwrap().get("quantity");
    assert_eq!(quantity, 2);
    let quote = quote(&c, f.character, &[Value::Int(i64::from(FULL_INJECTOR)), Value::Int(1), Value::Int(999)]).await.unwrap_or_else(|_| panic!("quote failed"));
    inject(&mut c, f.character, &args, true).await.unwrap_or_else(|_| panic!("constraint check failed"));
    inject(&mut c, f.character, &args, false).await.unwrap_or_else(|_| panic!("injection failed"));
    assert_eq!(crate::dao::characters::attributes::free_points(&db, f.character).await.unwrap(), quote.as_int().unwrap());
    assert!(inject(&mut c, f.character, &args, false).await.is_err());
    let remote = transfers::insert(&db, FULL_INJECTOR, f.character, station + 1, FLAG_HANGAR, 1).await.unwrap();
    assert!(inject(&mut c, f.character, &[Value::Int(remote), Value::Int(1)], false).await.is_err());
    let seven = transfers::insert(&db, SMALL_INJECTOR, f.character, station, FLAG_HANGAR, 7).await.unwrap();
    convert(&mut c, f.character, &[Value::Int(seven), Value::Int(7)], false).await.unwrap_or_else(|_| panic!("combine seven failed"));
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&seven]).await.unwrap().get::<_, i32>("quantity"), 2);
    let quantity: i64 = db.query_one("SELECT sum(quantity)::bigint AS quantity FROM game.items WHERE owner_id=$1 AND type_id=$2 AND location_id=$3", &[&f.character, &FULL_INJECTOR, &station]).await.unwrap().get("quantity");
    assert_eq!(quantity, 2);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn credit_override_zero_is_authoritative() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let booster = c.sde.types.iter().find(|t| c.sde.types.attribute(t.type_id, ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).is_some_and(|value| value >= 1.0 && value.fract() == 0.0)).expect("credit booster in SDE");
    let item = transfers::insert(&db, booster.type_id, f.character, f.character, crate::consts::FLAG_BOOSTER, 1).await.unwrap();
    assert!(available_credits(&db, &c.sde, f.character, crate::client::filetime_now()).await.unwrap_or_else(|_| panic!("credit read failed")) > 0);
    db.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value) VALUES ($1,$2,0)", &[&item, &ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES]).await.unwrap();
    assert_eq!(available_credits(&db, &c.sde, f.character, crate::client::filetime_now()).await.unwrap_or_else(|_| panic!("credit read failed")), 0);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn extraction_refuses_broken_dependent_prerequisite() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let (dependent, required, threshold) = c.sde.types.iter().filter(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true)).find_map(|ty| {
        super::super::requirements::requirements(&c.sde, ty.type_id).find_map(|(required, level)| {
            let target = c.sde.types.get(required)?;
            let threshold = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, required).rank, i64::from(level));
            (required != ty.type_id && target.published == Some(true) && threshold >= BUCKET).then_some((ty.type_id, required, threshold))
        })
    }).expect("SDE prerequisite with enough points");
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,5,$3),($1,$4,1,$5)", &[&f.character, &required, &threshold, &dependent, &EXTRACTION_MINIMUM]).await.unwrap();
    let item = transfers::insert(&db, EXTRACTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    let before = fingerprint(&db, f.character).await;
    let args = [Value::Dict(vec![(Value::Int(i64::from(required)), Value::Int(BUCKET))]), Value::Int(item), Value::Int(1)];
    assert!(extract(&mut c, f.character, &args).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.try_recv().is_err());
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i32>("quantity"), 1);
    assert!(crate::dao::skills::history::list(&db, f.character, 50).await.unwrap().is_empty());
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn extraction_refuses_equipped_item_prerequisite() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let (equipment, required, threshold) = c.sde.types.iter().filter(|t| t.category_id != Some(crate::consts::CATEGORY_SKILL)).find_map(|ty| {
        super::super::requirements::requirements(&c.sde, ty.type_id).find_map(|(required, level)| {
            let target = c.sde.types.get(required)?;
            let threshold = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, required).rank, i64::from(level));
            (target.published == Some(true) && threshold >= BUCKET).then_some((ty.type_id, required, threshold))
        })
    }).expect("SDE equipment prerequisite with enough points");
    let filler = c.sde.types.iter().find(|t| t.type_id != required && t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true)).expect("filler skill").type_id;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,5,$3),($1,$4,5,$5)", &[&f.character, &required, &threshold, &filler, &EXTRACTION_MINIMUM]).await.unwrap();
    transfers::insert(&db, equipment, f.character, f.character, crate::consts::FLAG_IMPLANT, 1).await.unwrap();
    let item = transfers::insert(&db, EXTRACTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    let before = fingerprint(&db, f.character).await;
    let args = [Value::Dict(vec![(Value::Int(i64::from(required)), Value::Int(BUCKET))]), Value::Int(item), Value::Int(1)];
    assert!(extract(&mut c, f.character, &args).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.try_recv().is_err());
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i32>("quantity"), 1);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn extraction_refuses_spare_clone_implant_prerequisite() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (implant, slot, required, threshold) = c.sde.types.iter().filter(|t| t.category_id == Some(crate::consts::CATEGORY_IMPLANT) && t.group_id != Some(crate::consts::GROUP_BOOSTER)).find_map(|ty| {
        let slot = ty.attribute(crate::consts::dogma::ATTR_IMPLANTNESS).filter(|v| *v > 0.0 && v.fract() == 0.0)? as i32;
        super::super::requirements::requirements(&c.sde, ty.type_id).find_map(|(required, level)| {
            let target = c.sde.types.get(required)?;
            let threshold = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, required).rank, i64::from(level));
            (target.published == Some(true) && threshold >= BUCKET).then_some((ty.type_id, slot, required, threshold))
        })
    }).expect("implant prerequisite with enough points");
    let filler = c.sde.types.iter().find(|t| t.type_id != required && t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&c.sde, t.type_id).next().is_none()).unwrap().type_id;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,5,$3),($1,$4,5,$5)", &[&f.character, &required, &threshold, &filler, &EXTRACTION_MINIMUM]).await.unwrap();
    let clone = crate::dao::characters::clones::create(&db, f.character, station).await.unwrap();
    let dormant = transfers::insert(&db, implant, f.character, f.character, crate::consts::FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true, brain_slot=$2, jump_clone_id=$3 WHERE item_id=$1", &[&dormant, &slot, &clone.clone_id]).await.unwrap();
    let extractor = transfers::insert(&db, EXTRACTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    let args = [Value::Dict(vec![(Value::Int(i64::from(required)), Value::Int(BUCKET))]), Value::Int(extractor), Value::Int(1)];
    assert!(extract(&mut c, f.character, &args).await.is_err());
    assert_eq!(db.query_one("SELECT skill_points FROM game.skills WHERE character_id=$1 AND type_id=$2", &[&f.character, &required]).await.unwrap().get::<_, i64>("skill_points"), threshold);
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&extractor]).await.unwrap().get::<_, i32>("quantity"), 1);
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&dormant]).await.unwrap();
    crate::dao::characters::clones::destroy(&db, clone.clone_id, f.character).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn extraction_refuses_clone_floor_and_overtraining_atomically() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let clone_skill = crate::consts::SKILL_INFOMORPH_PSYCHOLOGY;
    assert!(c.sde.types.get(clone_skill).is_some(), "clone skill");
    let filler = c.sde.types.iter().find(|t| t.type_id != clone_skill && t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&c.sde, t.type_id).next().is_none() && super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, t.type_id).rank, i64::from(crate::consts::dogma::MAX_SKILL_LEVEL)) >= BUCKET).expect("filler skill").type_id;
    let reserve = c.sde.types.iter().find(|t| t.type_id != clone_skill && t.type_id != filler && t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&c.sde, t.type_id).next().is_none()).expect("reserve skill").type_id;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let clone_points = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, clone_skill).rank, 1);
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,1,$3),($1,$4,5,$5),($1,$6,5,$7)", &[&f.character, &clone_skill, &clone_points, &filler, &BUCKET, &reserve, &FREE_ZONE]).await.unwrap();
    let item = transfers::insert(&db, EXTRACTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    crate::dao::characters::clones::create(&db, f.character, station).await.unwrap();
    let args = [Value::Dict(vec![(Value::Int(i64::from(clone_skill)), Value::Int(1)), (Value::Int(i64::from(filler)), Value::Int(BUCKET - 1))]), Value::Int(item), Value::Int(1)];
    let before = fingerprint(&db, f.character).await;
    assert!(extract(&mut c, f.character, &args).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.try_recv().is_err());
    for (tier, count) in [(crate::consts::SKILL_ADVANCED_INFOMORPH_PSYCHOLOGY, 6_i64), (crate::consts::SKILL_ELITE_INFOMORPH_PSYCHOLOGY, 11_i64)] {
        if c.sde.types.get(tier).is_none() { continue; }
        let existing_count = crate::dao::characters::clones::clones(&db, f.character).await.unwrap().len() as i64;
        for index in existing_count..count {
            crate::dao::characters::clones::create(&db, f.character, station + index).await.unwrap();
        }
        let points = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, tier).rank, 1);
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,1,$3)", &[&f.character, &tier, &points]).await.unwrap();
        let args = [Value::Dict(vec![(Value::Int(i64::from(tier)), Value::Int(1)), (Value::Int(i64::from(filler)), Value::Int(BUCKET - 1))]), Value::Int(item), Value::Int(1)];
        let before = fingerprint(&db, f.character).await;
        assert!(extract(&mut c, f.character, &args).await.is_err());
        assert_eq!(fingerprint(&db, f.character).await, before);
        assert!(receiver.try_recv().is_err());
        db.execute("DELETE FROM game.skills WHERE character_id=$1 AND type_id=$2", &[&f.character, &tier]).await.unwrap();
    }
    db.execute("DELETE FROM game.jump_clones WHERE owner_id=$1", &[&f.character]).await.unwrap();
    let cap = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, filler).rank, i64::from(crate::consts::dogma::MAX_SKILL_LEVEL));
    db.execute("UPDATE game.skills SET skill_points=$3 WHERE character_id=$1 AND type_id=$2", &[&f.character, &filler, &(cap + 1)]).await.unwrap();
    let args = [Value::Dict(vec![(Value::Int(i64::from(filler)), Value::Int(BUCKET))]), Value::Int(item), Value::Int(1)];
    let before = fingerprint(&db, f.character).await;
    assert!(extract(&mut c, f.character, &args).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.try_recv().is_err());
    let mut buffer = [0_u8; 1];
    assert!(matches!(peer.try_read(&mut buffer), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn injection_and_conversion_outboxes_are_exact() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let item = transfers::insert(&db, FULL_INJECTOR, f.character, station, FLAG_HANGAR, 2).await.unwrap();
    let now = crate::client::filetime_now();
    let sde = c.sde.clone();
    let _market_guard = crate::services::market::lock().await;
    let events = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone();
        Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, item, 1, 1, now, false).await })
    }, |_, outbox| async move { Ok(notification_events(outbox)) }).await.unwrap_or_else(|_| panic!("injection failed"));
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnItemChange").count(), 1);
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnFreeSkillPointsChanged").count(), 1);
    assert_eq!(events.len(), 2);
    let events = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone();
        Box::pin(async move { convert_tx(tx, outbox, &sde, f.character, None, item, 1, 1, now, true).await })
    }, |_, outbox| async move { Ok(notification_events(outbox)) }).await.unwrap_or_else(|_| panic!("conversion failed"));
    assert_eq!(events.iter().filter(|event| event.as_str() == "OnItemChange").count(), 2);
    assert_eq!(events.len(), 2);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn overdue_training_booster_quote_check_and_settlement_agree() {
    use crate::services::tests::{client, fixture};
    use crate::services::fit::{Fit, Key};
    use crate::dao::skills::queue::QueueEntry;
    use super::super::training;
    use crate::consts::dogma::CHAR_ATTRIBUTES;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let injector = transfers::insert(&db, FULL_INJECTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    let now = crate::client::filetime_now();
    let start = now - 20 * crate::consts::FILETIME_MINUTE;
    let expiry = now - 10 * crate::consts::FILETIME_MINUTE;
    let existing = crate::dao::skills::training::current(&db, f.character).await.unwrap();
    let mut found = false;
    'types: for data in c.sde.types.iter() {
        if data.attribute(crate::consts::dogma::ATTR_BOOSTERNESS).is_none() { continue; }
        for entry in data.effects() {
            let Some(effect) = c.sde.dogma.effect(entry.effect_id) else { continue };
            if effect.effect_category.unwrap_or(0) != 0 { continue; }
            for modifier in effect.modifiers() {
                if !CHAR_ATTRIBUTES.contains(&(modifier.modified as i64)) || modifier.modified == modifier.modifying { continue; }
                let item: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_activated_at,brain_expires_at,brain_effect_ids) VALUES ($1,$2,$2,$3,true,$4,$5,ARRAY[$6::integer]) RETURNING item_id", &[&data.type_id, &f.character, &crate::consts::FLAG_BOOSTER, &start, &expiry, &effect.effect_id]).await.unwrap().get("item_id");
                db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&item, &modifier.modifying]).await.unwrap();
                let before = Fit::load_character_at(&db, c.sde.clone(), f.character, expiry - 1).await.unwrap_or_else(|_| panic!("loading active booster"));
                let after = Fit::load_character_at(&db, c.sde.clone(), f.character, expiry).await.unwrap_or_else(|_| panic!("loading expired booster"));
                let old = CHAR_ATTRIBUTES.iter().map(|&id| (id, before.value(Key::Char, id as i32))).collect();
                let new = CHAR_ATTRIBUTES.iter().map(|&id| (id, after.value(Key::Char, id as i32))).collect();
                if old != new {
                    for skill in c.sde.types.iter().filter(|skill| skill.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some() && !existing.iter().any(|row| row.type_id == skill.type_id)) {
                        let mut row = QueueEntry { position: 0, type_id: skill.type_id, to_level: 1, start_sp: 0, dest_sp: 100_000, start_time: None, end_time: None };
                        let mut slower = row.clone();
                        let Ok(old_end) = training::schedule(&c.sde, &mut row, &old, start) else { continue };
                        let Ok(new_end) = training::schedule(&c.sde, &mut slower, &new, start) else { continue };
                        if old_end <= now || new_end <= old_end { continue; }
                        crate::dao::skills::training::starting_skill(&db, f.character, skill.type_id, 0, 0).await.unwrap();
                        crate::dao::skills::queue::insert(&db, f.character, &row).await.unwrap();
                        let plan = super::super::timeline::plan_tx(&db, &c.sde, f.character, now).await.unwrap_or_else(|_| panic!("planning expiry"));
                        let projected = plan.trained_points().unwrap_or_else(|_| panic!("projecting points"));
                        let stale = existing.iter().map(|s| s.skill_points).sum::<i64>() + training::points_at(0, row.dest_sp, start, old_end, now);
                        if stale <= projected + 10 || (projected + stale) / 2 >= FREE_ZONE {
                            crate::dao::skills::queue::clear(&db, f.character).await.unwrap();
                            crate::dao::skills::training::remove(&db, f.character, &[skill.type_id]).await.unwrap();
                            continue;
                        }
                        let free = FREE_ZONE - (projected + stale) / 2;
                        db.execute("UPDATE game.characters SET free_skill_points=$2 WHERE character_id=$1", &[&f.character, &free]).await.unwrap();
                        let fingerprint_before = fingerprint(&db, f.character).await;
                        let quote = quote(&c, f.character, &[Value::Int(i64::from(FULL_INJECTOR)), Value::Int(1)]).await.unwrap_or_else(|_| panic!("quoting injector")).as_int().unwrap();
                        assert_eq!(quote, BUCKET);
                        assert_eq!(fingerprint(&db, f.character).await, fingerprint_before);
                        let guard = crate::services::market::lock().await;
                        let sde = c.sde.clone();
                        let check_events = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
                            let sde = sde.clone();
                            Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, injector, 1, 1, now, true).await })
                        }, |_, outbox| async move { Ok(outbox.into_entries().len()) }).await.unwrap_or_else(|_| panic!("checking injector"));
                        assert_eq!(check_events, 0);
                        assert_eq!(fingerprint(&db, f.character).await, fingerprint_before);
                        let events = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
                            let sde = sde.clone();
                            Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, injector, 1, 1, now, false).await })
                        }, |_, outbox| async move { Ok(notification_events(outbox)) }).await.unwrap_or_else(|_| panic!("settling injector"));
                        drop(guard);
                        assert_eq!(crate::dao::characters::attributes::free_points(&db, f.character).await.unwrap(), free + quote);
                        assert!(crate::dao::items::brain::get(&db, f.character, item).await.unwrap().is_none());
                        assert_eq!(events.iter().filter(|e| e.as_str() == "OnServerBoostersChanged").count(), 1);
                        assert_eq!(events.iter().filter(|e| e.as_str() == crate::client::BRAIN_REFRESH).count(), 1);
                        assert_eq!(events.iter().filter(|e| e.as_str() == "OnNewSkillQueueSaved").count(), 1);
                        found = true;
                        break;
                    }
                }
                if found { break 'types; }
                db.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
            }
        }
    }
    fixture::cleanup(&db, f).await.unwrap();
    assert!(found, "test SDE has no usable training booster and skill");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn expiry_resets_cache_before_consumption_and_spent_credit_survives_replay() {
    use crate::db::outbox::{Effect, Notification};
    use crate::services::tests::{client, fixture};
    use super::super::completion;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let existing = crate::dao::skills::training::current(&db, f.character).await.unwrap();
    let mut seen: std::collections::BTreeSet<_> = existing.iter().map(|skill| skill.type_id).collect();
    let mut points = 0_i64;
    for skill in c.sde.types.iter().filter(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true)) {
        let Some(rank) = skill.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).filter(|rank| rank.is_finite() && *rank > 0.0) else { continue };
        if !seen.insert(skill.type_id) { continue; }
        let skill_points = super::super::formulas::sp_for_level(rank, i64::from(crate::consts::dogma::MAX_SKILL_LEVEL));
        crate::dao::skills::training::starting_skill(&db, f.character, skill.type_id, crate::consts::dogma::MAX_SKILL_LEVEL, skill_points).await.unwrap();
        points += skill_points;
        if points >= FREE_ZONE { break; }
    }
    assert!(points >= FREE_ZONE, "published skills must provide enough rank-derived points");
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let injector = transfers::insert(&db, FULL_INJECTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    let booster = c.sde.types.iter().find(|t| t.attribute(ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).is_some_and(|v| v >= 1.0 && v.fract() == 0.0)).expect("credit type in SDE");
    let type_id = booster.type_id;
    let now = crate::client::filetime_now();
    let past = now - crate::consts::FILETIME_MINUTE;
    let future = now + crate::consts::FILETIME_MINUTE;
    let mut ids = Vec::new();
    for (expiry, value) in [(past, 2.0_f64), (future, 1.0_f64)] {
        let id: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_expires_at,brain_effect_ids) VALUES ($1,$2,$2,$3,true,$4,ARRAY[]::integer[]) RETURNING item_id", &[&type_id, &f.character, &crate::consts::FLAG_BOOSTER, &expiry]).await.unwrap().get("item_id");
        db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&id, &ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, &value]).await.unwrap();
        ids.push(id);
    }
    let guard = crate::services::market::lock().await;
    let sde = c.sde.clone();
    let events = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone();
        Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, injector, 1, 1, now, false).await })
    }, |_, outbox| async move { Ok({ let entries = outbox.into_entries(); assert!(!entries.iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); entries }) }).await.unwrap_or_else(|_| panic!("injecting with credits"));
    drop(guard);
    let credit_events: Vec<_> = events.into_iter().filter_map(|entry| match entry {
        Effect::Notification(Notification::Pilot { char_id, event, args }) if char_id == f.character && event.starts_with("OnNonDiminishingInjections") => Some((event, args)),
        _ => None,
    }).collect();
    assert_eq!(credit_events.len(), 3);
    assert_eq!(credit_events[0].0, "OnNonDiminishingInjectionsRemoved");
    assert!(credit_events[0].1.is_empty());
    assert_eq!(credit_events[1].0, "OnNonDiminishingInjectionsAdded");
    assert_eq!(credit_events[1].1[0].as_int(), Some(1));
    assert_eq!(credit_events[2].0, "OnNonDiminishingInjectionsUsed");
    assert_eq!(credit_events[2].1[0].as_int(), Some(1));
    assert_eq!(available_credits(&db, &c.sde, f.character, now).await.unwrap_or_else(|_| panic!("reading spent credit")), 0);
    let stored = crate::dao::items::dogma::overrides(&db, ids[1]).await.unwrap();
    assert_eq!(stored.iter().find(|row| row.attribute_id == ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).map(|row| row.value), Some(0.0));
    completion::seed(&db).await.unwrap_or_else(|_| panic!("seeding completion"));
    let job_id = format!("{}:{}", completion::KIND, f.character);
    let job = crate::scheduler::Job { id: job_id.clone(), kind: completion::KIND.into(), due_at: now, payload: f.character.to_string() };
    completion::handle(db.clone(), c.world.clone(), job.clone()).await.unwrap();
    completion::seed(&db).await.unwrap_or_else(|_| panic!("reseeding completion"));
    completion::handle(db.clone(), c.world.clone(), job).await.unwrap();
    assert_eq!(available_credits(&db, &c.sde, f.character, now).await.unwrap_or_else(|_| panic!("reading replayed credit")), 0);
    assert!(receiver.try_recv().is_err());
    crate::scheduler::cancel(&db, &job_id).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}
