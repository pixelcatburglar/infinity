//! Implant destruction must reconcile every owned online hull atomically.
use super::super::*;
use super::fixtures::*;
use infinity_core::{consts::{dogma::ATTR_CPU, dogma::ATTR_CPU_LOAD, dogma::ATTR_CPU_OUTPUT, dogma::ATTR_POWER, dogma::ATTR_POWER_LOAD, dogma::ATTR_POWER_OUTPUT, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_ONLINE, FLAG_HI_SLOT_FIRST}};
use crate::{db::outbox::{Effect, Notification}, services::skills::{formulas, training}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn implant_destruction_reconciles_owned_hulls_and_rolls_back() {
    let _market = crate::services::market::lock().await;
    let (db, f, mut c, _peer) = setup().await;
    let sde = c.sde.clone();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let receiver = Arc::new(std::sync::Mutex::new(receiver));
    c.world.register_pilot(f.character, sender);
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP) && t.attribute(14).is_some_and(|v| v >= 1.0)).unwrap().type_id;
    let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == 12) && t.effects().iter().any(|e| i64::from(e.effect_id) == EFFECT_ONLINE)).unwrap().type_id;
    let mut ships = Vec::new();
    let mut modules = Vec::new();
    for _ in 0..2 {
        let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,0,1,true) RETURNING item_id", &[&ship_type, &f.character, &STATION]).await.unwrap().get("item_id");
        let module: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,false) RETURNING item_id", &[&module_type, &f.character, &ship, &FLAG_HI_SLOT_FIRST]).await.unwrap().get("item_id");
        for attr in [ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT] { assert!(dogma::set_override(&db, ship, attr, 1000.0).await.unwrap()); }
        for attr in [ATTR_CPU, ATTR_POWER] { assert!(dogma::set_override(&db, module, attr, 0.0).await.unwrap()); }
        ships.push(ship); modules.push(module);
    }
    c.session.apply(vec![("shipid", Value::Int(ships[0]))]);
    let (implant_type, output) = sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER) && t.attribute(ATTR_IMPLANTNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0)).find_map(|t| {
        [ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT].into_iter().find(|&attr| t.modifiers(&sde.dogma, attr).any(|(effect, m)| effect.effect_category == Some(0) && m.func == "ItemModifier" && m.domain == "shipID" && m.op == crate::services::fit::OP_POST_PERCENT && t.attribute(m.modifying).is_some_and(|v| v > 0.0))).map(|attr| (t.type_id, attr))
    }).expect("published implant with a positive ship resource modifier");
    for (skill, level) in requirements(&sde, implant_type) {
        let points = formulas::sp_for_level(training::skill_type(&sde, skill).rank, i64::from(level));
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,$4) ON CONFLICT (character_id,type_id) DO UPDATE SET level=EXCLUDED.level,skill_points=EXCLUDED.skill_points", &[&f.character, &skill, &level, &points]).await.unwrap();
    }
    let before_fit = Fit::load(&db, sde.clone(), f.character, ships[0]).await.ok().unwrap().value(Key::Ship, output);
    let implant = item(&db, f.character, implant_type, STATION, 1).await;
    let now = filetime_now();
    super::fixtures::activate(&db, &sde, f.character, implant, None, now, false, vec![]).await.unwrap_or_else(|_| panic!("implant activation failed"));
    let after_fit = Fit::load(&db, sde.clone(), f.character, ships[0]).await.ok().unwrap().value(Key::Ship, output);
    assert!(after_fit > before_fit);
    let (resource, load) = if output == ATTR_CPU_OUTPUT { (ATTR_CPU, ATTR_CPU_LOAD) } else { (ATTR_POWER, ATTR_POWER_LOAD) };
    for &module in &modules { assert!(dogma::set_override(&db, module, resource, (before_fit + after_fit) / 2.0).await.unwrap()); }
    db.execute("UPDATE game.items SET online=true WHERE item_id=ANY($1)", &[&&modules[..]]).await.unwrap();
    for &ship in &ships { let fit = Fit::load(&db, sde.clone(), f.character, ship).await.ok().unwrap(); assert!(fit.value(Key::Ship, load) <= fit.value(Key::Ship, output)); }
    let before = fingerprint(&db, f.character).await;
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let fences = crate::services::effects::online::capture_online_fences(&world, &[f.character]).await;
    let rolled_back: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone(); let ships = ships.clone(); let receiver = receiver.clone(); let fences = fences.clone();
        Box::pin(async move {
            destroy_tx(tx, outbox, &sde, &fences, f.character, implant, now).await?;
            for ship in ships { assert!(dogma::online(tx, ship).await.map_err(db_err)?.is_empty()); }
            assert!(receiver.lock().unwrap().try_recv().is_err());
            tx.query_one("SELECT 1 / 0", &[]).await.map_err(|error| db_err(crate::db::Error::from(error)))?;
            Ok(())
        })
    }, |(), _| async move { panic!("rolled-back destruction dispatched") }).await;
    match rolled_back {
        Err(CallError::Internal(error)) => match error.downcast_ref::<crate::db::Error>() {
            Some(crate::db::Error::Postgres(error)) => assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)),
            _ => panic!("expected a PostgreSQL division-by-zero error"),
        },
        _ => panic!("expected a PostgreSQL division-by-zero error"),
    }
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert!(receiver.lock().unwrap().try_recv().is_err());
    for &ship in &ships { assert_eq!(dogma::online(&db, ship).await.unwrap().len(), 1); }
    let (_, effects) = destroy(&db, &sde, f.character, implant, now).await.unwrap_or_else(|_| panic!("implant destruction failed"));
    for &ship in &ships { assert!(dogma::online(&db, ship).await.unwrap().is_empty()); }
    for &module in &modules {
        assert!(effects.iter().any(|entry| matches!(entry, Effect::Notification(Notification::Pilot { event, args, .. }) if event == "OnGodmaShipEffect" && args.first().and_then(Value::as_int) == Some(module) && args.get(3).and_then(Value::as_int) == Some(0))));
        assert!(effects.iter().any(|entry| matches!(entry, Effect::Notification(Notification::Pilot { event, args, .. }) if event == "OnModuleAttributeChanges" && matches!(args.as_slice(), [Value::List(changes)] if changes.iter().any(|change| matches!(change, Value::Tuple(row) if row.get(2) == Some(&Value::Int(module)) && row.get(3) == Some(&Value::Int(i64::from(infinity_core::consts::dogma::ATTR_IS_ONLINE))) && row.get(5) == Some(&Value::Int(0))))))));
    }
    c.world.unregister_pilot_generation(f.character, None);
    cleanup(&db, f).await;
}
