//! Scheduler expiry reconciles fitting resources in the committing transaction.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use infinity_core::{
    consts::{dogma::ATTR_BOOSTER_DURATION, dogma::ATTR_BOOSTERNESS, dogma::ATTR_CPU, dogma::ATTR_CPU_LOAD, dogma::ATTR_CPU_OUTPUT, dogma::ATTR_POWER, dogma::ATTR_POWER_LOAD, dogma::ATTR_POWER_OUTPUT, CATEGORY_IMPLANT, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_ONLINE, FLAG_HANGAR, FLAG_HI_SLOT_FIRST, GROUP_BOOSTER},
};
use crate::{
    dao::items::dogma,
    db::outbox::Outbox,
    services::{effects::online::{self, OnlineChanged, OnlinePolicy}, fit::{Fit, Key}, skills::{brain_items, requirements::requirements}},
};

/// Current static data has no booster with both native duration and a CPU/PG
/// resource modifier; a supported per-item duration override calibrates this fixture.
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn scheduled_booster_expiry_reconciles_online_fit_after_commit() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let test_db = db.clone();
    let world = c.world.clone();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let sde = c.sde.clone();
        let station: i64 = 60000001;
        db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
        let booster = sde.types.iter().find(|t| t.published == Some(true)
            && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id == Some(GROUP_BOOSTER)
            && t.attribute(ATTR_BOOSTERNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0)
            && [ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT].into_iter().any(|attr|
                t.modifiers(&sde.dogma, attr).any(|(effect, m)| effect.effect_category == Some(0) && m.func == "ItemModifier" && m.domain == "shipID" && m.op == crate::services::fit::OP_POST_PERCENT && t.attribute(m.modifying).is_some_and(|v| v > 0.0))))
            .expect("published resource booster").type_id;
        let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
            && t.attribute(14).is_some_and(|v| v >= 1.0)).expect("published ship with a high slot").type_id;
        let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
            && t.effects().iter().any(|e| e.effect_id == 12)
            && t.effects().iter().any(|e| i64::from(e.effect_id) == EFFECT_ONLINE)
            && t.attribute(ATTR_CPU).is_some_and(|v| v > 0.0)
            && !t.effects().iter().any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_TURRET_FITTED || e.effect_id == infinity_core::consts::dogma::EFFECT_LAUNCHER_FITTED)).expect("high-slot module").type_id;
        for (skill, level) in requirements(&sde, booster).chain(requirements(&sde, module_type)) {
            let points = crate::services::skills::formulas::sp_for_level(crate::services::skills::training::skill_type(&sde, skill).rank, i64::from(level));
            training::set_level(&db, f.character, skill, level, points).await.unwrap();
        }
        let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&ship_type, &f.character, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let module: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,false) RETURNING item_id", &[&module_type, &f.character, &ship, &FLAG_HI_SLOT_FIRST]).await.unwrap().get("item_id");
        let item = crate::dao::items::transfers::insert(&db, booster, f.character, station, FLAG_HANGAR, 1).await.unwrap();
        assert_eq!(db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&item]).await.unwrap(), 1);
        assert!(dogma::set_override(&db, item, ATTR_BOOSTER_DURATION, 3_600_000.0).await.unwrap());
        for (id, attr, value) in [(ship, ATTR_CPU_OUTPUT, 1000.0), (ship, ATTR_POWER_OUTPUT, 1000.0), (module, ATTR_CPU, 0.0), (module, ATTR_POWER, 0.0)] {
            assert!(dogma::set_override(&db, id, attr, value).await.unwrap());
        }
        let base = Fit::load(&db, sde.clone(), f.character, ship).await.ok().expect("base fit");
        let before = [base.value(Key::Ship, ATTR_CPU_OUTPUT), base.value(Key::Ship, ATTR_POWER_OUTPUT)];
        let mut draws: Vec<_> = sde.types.effects(booster).iter().filter_map(|e| {
            let chance = sde.dogma.effect(e.effect_id)?.fitting_usage_chance_attribute_id?;
            sde.types.attribute(booster, chance)?;
            Some((e.effect_id, 0.0))
        }).collect();
        draws.sort_unstable_by_key(|v| v.0);
        draws.dedup_by_key(|v| v.0);
        let now = filetime_now();
        let fences = online::capture_online_fences(&test_world, &[f.character]).await;
        {
            let _market = crate::services::market::lock().await;
            db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
                let sde = sde.clone(); let draws = draws.clone(); let fences = fences.clone();
                Box::pin(async move { brain_items::activate_tx(tx, outbox, &sde, &fences, f.character, None, item, Some(station), now, &draws, true).await })
            }, |value, outbox| { let client = &c; async move { online::dispatch_changed(value, outbox, client) } }).await.unwrap_or_else(|error| match error {
                CallError::Internal(error) => panic!("booster activation: {error:#}"),
                CallError::User { msg, .. } => panic!("booster activation refused: {msg}"),
            });
        }
        let boosted = Fit::load(&db, sde.clone(), f.character, ship).await.ok().expect("boosted fit");
        let after = [boosted.value(Key::Ship, ATTR_CPU_OUTPUT), boosted.value(Key::Ship, ATTR_POWER_OUTPUT)];
        let index = (0..2).find(|&i| after[i] > before[i] + 1e-4).expect("booster increases ship CPU or power output");
        let (usage, load, output) = [(ATTR_CPU, ATTR_CPU_LOAD, ATTR_CPU_OUTPUT), (ATTR_POWER, ATTR_POWER_LOAD, ATTR_POWER_OUTPUT)][index];
        assert!(dogma::set_override(&db, module, usage, (before[index] + after[index]) / 2.0).await.unwrap());
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        test_world.register_pilot(f.character, sender.clone());
        let fences = online::capture_online_fences(&test_world, &[f.character]).await;
        let policy = OnlinePolicy { owner: f.character, active_ship: None, station: Some(station), notify: sender };
        {
            let _market = crate::services::market::lock().await;
            let changed = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
                let sde = sde.clone(); let policy = policy.clone(); let fences = fences.clone();
                Box::pin(async move {
                    let before = online::fitting::capture(tx, &sde, &fences, policy.owner, ship).await?;
                    let (changed, fit) = online::set_online_core(tx, &sde, &policy, outbox, &fences, ship, module, true).await?;
                    online::fitting::finish(tx, before, fit, outbox).await?;
                    Ok(changed)
                })
            }, |value, outbox| { let world = &test_world; async move { online::dispatch_changed_world(value, outbox, world) } }).await.ok().expect("online module");
            assert!(changed);
        }
        let legal = Fit::load(&db, sde.clone(), f.character, ship).await.ok().expect("online fit");
        assert!(legal.value(Key::Ship, load) <= legal.value(Key::Ship, output));
        assert_eq!(dogma::online(&db, ship).await.unwrap().len(), 1);
        while receiver.lock().unwrap().try_recv().is_ok() {}
        let old = brain::get(&db, f.character, item).await.unwrap().unwrap();
        let shift = crate::client::filetime_ticks_from_seconds(7200);
        let due = old.expires_at.unwrap() - shift;
        assert!(due <= filetime_now());
        assert!(brain::update_metadata(&db, f.character, &old, old.slot, old.activated_at.map(|v| v - shift), Some(due), old.effect_ids.as_deref()).await.unwrap());
        let expired_fit = Fit::load(&db, sde.clone(), f.character, ship).await.ok().expect("expired fit");
        assert!(expired_fit.value(Key::Ship, load) > expired_fit.value(Key::Ship, output));
        reschedule(&db, f.character).await.ok().expect("reschedule expiry");
        let dispatched = Arc::new(AtomicBool::new(false));
        let failed: Result<(), CallError> = {
            let _market = crate::services::market::lock().await;
            db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
                let sde = sde.clone(); let receiver = receiver.clone(); let fences = fences.clone();
                Box::pin(async move {
                    crate::dao::characters::selection::lock_character(tx, f.character).await.map_err(db_err)?;
                    let progress = advance_tx(tx, outbox, &sde, f.character, filetime_now()).await?;
                    finish_tx(tx, outbox, &sde, &fences, f.character, filetime_now(), &progress, &[]).await?;
                    assert!(brain::get(tx, f.character, item).await.map_err(db_err)?.is_none());
                    assert!(dogma::online(tx, ship).await.map_err(db_err)?.is_empty());
                    assert!(receiver.lock().unwrap().try_recv().is_err(), "notification before commit");
                    tx.query_one("SELECT 1 / 0", &[]).await.map_err(|e| db_err(crate::db::Error::from(e)))?;
                    Ok(())
                })
            }, |(), _| { let dispatched = dispatched.clone(); async move { dispatched.store(true, Ordering::SeqCst); Ok(()) } }).await
        };
        fixture::assert_division_by_zero(failed);
        assert!(!dispatched.load(Ordering::SeqCst));
        assert!(brain::get(&db, f.character, item).await.unwrap().is_some());
        assert_eq!(dogma::online(&db, ship).await.unwrap().len(), 1);
        assert!(receiver.lock().unwrap().try_recv().is_err());
        let job = Job { id: format!("{KIND}:{}", f.character), kind: KIND.into(), due_at: due, payload: f.character.to_string() };
        handle(db.clone(), test_world.clone(), job.clone()).await.unwrap();
        assert!(brain::get(&db, f.character, item).await.unwrap().is_none());
        assert!(dogma::online(&db, ship).await.unwrap().is_empty());
        let mut stopped = false;
        let mut removed = false;
        while let Ok((event, args)) = receiver.lock().unwrap().try_recv() {
            if event == "OnGodmaShipEffect" && args.first().and_then(Value::as_int) == Some(module) && args.get(3).and_then(Value::as_int) == Some(0) { stopped = true; }
            if event == "OnItemChange" { removed = true; }
        }
        assert!(stopped, "missing module offline notification");
        assert!(removed, "missing booster removal notification");
        handle(db.clone(), test_world.clone(), job).await.unwrap();
        assert!(brain::get(&db, f.character, item).await.unwrap().is_none());
        assert!(dogma::online(&db, ship).await.unwrap().is_empty());
        assert!(receiver.lock().unwrap().try_recv().is_err(), "retry emitted another notification");
    }).await;
    world.unregister_pilot_generation(f.character, None);
    scheduler::cancel(&db, &format!("{KIND}:{}", f.character)).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    result.unwrap();
}
