use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn stale_claim_after_service_renewal_preserves_nearest_job() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db.clone(), character).await;
        c.login.role = infinity_core::consts::ROLE_GML;
        let mut skins = c.sde.ship_skins.licenses().filter_map(|license| {
            let skin = c.sde.ship_skins.skin(license.skin_id)?;
            (!skin.is_structure_skin && skin.visible(&c.cfg.server.boot_region)).then_some(skin.skin_id)
        }).collect::<Vec<_>>();
        skins.sort_unstable(); skins.dedup();
        let first = skins[0]; let second = skins[1];
        let now = crate::client::filetime_now();
        let future = now + infinity_core::consts::FILETIME_DAY;
        assert!(ship_skins::insert_license(&test_db, character, first, Some(now - 1)).await.unwrap());
        assert!(ship_skins::insert_license(&test_db, character, second, Some(future)).await.unwrap());
        schedule(&test_db, character).await.unwrap();
        let job_id = id(character);
        let other_ids: Vec<String> = test_db.query("SELECT job_id FROM game.jobs WHERE job_id <> $1", &[&job_id]).await.unwrap().into_iter().map(|row| row.get("job_id")).collect();
        let excluded: Vec<&str> = other_ids.iter().map(String::as_str).collect();
        let claim = crate::dao::jobs::claim(&test_db, now, future, 1, &excluded).await.unwrap().into_iter().find(|claim| claim.job.id == job_id).unwrap();
        let args = [Value::Int(i64::from(first))];
        let kwargs = Value::dict([("duration", Value::Int(1))]);
        let call = crate::services::Call { service: "shipCosmeticsMgr", method: "GiveSkin", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(super::super::dispatch(&mut c, &call).await.unwrap().unwrap(), Value::None);
        let renewed = ship_skins::get_stored_license(&test_db, character, first).await.unwrap().unwrap().expires_at.unwrap();
        assert!(renewed > now);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        c.world.register_pilot(character, sender);
        handle(test_db.clone(), c.world.clone(), claim.job.clone()).await.unwrap();
        handle(test_db.clone(), c.world.clone(), claim.job).await.unwrap();
        assert!(receiver.try_recv().is_err());
        assert_eq!(ship_skins::get_stored_license(&test_db, character, first).await.unwrap().unwrap().expires_at, Some(renewed));
        assert_eq!(crate::dao::jobs::ack(&test_db, &job_id, claim.token).await.unwrap(), 0);
        assert_eq!(crate::dao::jobs::lock_job(&test_db, &job_id).await.unwrap().unwrap().due_at, future.min(renewed) + 1);
    }).await;
    crate::scheduler::cancel(&db, &id(character)).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn equality_boundary_removes_application_only_on_first_later_tick() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db.clone(), character).await;
        let station = c.sde.map.stations().next().unwrap().station_id;
        c.session.apply(vec![("stationid", Value::Int(station))]);
        let (skin, hull) = c.sde.ship_skins.licenses().find_map(|license| {
            let skin = c.sde.ship_skins.skin(license.skin_id)?;
            if skin.is_structure_skin || !skin.visible(&c.cfg.server.boot_region) { return None; }
            let hull = skin.type_ids.iter().copied().find(|id| c.sde.types.get(*id).is_some_and(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)))?;
            Some((skin.skin_id, hull))
        }).unwrap();
        let flag = infinity_core::consts::FLAG_HANGAR;
        let ship: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &character, &station, &flag]).await.unwrap().get("item_id");
        let deadline = crate::client::filetime_now() + infinity_core::consts::FILETIME_DAY;
        assert!(ship_skins::insert_license(&test_db, character, skin, Some(deadline)).await.unwrap());
        let later_skin = c.sde.ship_skins.licenses().find_map(|license| {
            let candidate = c.sde.ship_skins.skin(license.skin_id)?;
            (candidate.skin_id != skin && !candidate.is_structure_skin && candidate.visible(&c.cfg.server.boot_region)).then_some(candidate.skin_id)
        }).unwrap();
        let later = deadline + infinity_core::consts::FILETIME_DAY;
        assert!(ship_skins::insert_license(&test_db, character, later_skin, Some(later)).await.unwrap());
        assert_eq!(super::super::apply(&mut c, ship, Some(skin)).await.unwrap(), Value::None);
        let targets = prepare(&test_db, &c.world, character).await.unwrap();
        let mut outbox: Outbox = Outbox::default();
        expire_tx(&test_db, character, deadline, &targets, &mut outbox).await.unwrap();
        assert!(outbox.into_entries().is_empty());
        assert_eq!(ship_skins::applications_for_character(&test_db, character).await.unwrap().len(), 1);
        assert_eq!(crate::dao::jobs::lock_job(&test_db, &id(character)).await.unwrap().unwrap().due_at, deadline + 1);
        let mut outbox: Outbox = Outbox::default();
        expire_tx(&test_db, character, deadline + 1, &targets, &mut outbox).await.unwrap();
        assert!(!outbox.into_entries().is_empty());
        assert!(ship_skins::get_stored_license(&test_db, character, skin).await.unwrap().is_none());
        assert!(ship_skins::applications_for_character(&test_db, character).await.unwrap().is_empty());
        assert_eq!(crate::dao::jobs::lock_job(&test_db, &id(character)).await.unwrap().unwrap().due_at, later + 1);
        assert!(ship_skins::update_expiry(&test_db, character, later_skin, Some(later), None).await.unwrap());
        schedule(&test_db, character).await.unwrap();
        assert!(crate::dao::jobs::lock_job(&test_db, &id(character)).await.unwrap().is_none());
    }).await;
    crate::scheduler::cancel(&db, &id(character)).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn reopened_runner_and_startup_recovery_remove_overdue_applications() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, peer) = crate::services::tests::client(test_db.clone(), character).await;
        let station = c.sde.map.stations().next().unwrap().station_id;
        c.session.apply(vec![("stationid", Value::Int(station))]);
        let (skin, hull) = c.sde.ship_skins.licenses().find_map(|license| {
            let skin = c.sde.ship_skins.skin(license.skin_id)?;
            if skin.is_structure_skin || !skin.visible(&c.cfg.server.boot_region) { return None; }
            let hull = skin.type_ids.iter().copied().find(|id| c.sde.types.get(*id).is_some_and(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)))?;
            Some((skin.skin_id, hull))
        }).unwrap();
        let flag = infinity_core::consts::FLAG_HANGAR;
        let ship: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &character, &station, &flag]).await.unwrap().get("item_id");
        let future = crate::client::filetime_now() + infinity_core::consts::FILETIME_DAY;
        assert!(ship_skins::insert_license(&test_db, character, skin, Some(future)).await.unwrap());
        super::super::apply(&mut c, ship, Some(skin)).await.unwrap();
        assert!(ship_skins::update_expiry(&test_db, character, skin, Some(future), Some(crate::client::filetime_now() - 1)).await.unwrap());
        schedule(&test_db, character).await.unwrap();
        drop(c); drop(peer); drop(test_db);
        let reopened = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        assert!(ship_skins::get_stored_license(&reopened, character, skin).await.unwrap().is_some());
        assert_eq!(ship_skins::applications_for_character(&reopened, character).await.unwrap().len(), 1);
        let sde = Arc::new(crate::sde::Sde::load(&reopened).await.unwrap());
        let world = World::new(reopened.clone(), sde, 1);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        world.register_pilot(character, sender);
        let (shutdown, watch) = tokio::sync::watch::channel(false);
        let mut runner = scheduler::runner::start(reopened.clone(), world.clone(), crate::services::scheduler_registrations(), crate::config::scheduler::SchedulerConfig::default(), watch).unwrap();
        let notice = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some((event, args)) = receiver.recv().await {
                if event == "OnSkinLicenseRemoved" { return Some(args); }
            }
            None
        }).await;
        let _ = shutdown.send(true);
        let drained = tokio::time::timeout(std::time::Duration::from_secs(10), &mut runner).await;
        if drained.is_err() { runner.abort(); let _ = runner.await; }
        assert_eq!(notice.unwrap().unwrap(), vec![Value::Int(i64::from(skin)), Value::Int(ship), Value::Int(i64::from(hull))]);
        drained.unwrap().unwrap();
        assert!(ship_skins::get_stored_license(&reopened, character, skin).await.unwrap().is_none());
        assert!(ship_skins::applications_for_character(&reopened, character).await.unwrap().is_empty());
        assert!(crate::dao::jobs::lock_job(&reopened, &id(character)).await.unwrap().is_none());
        let (mut client, _peer) = crate::services::tests::client(reopened.clone(), character).await;
        client.session.apply(vec![("stationid", Value::Int(station))]);
        let next = crate::client::filetime_now() + infinity_core::consts::FILETIME_DAY;
        assert!(ship_skins::insert_license(&reopened, character, skin, Some(next)).await.unwrap());
        super::super::apply(&mut client, ship, Some(skin)).await.unwrap();
        assert!(ship_skins::update_expiry(&reopened, character, skin, Some(next), Some(crate::client::filetime_now() - 1)).await.unwrap());
        recover(&reopened, &world).await.unwrap();
        assert!(ship_skins::applications_for_character(&reopened, character).await.unwrap().is_empty());
        assert!(ship_skins::get_stored_license(&reopened, character, skin).await.unwrap().is_none());
        assert!(crate::dao::jobs::lock_job(&reopened, &id(character)).await.unwrap().is_none());
    }).await;
    crate::scheduler::cancel(&db, &id(character)).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn queued_renewal_and_application_precede_stale_expiry() {
    use std::{future::Future, task::Poll};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db.clone(), character).await;
        c.login.role = infinity_core::consts::ROLE_GML;
        let station = c.sde.map.stations().next().unwrap().station_id;
        c.session.apply(vec![("stationid", Value::Int(station))]);
        let (skin, hull) = c.sde.ship_skins.licenses().find_map(|license| {
            let skin = c.sde.ship_skins.skin(license.skin_id)?;
            if skin.is_structure_skin || !skin.visible(&c.cfg.server.boot_region) { return None; }
            let hull = skin.type_ids.iter().copied().find(|id| c.sde.types.get(*id).is_some_and(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)))?;
            Some((skin.skin_id, hull))
        }).unwrap();
        let flag = infinity_core::consts::FLAG_HANGAR;
        let ship: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &character, &station, &flag]).await.unwrap().get("item_id");
        let expiry = crate::client::filetime_now() + infinity_core::consts::FILETIME_DAY;
        assert!(ship_skins::insert_license(&test_db, character, skin, Some(expiry)).await.unwrap());
        schedule(&test_db, character).await.unwrap();
        let stale = crate::dao::jobs::lock_job(&test_db, &id(character)).await.unwrap().unwrap();
        let guard = market::lock().await;
        let (ready, queued) = tokio::sync::oneshot::channel();
        let renewal = tokio::spawn(async move {
            let args = [Value::Int(i64::from(skin))];
            let kwargs = Value::dict([("duration", Value::Int(1))]);
            let call = crate::services::Call { service: "shipCosmeticsMgr", method: "GiveSkin", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let mut future = Box::pin(super::super::dispatch(&mut c, &call));
            std::future::poll_fn(|cx| { assert!(future.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            ready.send(()).unwrap();
            future.await.unwrap().unwrap();
            c
        });
        queued.await.unwrap();
        let (ready, queued) = tokio::sync::oneshot::channel();
        let handler_db = test_db.clone();
        let handler_world = { let sde = Arc::new(crate::sde::Sde::load(&test_db).await.unwrap()); World::new(test_db.clone(), sde, 1) };
        let handler = tokio::spawn(async move {
            let mut future = handle(handler_db, handler_world, stale);
            std::future::poll_fn(|cx| { assert!(future.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            ready.send(()).unwrap();
            future.await.unwrap();
        });
        queued.await.unwrap();
        drop(guard);
        let mut c = renewal.await.unwrap();
        handler.await.unwrap();
        assert_eq!(ship_skins::get_stored_license(&test_db, character, skin).await.unwrap().unwrap().expires_at, Some(expiry + infinity_core::consts::FILETIME_DAY));
        let stale = crate::dao::jobs::lock_job(&test_db, &id(character)).await.unwrap().unwrap();
        let guard = market::lock().await;
        let (ready, queued) = tokio::sync::oneshot::channel();
        let application = tokio::spawn(async move {
            let mut future = Box::pin(super::super::apply(&mut c, ship, Some(skin)));
            std::future::poll_fn(|cx| { assert!(future.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            ready.send(()).unwrap();
            future.await.unwrap();
        });
        queued.await.unwrap();
        let (ready, queued) = tokio::sync::oneshot::channel();
        let handler_db = test_db.clone();
        let handler_world = { let sde = Arc::new(crate::sde::Sde::load(&test_db).await.unwrap()); World::new(test_db.clone(), sde, 1) };
        let handler = tokio::spawn(async move {
            let mut future = handle(handler_db, handler_world, stale);
            std::future::poll_fn(|cx| { assert!(future.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            ready.send(()).unwrap();
            future.await.unwrap();
        });
        queued.await.unwrap();
        drop(guard);
        application.await.unwrap();
        handler.await.unwrap();
        assert_eq!(ship_skins::applications_for_character(&test_db, character).await.unwrap().len(), 1);
        assert_eq!(ship_skins::states_for_ships(&test_db, &[ship], crate::client::filetime_now()).await.unwrap()[0].skin_id, Some(skin));
        assert_eq!(crate::dao::jobs::lock_job(&test_db, &id(character)).await.unwrap().unwrap().due_at, expiry + infinity_core::consts::FILETIME_DAY + 1);
    }).await;
    crate::scheduler::cancel(&db, &id(character)).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
