//! Database regression for extraction-driven fitting reconciliation.
use super::*;
use infinity_core::{consts::{dogma::ATTR_CPU, dogma::ATTR_CPU_LOAD, dogma::ATTR_CPU_OUTPUT, dogma::ATTR_POWER, dogma::ATTR_POWER_LOAD, dogma::ATTR_POWER_OUTPUT, CATEGORY_MODULE, CATEGORY_SHIP, CATEGORY_SKILL, dogma::EFFECT_ONLINE, FLAG_HI_SLOT_FIRST}};
use crate::{dao::items::dogma, services::{fit::{Fit, Key}, skills::{formulas, training}}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn extraction_reconciles_owned_hulls_and_rolls_back() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let scenario = crate::tests::catch_scenario(async {
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let receiver = std::sync::Arc::new(std::sync::Mutex::new(receiver));
    c.world.register_pilot(f.character, sender);
    let fences = crate::services::effects::online::capture_online_fences(&c.world, &[f.character]).await;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP) && t.attribute(14).is_some_and(|v| v >= 1.0)).unwrap().type_id;
    let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == 12) && t.effects().iter().any(|e| i64::from(e.effect_id) == EFFECT_ONLINE)).unwrap().type_id;
    let mut ships = Vec::new();
    let mut modules = Vec::new();
    for _ in 0..2 {
        let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,0,1,true) RETURNING item_id", &[&ship_type, &f.character, &station]).await.unwrap().get("item_id");
        let module: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,false) RETURNING item_id", &[&module_type, &f.character, &ship, &FLAG_HI_SLOT_FIRST]).await.unwrap().get("item_id");
        for attr in [ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT] { assert!(dogma::set_override(&db, ship, attr, 1000.0).await.unwrap()); }
        for attr in [ATTR_CPU, ATTR_POWER] { assert!(dogma::set_override(&db, module, attr, 0.0).await.unwrap()); }
        ships.push(ship); modules.push(module);
    }
    c.session.apply(vec![("shipid", Value::Int(ships[0]))]);
    let mut chosen = None;
    for t in sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SKILL) && [ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT].into_iter().any(|a| t.modifiers(&sde.dogma, a).any(|(_, m)| m.func == "ItemModifier" && m.domain == "shipID"))) {
        let rank = training::skill_type(&sde, t.type_id).rank;
        let high_sp = formulas::sp_for_level(rank, 5);
        let removed = BUCKET.min(high_sp - formulas::sp_for_level(rank, 4));
        if removed <= 0 { continue; }
        let low_level = formulas::level_from_points(high_sp - removed, rank);
        if super::super::super::requirements::requirements(&sde, ship_type).chain(super::super::super::requirements::requirements(&sde, module_type)).any(|(id, level)| id == t.type_id && level > low_level) { continue; }
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,$4)", &[&f.character, &t.type_id, &low_level, &(high_sp - removed)]).await.unwrap();
        let low = Fit::load(&db, sde.clone(), f.character, ships[0]).await.ok().unwrap();
        let low_values = [low.value(Key::Ship, ATTR_CPU_OUTPUT), low.value(Key::Ship, ATTR_POWER_OUTPUT)];
        db.execute("UPDATE game.skills SET level=5,skill_points=$3 WHERE character_id=$1 AND type_id=$2", &[&f.character, &t.type_id, &high_sp]).await.unwrap();
        let high = Fit::load(&db, sde.clone(), f.character, ships[0]).await.ok().unwrap();
        let high_values = [high.value(Key::Ship, ATTR_CPU_OUTPUT), high.value(Key::Ship, ATTR_POWER_OUTPUT)];
        if let Some(i) = (0..2).find(|&i| high_values[i] > low_values[i] + 1e-4) {
            chosen = Some((t.type_id, high_sp, removed, [ATTR_CPU, ATTR_POWER][i], (high_values[i] + low_values[i]) / 2.0)); break;
        }
        db.execute("DELETE FROM game.skills WHERE character_id=$1 AND type_id=$2", &[&f.character, &t.type_id]).await.unwrap();
    }
    let (skill, mut total, removed, resource, usage) = chosen.expect("published fitting skill with an extractable level-five bonus");
    let mut selected = BTreeMap::from([(skill, removed)]);
    let mut remaining = BUCKET - removed;
    for t in sde.types.iter().filter(|t| t.type_id != skill && t.published == Some(true) && t.category_id == Some(CATEGORY_SKILL) && super::super::super::requirements::requirements(&sde, t.type_id).next().is_none() && [ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT].into_iter().all(|a| t.modifiers(&sde.dogma, a).next().is_none())) {
        if total >= EXTRACTION_MINIMUM && remaining == 0 { break; }
        let points = formulas::sp_for_level(training::skill_type(&sde, t.type_id).rank, 5);
        if points <= 0 { continue; }
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,5,$3)", &[&f.character, &t.type_id, &points]).await.unwrap();
        total += points;
        let take = remaining.min(points);
        let low_level = formulas::level_from_points(points - take, training::skill_type(&sde, t.type_id).rank);
        if take > 0 && !super::super::super::requirements::requirements(&sde, ship_type).chain(super::super::super::requirements::requirements(&sde, module_type)).chain(super::super::super::requirements::requirements(&sde, skill)).any(|(id, level)| id == t.type_id && level > low_level) {
            selected.insert(t.type_id, take);
            remaining -= take;
        }
    }
    assert!(total >= EXTRACTION_MINIMUM && remaining == 0);
    for &module in &modules { assert!(dogma::set_override(&db, module, resource, usage).await.unwrap()); }
    db.execute("UPDATE game.items SET online=true WHERE item_id=ANY($1)", &[&&modules[..]]).await.unwrap();
    let (load, output) = if resource == ATTR_CPU { (ATTR_CPU_LOAD, ATTR_CPU_OUTPUT) } else { (ATTR_POWER_LOAD, ATTR_POWER_OUTPUT) };
    for &ship in &ships { let fit = Fit::load(&db, sde.clone(), f.character, ship).await.ok().unwrap(); assert!(fit.value(Key::Ship, load) <= fit.value(Key::Ship, output)); }
    let item = transfers::insert(&db, EXTRACTOR, f.character, station, FLAG_HANGAR, 1).await.unwrap();
    let _lock = crate::services::market::lock().await;
    let before = super::fingerprint(&db, f.character).await;
    let rolled_back: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone(); let selected = selected.clone(); let ships = ships.clone(); let receiver = receiver.clone(); let fences = fences.clone();
        Box::pin(async move {
            extract_tx(tx, outbox, &sde, &fences, f.character, Some(ships[0]), item, crate::client::filetime_now(), &selected).await?;
            for ship in ships { assert!(dogma::online(tx, ship).await.map_err(db_err)?.is_empty()); }
            assert!(receiver.lock().unwrap().try_recv().is_err());
            tx.query_one("SELECT 1 / 0", &[]).await.map_err(db_err)?;
            Ok(())
        })
    }, |(), _| async move { panic!("rolled-back extraction dispatched") }).await;
    assert!(rolled_back.is_err());
    assert_eq!(super::fingerprint(&db, f.character).await, before);
    assert!(receiver.lock().unwrap().try_recv().is_err());
    for &ship in &ships { assert_eq!(dogma::online(&db, ship).await.unwrap().len(), 1); }
    let active_ship = ships[0];
    let events = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone(); let selected = selected.clone(); let fences = fences.clone();
        Box::pin(async move { extract_tx(tx, outbox, &sde, &fences, f.character, Some(active_ship), item, crate::client::filetime_now(), &selected).await })
    }, |_, outbox| { let world = &c.world; async move {
        let mut notifications = Vec::new();
        online::dispatch_entries((), outbox, world, |notification| {
            if let crate::db::outbox::Notification::Pilot { event, args, .. } = notification {
                notifications.push((event, args));
            }
            Ok(())
        })?;
        Ok(notifications)
    }}).await.unwrap_or_else(|_| panic!("extraction failed"));
    for &ship in &ships { assert!(dogma::online(&db, ship).await.unwrap().is_empty()); }
    for &module in &modules {
        assert!(events.iter().any(|(event, args)| event == "OnGodmaShipEffect" && args.first().and_then(Value::as_int) == Some(module) && args.get(3).and_then(Value::as_int) == Some(0)));
        assert!(events.iter().any(|(event, args)| event == "OnModuleAttributeChanges" && matches!(args.as_slice(), [Value::List(changes)] if changes.iter().any(|change| matches!(change, Value::Tuple(row) if row.get(2) == Some(&Value::Int(module)) && row.get(3) == Some(&Value::Int(i64::from(infinity_core::consts::dogma::ATTR_IS_ONLINE))) && row.get(5) == Some(&Value::Int(0)))))));
    }
    c.world.unregister_pilot_generation(f.character, None);
    }).await;
    crate::scheduler::cancel(&db, &format!("{}:{}", crate::services::skills::completion::KIND, f.character)).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
}
