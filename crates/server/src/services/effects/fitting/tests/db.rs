//! Opt-in real database regressions for fitting and online transitions.
#[path = "fixture.rs"] mod fixture;
mod scenarios;
mod synchronization;

use std::sync::Arc;
use infinity_core::{
    consts::{dogma::ATTR_CPU, dogma::ATTR_CPU_LOAD, dogma::ATTR_CPU_OUTPUT, dogma::ATTR_POWER, dogma::ATTR_POWER_OUTPUT, dogma::ATTR_REQUIRED_SKILLS, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_ONLINE, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST, GM_ROLES},
};
use crate::{
    client::Client,
    dao::items::dogma as item_dogma,
    sde::Sde,
    services::{self, Call, CallError, fit::{Fit, Key}, inventory::{self, access::Container}},
    space::World,
};
use marshal::Value;

fn offline_attribute(payload: &[Value], owner: i64, item: i64) -> bool {
    let [Value::List(changes)] = payload else { return false };
    changes.iter().any(|change| {
        let Value::Tuple(row) = change else { return false };
        row.get(1) == Some(&Value::Int(owner))
            && row.get(2) == Some(&Value::Int(item))
            && row.get(3) == Some(&Value::Int(i64::from(infinity_core::consts::dogma::ATTR_IS_ONLINE)))
            && row.get(5) == Some(&Value::Int(0))
            && row.get(6) == Some(&Value::Int(1))
    })
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn fitting_online_priming_and_slash() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().expect("station in SDE").station_id;
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(14).is_some_and(|v| v >= 2.0)).expect("published ship with two high slots").type_id;
    let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == 12)
        && t.effects().iter().any(|e| i64::from(e.effect_id) == EFFECT_ONLINE)
        && t.attribute(ATTR_CPU).is_some_and(|v| v > 0.0)
        && super::super::CAN_FIT_GROUPS.iter().chain(super::super::CAN_FIT_TYPES.iter()).all(|a| t.attribute(*a).unwrap_or(0.0) == 0.0)
        && !t.effects().iter().any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_TURRET_FITTED || e.effect_id == infinity_core::consts::dogma::EFFECT_LAUNCHER_FITTED)
        && t.attribute(super::super::MAX_GROUP_FITTED).unwrap_or(0.0) == 0.0
        && t.attribute(super::super::MAX_TYPE_FITTED).unwrap_or(0.0) == 0.0).expect("unrestricted high-slot online module").type_id;
    let required: Vec<_> = ATTR_REQUIRED_SKILLS.iter().filter_map(|a| sde.types.get(module_type).and_then(|t| t.attribute(*a)).filter(|v| *v > 0.0).map(|v| (v as i32, 5))).collect();
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, ship_type, module_type).await.unwrap();
    db.execute("UPDATE game.items SET location_id=$2, flag_id=$3 WHERE item_id=$1", &[&f.ship, &station, &infinity_core::consts::FLAG_HANGAR]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_db = db.clone();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let mut client = Client::new(crate::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), test_db.clone(), sde.clone(),
            crate::chat::Hub::new("localhost", test_db.clone(), test_world.clone()), test_world.clone(), crate::handshake::Login { user_id: f.account, user_name: "fitting-fixture".into(), role: GM_ROLES, client_id: 1, session_id: 1, language_id: "EN".into() });
        client.session.apply(vec![("charid", Value::Int(f.owner)), ("shipid", Value::Int(f.ship)), ("stationid", Value::Int(station))]);
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = notify;
        let drone = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_DRONE)
            && ATTR_REQUIRED_SKILLS.iter().zip(infinity_core::consts::dogma::ATTR_REQUIRED_SKILL_LEVELS).any(|(&skill, level)|
                t.attribute(skill).unwrap_or(0.0) > 0.0 && t.attribute(level).unwrap_or(0.0) > 0.0))
            .expect("published drone with skill prerequisites");
        let drone_type = drone.type_id;
        let drone_skills = ATTR_REQUIRED_SKILLS.iter().zip(infinity_core::consts::dogma::ATTR_REQUIRED_SKILL_LEVELS)
            .filter_map(|(&skill, level)| {
                let skill = drone.attribute(skill).unwrap_or(0.0) as i32;
                let level = drone.attribute(level).unwrap_or(0.0) as i32;
                (skill > 0 && level > 0).then_some((skill, level))
            }).collect();
        let mut drone_fit = Fit::load(&test_db, sde.clone(), f.owner, f.ship).await.ok().unwrap();
        drone_fit.include_drone(f.modules[0], drone_type, vec![]);
        assert!(matches!(super::super::required_skills(&drone_fit, &sde, Key::Drone(f.modules[0]), drone_type, false),
            Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_FITTING_HAS_SKILL_PREREQUISITES));
        services::skills::gm::apply(&mut client, f.owner, services::skills::gm::Change::Set(drone_skills)).await.ok().expect("seed drone prerequisites");
        let mut drone_fit = Fit::load(&test_db, sde.clone(), f.owner, f.ship).await.ok().unwrap();
        drone_fit.include_drone(f.modules[0], drone_type, vec![]);
        assert!(super::super::required_skills(&drone_fit, &sde, Key::Drone(f.modules[0]), drone_type, false).is_ok());
        services::skills::gm::apply(&mut client, f.owner, services::skills::gm::Change::Set(required)).await.ok().expect("seed fitting prerequisites");
        while events.try_recv().is_ok() {}
        for (id, attr, value) in [(f.ship, ATTR_CPU_OUTPUT, 0.0), (f.ship, ATTR_POWER_OUTPUT, 1000.0), (f.modules[0], ATTR_CPU, 10.0), (f.modules[0], ATTR_POWER, 0.0)] {
            assert!(item_dogma::set_override(&test_db, id, attr, value).await.unwrap());
        }
        let destination = Container::Item(f.ship);
        assert_eq!(inventory::movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, f.modules[0], Some(f.ship), Some(FLAG_HI_SLOT_FIRST), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.ok().flatten(), Some(f.modules[0]));
        synchronization::installed_module_follows_inventory(&mut events, f.owner, f.modules[0]);
        let mut fit = Fit::load(&test_db, sde.clone(), f.owner, f.ship).await.ok().unwrap();
        let offline_load = fit.value(Key::Ship, ATTR_CPU_LOAD);
        let need = fit.value(Key::Module(f.modules[0]), ATTR_CPU);
        fit.set_module_online(f.modules[0], true);
        assert!((fit.value(Key::Ship, ATTR_CPU_LOAD) - offline_load - need).abs() < 1e-6);
        fit.set_module_online(f.modules[0], false);
        assert!((fit.value(Key::Ship, ATTR_CPU_LOAD) - offline_load).abs() < 1e-6);
        match services::effects::online::set_online(&services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.notify_tx.clone(), client.login.user_id, f.ship, f.modules[0], true, |notification| services::effects::online::dispatch_notification(notification, &client.world, &client.notify_tx, client.session.int("charid"))).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, "NotEnoughCpu");
                for (key, value) in [("moduleName", Value::tuple([Value::Int(4), Value::Int(i64::from(module_type))])), ("moduleType", Value::Int(i64::from(module_type))), ("require", Value::Float(need)), ("remaining", Value::Float(0.0)), ("total", Value::Float(0.0))] {
                    assert!(dict.iter().any(|(k, v)| k == key && *v == value));
                }
            }
            _ => panic!("expected NotEnoughCpu"),
        }
        assert!(item_dogma::online(&test_db, f.ship).await.unwrap().is_empty());
        assert!(events.try_recv().is_err());
        assert!(item_dogma::set_override(&test_db, f.ship, ATTR_CPU_OUTPUT, 1000.0).await.unwrap());
        assert!(item_dogma::set_override(&test_db, f.ship, ATTR_POWER_OUTPUT, 0.0).await.unwrap());
        assert!(item_dogma::set_override(&test_db, f.modules[0], ATTR_POWER, 10.0).await.unwrap());
        let power_fit = Fit::load(&test_db, sde.clone(), f.owner, f.ship).await.ok().unwrap();
        let power_need = power_fit.value(Key::Module(f.modules[0]), ATTR_POWER);
        match services::effects::online::set_online(&services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.notify_tx.clone(), client.login.user_id, f.ship, f.modules[0], true, |notification| services::effects::online::dispatch_notification(notification, &client.world, &client.notify_tx, client.session.int("charid"))).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, "NotEnoughPower");
                for (key, value) in [("moduleName", Value::tuple([Value::Int(4), Value::Int(i64::from(module_type))])), ("moduleType", Value::Int(i64::from(module_type))), ("require", Value::Float(power_need)), ("remaining", Value::Float(0.0)), ("total", Value::Float(0.0))] {
                    assert!(dict.iter().any(|(k, v)| k == key && *v == value));
                }
            }
            _ => panic!("expected NotEnoughPower"),
        }
        assert!(item_dogma::online(&test_db, f.ship).await.unwrap().is_empty());
        assert!(events.try_recv().is_err());
        assert!(item_dogma::set_override(&test_db, f.ship, ATTR_POWER_OUTPUT, power_need).await.unwrap());
        assert!(inventory::movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, f.modules[1], Some(f.ship), Some(FLAG_HI_SLOT_FIRST), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
        assert!(inventory::movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, f.modules[1], Some(f.ship), Some(FLAG_MED_SLOT_FIRST), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
        let row = test_db.query_one("SELECT location_id, flag_id FROM game.items WHERE item_id=$1", &[&f.modules[1]]).await.unwrap();
        assert_eq!(row.get::<_, i64>("location_id"), f.ship);
        assert_eq!(row.get::<_, i32>("flag_id"), infinity_core::consts::FLAG_CARGO);
        assert!(events.try_recv().is_err());
        assert!(item_dogma::set_override(&test_db, f.ship, ATTR_CPU_OUTPUT, need).await.unwrap());
        services::effects::online::set_online(&services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.notify_tx.clone(), client.login.user_id, f.ship, f.modules[0], true, |notification| services::effects::online::dispatch_notification(notification, &client.world, &client.notify_tx, client.session.int("charid"))).await.ok().expect("exact CPU boundary");
        while events.try_recv().is_ok() {}
        let legal_before = fixture::fingerprint(&test_db).await.unwrap();
        let all_args = [Value::Bool(false), Value::Bool(false)];
        let all_kwargs = Value::None;
        let all_info = Call { service: "dogmaIM", method: "GetAllInfo", args: &all_args, kwargs: &all_kwargs, bound: false, bind_params: None };
        services::dispatch(&mut client, &all_info).await.ok().unwrap();
        let kwargs = Value::Dict(vec![]);
        let info = Call { service: "dogmaIM", method: "ShipGetInfo", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        services::dogma::dispatch(&mut client, &info).await.ok().unwrap();
        assert_eq!(item_dogma::online(&test_db, f.ship).await.unwrap().len(), 1);
        assert_eq!(fixture::fingerprint(&test_db).await.unwrap(), legal_before);
        assert!(events.try_recv().is_err());
        let args = [Value::str("/dogma ship cpuOutput 0")];
        let command = Call { service: "slash", method: "SlashCmd", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        services::slash::dispatch(&mut client, &command).await.expect("slash dispatch").ok().expect("dogma command");
        assert!(item_dogma::online(&test_db, f.ship).await.unwrap().is_empty());
        let mut effect = false;
        let mut attribute = false;
        while let Ok((name, payload)) = events.try_recv() {
            if name == "OnGodmaShipEffect" && payload.first() == Some(&Value::Int(f.modules[0])) && payload.get(3).and_then(Value::as_int) == Some(0) { effect = true; }
            if name == "OnModuleAttributeChanges" && offline_attribute(&payload, f.owner, f.modules[0]) { attribute = true; }
        }
        assert!(effect && attribute);
        assert!(item_dogma::set_override(&test_db, f.ship, ATTR_CPU_OUTPUT, 1000.0).await.unwrap());
        assert!(item_dogma::set_override(&test_db, f.ship, ATTR_POWER_OUTPUT, 1000.0).await.unwrap());
        assert_eq!(inventory::movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, f.modules[1], Some(f.ship), Some(FLAG_HI_SLOT_FIRST + 1), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.ok().flatten(), Some(f.modules[1]));
        for (id, attr, value) in [(f.modules[0], ATTR_CPU, 20.0), (f.modules[1], ATTR_CPU, 10.0), (f.modules[0], ATTR_POWER, 0.0), (f.modules[1], ATTR_POWER, 0.0), (f.ship, ATTR_CPU_OUTPUT, 15.0)] {
            assert!(item_dogma::set_override(&test_db, id, attr, value).await.unwrap());
        }
        assert_eq!(test_db.execute("UPDATE game.items SET online=true WHERE item_id=ANY($1) AND owner_id=$2 AND location_id=$3", &[&&f.modules[..], &f.owner, &f.ship]).await.unwrap(), 2);
        while events.try_recv().is_ok() {}
        services::dispatch(&mut client, &all_info).await.ok().unwrap();
        let online = item_dogma::online(&test_db, f.ship).await.unwrap();
        assert_eq!(online.len(), 1);
        assert_eq!(online[0].item_id, f.modules[1]);
        let mut stopped = false;
        let mut changed = false;
        while let Ok((name, payload)) = events.try_recv() {
            if name == "OnGodmaShipEffect" && payload.first() == Some(&Value::Int(f.modules[0])) && payload.get(3).and_then(Value::as_int) == Some(0) { stopped = true; }
            if name == "OnModuleAttributeChanges" && offline_attribute(&payload, f.owner, f.modules[0]) { changed = true; }
        }
        assert!(stopped && changed);
        let fences = services::effects::online::capture_online_fences(&client.world, &[f.owner]).await;
        let rollback: Result<(), CallError> = test_db.transaction(
            |tx, outbox: &mut crate::db::outbox::Outbox<services::effects::online::OnlineChanged>| {
                let sde = sde.clone();
                let fences = fences.clone();
                Box::pin(async move {
                    item_dogma::set_override(tx, f.ship, ATTR_CPU_OUTPUT, 0.0).await.map_err(services::db_err)?;
                    services::effects::online::reconcile_core(tx, &sde, outbox, &fences, f.owner, f.ship).await?;
                    Err(CallError::Internal(anyhow::anyhow!("fixture rollback")))
                })
            },
            |(), _| async move { panic!("rollback dispatched") },
        ).await;
        assert!(rollback.is_err());
        assert_eq!(item_dogma::online(&test_db, f.ship).await.unwrap().len(), 1);
        assert_eq!(item_dogma::overrides(&test_db, f.ship).await.unwrap().iter().find(|r| r.attribute_id == ATTR_CPU_OUTPUT).unwrap().value, 15.0);
        assert!(events.try_recv().is_err());
        let rig_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
            && t.effects().iter().any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_RIG_SLOT)
            && super::super::CAN_FIT_GROUPS.iter().chain(super::super::CAN_FIT_TYPES.iter()).all(|a| t.attribute(*a).unwrap_or(0.0) == 0.0)
            && t.attribute(super::super::MAX_GROUP_FITTED).unwrap_or(0.0) == 0.0
            && t.attribute(super::super::MAX_TYPE_FITTED).unwrap_or(0.0) == 0.0).expect("unrestricted rig").type_id;
        let rig = sde.types.get(rig_type).unwrap();
        let rig_size = rig.attribute(super::super::RIG_SIZE).unwrap_or(0.0);
        let rig_skills = ATTR_REQUIRED_SKILLS.iter().filter_map(|a| rig.attribute(*a).filter(|v| *v > 0.0).map(|v| (v as i32, 5))).collect();
        services::skills::gm::apply(&mut client, f.owner, services::skills::gm::Change::Set(rig_skills)).await.ok().expect("seed rig prerequisites");
        for (attr, value) in [(1137, 2.0), (super::super::RIG_SIZE, rig_size), (infinity_core::consts::dogma::ATTR_UPGRADE_CAPACITY, 1_000_000.0)] {
            assert!(item_dogma::set_override(&test_db, f.ship, attr, value).await.unwrap());
        }
        for index in 0..2 {
            let id: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,false) RETURNING item_id", &[&rig_type,&f.owner,&f.ship,&infinity_core::consts::FLAG_CARGO]).await.unwrap().get("item_id");
            assert_eq!(inventory::movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, id, Some(f.ship), Some(infinity_core::consts::FLAG_RIG_FIRST + index), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.ok().flatten(), Some(id), "fitting rig slot {index}");
        }
        while events.try_recv().is_ok() {}
        scenarios::exercise(&mut client, &f, &sde, &mut events).await;
    }).await;
    let owned_ids: Vec<i64> = db.query("SELECT item_id FROM game.items WHERE owner_id=$1", &[&f.owner]).await.unwrap().iter().map(|row| row.get("item_id")).collect();
    let progress: Vec<String> = db.query("SELECT event_id::text AS event_id FROM game.progress_events WHERE subject_character_id=$1 OR (kind='WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint=ANY($2)) OR (kind='ItemsMoved' AND (payload->'ItemsMoved'->>'item_id')::bigint=ANY($3))", &[&f.owner, &&[f.account, f.owner][..], &owned_ids]).await.unwrap().iter().map(|row| row.get("event_id")).collect();
    let cleaned = fixture::cleanup(&db, &world, f).await;
    let after = fixture::fingerprint(&db).await;
    cleaned.unwrap();
    let remaining: i64 = db.query_one("SELECT count(*) AS count FROM game.progress_events WHERE subject_character_id=$1 OR (kind='WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint=ANY($2)) OR (kind='ItemsMoved' AND (payload->'ItemsMoved'->>'item_id')::bigint=ANY($3))", &[&f.owner, &&[f.account, f.owner][..], &owned_ids]).await.unwrap().get("count");
    assert_eq!(remaining, 0, "fixture progress events remain");
    let deliveries: i64 = db.query_one("SELECT count(*) AS count FROM game.progress_deliveries WHERE event_id::text=ANY($1)", &[&progress]).await.unwrap().get("count");
    assert_eq!(deliveries, 0, "fixture progress deliveries remain");
    assert_eq!(after.unwrap(), before);
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn reconciliation_offlines_only_modules_with_missing_prerequisites() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let requirements = |ty| services::skills::requirements::requirements(&sde, ty);
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(14).is_some_and(|v| v >= 2.0)).expect("published ship with two high slots").type_id;
    let is_online_module = |t: &crate::sde::types::TypeData| {
        t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
            && t.effects().iter().any(|e| e.effect_id == 12)
            && t.effects().iter().any(|e| i64::from(e.effect_id) == EFFECT_ONLINE)
    };
    let dependent = sde.types.iter().find(|t| is_online_module(t) && requirements(t.type_id).next().is_some())
        .expect("high-slot online module with prerequisites").type_id;
    let mut needed: Vec<_> = requirements(dependent).collect();
    let lost_skill = needed[0].0;
    let independent = sde.types.iter().find(|t| is_online_module(t) && requirements(t.type_id).all(|(skill, _)| skill != lost_skill))
        .expect("high-slot online module not requiring the lost skill").type_id;
    needed.extend(requirements(independent));
    let f = fixture::create(&db, ship_type, dependent).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_db = db.clone();
    let test_sde = sde.clone();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        for (skill, level) in needed {
            test_db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0) ON CONFLICT (character_id,type_id) DO UPDATE SET level=GREATEST(game.skills.level,EXCLUDED.level)", &[&f.owner, &skill, &level]).await.unwrap();
        }
        for (index, id) in f.modules.into_iter().enumerate() {
            let ty = if index == 0 { dependent } else { independent };
            let flag = FLAG_HI_SLOT_FIRST + index as i32;
            assert_eq!(test_db.execute("UPDATE game.items SET type_id=$2,flag_id=$3,online=true WHERE item_id=$1", &[&id, &ty, &flag]).await.unwrap(), 1);
            for attr in [ATTR_CPU, ATTR_POWER] {
                assert!(item_dogma::set_override(&test_db, id, attr, 0.0).await.unwrap());
            }
        }
        for attr in [ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT] {
            assert!(item_dogma::set_override(&test_db, f.ship, attr, 1_000_000.0).await.unwrap());
        }
        let reconcile = || services::effects::online::reconcile_ship(&test_db, &test_world, &test_sde, f.owner, f.ship, |_| Ok(()));
        reconcile().await.ok().expect("reconcile with prerequisites");
        let online = item_dogma::online(&test_db, f.ship).await.unwrap();
        assert_eq!(online.len(), 2, "both modules must remain online with prerequisites");
        assert_eq!(test_db.execute("DELETE FROM game.skills WHERE character_id=$1 AND type_id=$2", &[&f.owner, &lost_skill]).await.unwrap(), 1);
        reconcile().await.ok().expect("reconcile after skill loss");
        let online = item_dogma::online(&test_db, f.ship).await.unwrap();
        assert_eq!(online.len(), 1);
        assert_eq!(online[0].item_id, f.modules[1], "only the module with the missing prerequisite should go offline");
    }).await;
    fixture::cleanup(&db, &world, f).await.unwrap();
    result.unwrap();
}
