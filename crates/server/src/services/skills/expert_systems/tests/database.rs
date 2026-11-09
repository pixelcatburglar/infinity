//! Isolated database coverage for persisted expert-system lifecycle guards.
//!
//! Includes DAO guard and shared-service lifecycle behavior on a dedicated database.

use crate::{config, dao::skills::expert_systems as expert, db::Db};

async fn connected() -> Db {
    crate::tests::init_config();
    Db::connect(&config::tests::test_database()).await.unwrap()
}

async fn expert_type_ids(db: &Db, count: usize) -> Vec<i32> {
    let sde = crate::sde::Sde::load(db).await.unwrap();
    let ids: Vec<_> = sde.expert_systems.iter().filter(|definition| !definition.retired).map(|definition| definition.type_id).take(count).collect();
    assert_eq!(ids.len(), count, "initialized SDE must provide enough active expert systems");
    ids
}
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn installation_renewal_removal_and_half_open_expiry_persist() {
    let db = connected().await;
    let [type_id] = expert_type_ids(&db, 1).await.try_into().unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        assert!(expert::insert(&db, character, type_id, 100, 200).await.unwrap());
        assert!(!expert::insert(&db, character, type_id, 100, 200).await.unwrap());
        assert_eq!(expert::active(&db, character, 100).await.unwrap().len(), 1);
        assert_eq!(expert::active(&db, character, 199).await.unwrap().len(), 1);
        assert!(expert::active(&db, character, 200).await.unwrap().is_empty());
        assert_eq!(expert::all(&db, character).await.unwrap().iter().map(|row| row.expires_at).min(), Some(200));
        assert!(expert::timed_character_ids(&db, 200).await.unwrap().contains(&character));

        assert!(!expert::renew(&db, character, type_id, 199, 300).await.unwrap());
        assert!(expert::renew(&db, character, type_id, 200, 300).await.unwrap());
        assert_eq!(expert::all(&db, character).await.unwrap()[0].expires_at, 300);
        assert!(!expert::delete(&db, character, type_id, 200).await.unwrap());
        assert!(expert::delete(&db, character, type_id, 300).await.unwrap());
        assert!(expert::all(&db, character).await.unwrap().is_empty());
        Ok::<_, crate::db::Error>(())
    }).await;
    let cleanup = crate::services::tests::fixture::cleanup(&db, fixture).await;
    result.unwrap().unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn overlapping_grants_are_independently_read_and_expired() {
    let db = connected().await;
    let [type_a, type_b, type_c] = expert_type_ids(&db, 3).await.try_into().unwrap();
    let first = crate::services::tests::fixture::create(&db).await.unwrap();
    let second = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        assert!(expert::insert(&db, first.character, type_a, 100, 400).await.unwrap());
        assert!(expert::insert(&db, first.character, type_b, 200, 300).await.unwrap());
        assert!(expert::insert(&db, second.character, type_c, 150, 250).await.unwrap());

        let rows = expert::active_many(&db, &[first.character, second.character], 250).await.unwrap();
        assert_eq!(rows.iter().map(|row| row.character_id).collect::<Vec<_>>(), vec![first.character, first.character]);
        assert_eq!(rows.iter().map(|row| row.type_id).collect::<Vec<_>>(), vec![type_a, type_b]);
        assert_eq!(expert::all(&db, first.character).await.unwrap().iter().map(|row| row.expires_at).min(), Some(300));
        assert_eq!(expert::active(&db, first.character, 300).await.unwrap().iter().map(|row| row.type_id).collect::<Vec<_>>(), vec![type_a]);
        assert!(expert::active(&db, second.character, 250).await.unwrap().is_empty());
        let timed = expert::timed_character_ids(&db, 300).await.unwrap();
        assert!(timed.contains(&first.character));
        assert!(timed.contains(&second.character));
        Ok::<_, crate::db::Error>(())
    }).await;
    let cleanup_first = crate::services::tests::fixture::cleanup(&db, first).await;
    let cleanup_second = crate::services::tests::fixture::cleanup(&db, second).await;
    result.unwrap().unwrap();
    cleanup_first.unwrap();
    cleanup_second.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn stale_renewal_and_removal_cannot_clobber_newer_row() {
    let db = connected().await;
    let type_id = expert_type_ids(&db, 1).await[0];
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        assert!(expert::insert(&db, character, type_id, 10, 20).await.unwrap());
        assert!(expert::renew(&db, character, type_id, 20, 30).await.unwrap());
        assert!(!expert::renew(&db, character, type_id, 20, 40).await.unwrap());
        assert!(!expert::delete(&db, character, type_id, 20).await.unwrap());
        let row = expert::all(&db, character).await.unwrap().pop().unwrap();
        assert_eq!((row.type_id, row.expires_at), (type_id, 30));
        assert!(expert::delete(&db, character, type_id, 30).await.unwrap());
        Ok::<_, crate::db::Error>(())
    }).await;
    let cleanup = crate::services::tests::fixture::cleanup(&db, fixture).await;
    result.unwrap().unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn rolled_back_installation_leaves_no_entitlement_row() {
    let db = connected().await;
    let type_id = expert_type_ids(&db, 1).await[0];
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let cfg = config::tests::test_database();
        let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let mut tx = client.transaction().await.unwrap();
        let save = tx.savepoint("expert_install_rollback").await.unwrap();
        assert!(expert::insert(&save, character, type_id, 50, 100).await.unwrap());
        assert_eq!(expert::all(&save, character).await.unwrap().len(), 1);
        save.rollback().await.unwrap();
        assert!(expert::all(&tx, character).await.unwrap().is_empty());
        tx.rollback().await.unwrap();
        assert!(expert::all(&db, character).await.unwrap().is_empty());
        Ok::<_, crate::db::Error>(())
    }).await;
    let cleanup = crate::services::tests::fixture::cleanup(&db, fixture).await;
    result.unwrap().unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn shared_rules_enforce_slots_and_persist_removal_and_expiry() {
    use std::sync::Arc;
    use crate::{db::outbox::Notification, services::identity::Identity, space::World};

    let db = connected().await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let types: Vec<_> = sde.expert_systems.iter().filter(|d| !d.hidden && !d.retired).map(|d| d.type_id).take(4).collect();
    assert_eq!(types.len(), 4, "initialized SDE must provide four installable expert systems");
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let identity = Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let mut events = Vec::new();
        for type_id in &types[..3] {
            super::super::gm(&identity, &test_db, &world, &sde, true, *type_id, |event| { events.push(event); Ok(()) }).await.unwrap();
        }
        let before = expert::all(&test_db, fixture.character).await.unwrap();
        assert_eq!(before.len(), 3);
        let job_id = format!("{}:{}", crate::services::skills::completion::KIND, fixture.character);
        let due: i64 = test_db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap().get("due_at");
        assert_eq!(due, before.iter().map(|row| row.expires_at).min().unwrap());
        assert!(super::super::gm(&identity, &test_db, &world, &sde, true, types[3], |_| Ok(())).await.is_err());
        assert_eq!(expert::all(&test_db, fixture.character).await.unwrap(), before);
        assert!(events.iter().any(|event| matches!(event, Notification::Pilot { event, .. } if event == "OnExpertSystemsUpdated")));
        super::super::remove(&identity, &test_db, &world, &sde, types[0], |_| Ok(())).await.unwrap();
        assert_eq!(expert::all(&test_db, fixture.character).await.unwrap().len(), 2);
        let row = expert::all(&test_db, fixture.character).await.unwrap().into_iter().find(|row| row.type_id == types[1]).unwrap();
        let mut expired_events = Vec::new();
        super::super::advance_expired(&test_db, &world, &sde, fixture.character, row.expires_at, |event| { expired_events.push(event); Ok(()) }).await.unwrap();
        assert!(!expert::all(&test_db, fixture.character).await.unwrap().iter().any(|item| item.type_id == row.type_id));
        assert!(expired_events.iter().any(|event| matches!(event, Notification::Pilot { event, .. } if event == "OnExpertSystemsUpdated")));
    }).await;
    let job_id = format!("{}:{}", crate::services::skills::completion::KIND, fixture.character);
    db.execute("DELETE FROM game.jobs WHERE job_id = $1", &[&job_id]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn rejected_shared_mutation_rolls_back_expiry_and_notifications() {
    use std::sync::Arc;
    use crate::{services::identity::Identity, space::World};
    let db = connected().await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let type_id = sde.expert_systems.iter().next().expect("expert-system SDE").type_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let identity = Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
        let world = World::new(test_db.clone(), sde.clone(), 1);
        assert!(expert::insert(&test_db, fixture.character, type_id, 10, 20).await.unwrap());
        let before = expert::all(&test_db, fixture.character).await.unwrap();
        let mut dispatched = Vec::new();
        assert!(super::super::gm(&identity, &test_db, &world, &sde, true, 0, |event| { dispatched.push(event); Ok(()) }).await.is_err());
        assert_eq!(expert::all(&test_db, fixture.character).await.unwrap(), before);
        assert!(dispatched.is_empty());
        let job_id = format!("{}:{}", crate::services::skills::completion::KIND, fixture.character);
        assert!(test_db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap().is_none());
    }).await;
    let job_id = format!("{}:{}", crate::services::skills::completion::KIND, fixture.character);
    db.execute("DELETE FROM game.jobs WHERE job_id = $1", &[&job_id]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
 
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn failed_consume_restores_inventory_quantity_and_entitlement() {
    use std::sync::Arc;
    use infinity_core::consts::FLAG_HANGAR;
    use crate::{services::identity::Identity, space::World};

    let db = connected().await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let definition = sde.expert_systems.iter().find(|d| !d.hidden && !d.retired).expect("installable expert-system SDE");
    let type_id = definition.type_id;
    let station = sde.map.stations().next().expect("SDE station").station_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let item: i64 = db.query_one(
        "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id",
        &[&type_id, &fixture.character, &station, &FLAG_HANGAR],
    ).await.unwrap().get("item_id");
    let now = crate::client::filetime_now();
    assert!(expert::insert(&db, fixture.character, type_id, now - 1, i64::MAX).await.unwrap());
    let identity = Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: Some(station), system_id: None };
    let world = World::new(db.clone(), sde.clone(), 1);
    let mut dispatched = Vec::new();
    let result = super::super::consume(&identity, &db, &world, &sde, item, |event| { dispatched.push(event); Ok(()) }).await;
    assert!(result.is_err());
    let quantity: i64 = db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get("quantity");
    assert_eq!(quantity, 2);
    assert!(dispatched.is_empty());
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn valid_service_renewal_extends_the_existing_row() {
    use std::sync::Arc;
    use crate::{services::identity::Identity, space::World};

    let db = connected().await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let definition = sde.expert_systems.iter().find(|d| !d.hidden && !d.retired).expect("installable expert-system SDE");
    let type_id = definition.type_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let identity = Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let world = World::new(db.clone(), sde.clone(), 1);
    let now = crate::client::filetime_now();
    let duration = definition.duration_filetime;
    let expires = now + infinity_core::consts::FILETIME_DAY * 29;
    assert!(expert::insert(&db, fixture.character, type_id, now - 1, expires).await.unwrap());
    let before = expert::all(&db, fixture.character).await.unwrap();
    super::super::gm(&identity, &db, &world, &sde, true, type_id, |_| Ok(())).await.unwrap();
    let after = expert::all(&db, fixture.character).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].installed_at, before[0].installed_at);
    assert_eq!(after[0].expires_at, before[0].expires_at + duration);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn concurrent_renewals_use_compare_and_swap_without_clobbering() {
    let db = connected().await;
    let type_id = expert_type_ids(&db, 1).await[0];
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    assert!(expert::insert(&db, fixture.character, type_id, 10, 20).await.unwrap());
    let left = db.clone();
    let right = db.clone();
    let a = tokio::spawn(async move { expert::renew(&left, fixture.character, type_id, 20, 30).await.unwrap() });
    let b = tokio::spawn(async move { expert::renew(&right, fixture.character, type_id, 20, 40).await.unwrap() });
    let winners = [a.await.unwrap(), b.await.unwrap()].into_iter().filter(|won| *won).count();
    assert_eq!(winners, 1);
    let row = expert::all(&db, fixture.character).await.unwrap().pop().unwrap();
    assert!(row.expires_at == 30 || row.expires_at == 40);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn expert_update_event_contains_exact_map_added_flag_and_type() {
    use std::sync::Arc;
    use crate::{db::outbox::Notification, services::identity::Identity, space::World};

    let db = connected().await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let type_id = sde.expert_systems.iter().find(|d| !d.hidden && !d.retired).expect("installable expert-system SDE").type_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let identity = Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let world = World::new(db.clone(), sde.clone(), 1);
    let mut events = Vec::new();
    super::super::gm(&identity, &db, &world, &sde, true, type_id, |event| { events.push(event); Ok(()) }).await.unwrap();
    let rows = expert::all(&db, fixture.character).await.unwrap();
    let Notification::Pilot { char_id, event, args, .. } = events.iter().find(|event| matches!(event, Notification::Pilot { event, .. } if event == "OnExpertSystemsUpdated")).expect("missing OnExpertSystemsUpdated event") else {
        panic!("expert update must be a pilot notification");
    };
    assert_eq!(*char_id, fixture.character);
    assert_eq!(event, "OnExpertSystemsUpdated");
    assert_eq!(args.len(), 3);
    assert_eq!(args[1], marshal::Value::Bool(true));
    assert_eq!(args[2], marshal::Value::Int(i64::from(type_id)));
    let marshal::Value::Dict(map) = &args[0] else { panic!("expert update map must be a dict") };
    assert_eq!(map.len(), 1);
    assert_eq!(map[0].0, marshal::Value::Int(i64::from(type_id)));
    let marshal::Value::Tuple(times) = &map[0].1 else { panic!("expert update value must be a tuple") };
    assert_eq!(times.as_slice(), &[marshal::Value::Int(rows[0].installed_at), marshal::Value::Int(rows[0].expires_at)]);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
 
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn manual_install_and_remove_preserve_training_deadline_and_banked_sp() {
    use std::sync::Arc;
    use crate::{dao::skills::{queue, training}, services::identity::Identity, space::World};

    let db = connected().await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let definition = sde.expert_systems.iter().find(|d| !d.hidden && !d.retired).expect("installable expert-system SDE");
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let now = crate::client::filetime_now();
    let skill = definition.skills.first().map(|(id, _)| *id).expect("expert definition skill");
    training::starting_skill(&db, fixture.character, skill, 0, 0).await.unwrap();
    queue::insert(&db, fixture.character, &queue::QueueEntry {
        position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: 1_000,
        start_time: Some(now), end_time: Some(now + infinity_core::consts::FILETIME_DAY),
    }).await.unwrap();
    let identity = Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let world = World::new(db.clone(), sde.clone(), 1);
    let before = queue::list(&db, fixture.character).await.unwrap();
    let before_skill = training::current(&db, fixture.character).await.unwrap();
    super::super::gm(&identity, &db, &world, &sde, true, definition.type_id, |_| Ok(())).await.unwrap();
    let installed = queue::list(&db, fixture.character).await.unwrap();
    assert_eq!(installed[0].position, before[0].position);
    assert_eq!(installed[0].end_time, before[0].end_time);
    assert!(training::current(&db, fixture.character).await.unwrap() == before_skill);
    super::super::remove(&identity, &db, &world, &sde, definition.type_id, |_| Ok(())).await.unwrap();
    let removed = queue::list(&db, fixture.character).await.unwrap();
    assert_eq!(removed[0].position, before[0].position);
    assert_eq!(removed[0].end_time, before[0].end_time);
    assert!(training::current(&db, fixture.character).await.unwrap() == before_skill);
    let job_id = format!("{}:{}", crate::services::skills::completion::KIND, fixture.character);
    db.execute("DELETE FROM game.jobs WHERE job_id = $1", &[&job_id]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
 
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn alpha_authority_installation_projects_virtual_skill_without_mutating_training() {
    use std::sync::Arc;
    use infinity_core::consts::GM_ROLES;
    use crate::{
        dao::{accounts::subscriptions, skills::{history, training}},
        services::{identity::Identity, skills::effective, subscriptions::{management, rules}},
        space::World,
    };

    let db = connected().await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let definition = sde.expert_systems.iter()
        .find(|definition| !definition.hidden && !definition.retired && definition.skills.iter().any(|(_, level)| *level > 0))
        .expect("installable expert system with a skill grant");
    let expert_type_id = definition.type_id;
    let &(skill, grant_level) = definition.skills.iter().find(|(_, level)| *level > 0).unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let now = crate::client::filetime_now();
        training::starting_skill(&test_db, fixture.character, skill, 0, 123).await.unwrap();
        let before_skills = training::current(&test_db, fixture.character).await.unwrap();
        let before_history: Vec<_> = history::list(&test_db, fixture.character, 50).await.unwrap().into_iter()
            .map(|row| (row.skill_type_id, row.event_type_id, row.absolute_points, row.log_date))
            .collect();
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let identity = Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };

        management::set(&test_db, &world, fixture.account, GM_ROLES, fixture.account, rules::ALPHA, None, 0).await.unwrap();
        let authority = subscriptions::by_user(&test_db, fixture.account).await.unwrap();
        assert_eq!(rules::current(&test_db, fixture.account, now).await.unwrap(), rules::ALPHA);
        assert_eq!(authority.grade, rules::ALPHA);

        let before = effective::snapshot_at(&test_db, &sde, fixture.character, now).await.unwrap();
        assert_eq!(before.level(skill), 0);
        super::super::gm(&identity, &test_db, &world, &sde, true, expert_type_id, |_| Ok(())).await.unwrap();
        let installed = effective::snapshot_at(&test_db, &sde, fixture.character, crate::client::filetime_now()).await.unwrap();
        assert!(installed.level(skill) >= grant_level);
        assert!(installed.level(skill) > before.level(skill));
        assert_eq!(installed.get(skill).unwrap().virtual_level, Some(grant_level));
        assert!(installed.get(skill).unwrap().trained.as_ref() == before.get(skill).unwrap().trained.as_ref());
        assert!(training::current(&test_db, fixture.character).await.unwrap() == before_skills);
        assert!(history::list(&test_db, fixture.character, 50).await.unwrap().into_iter()
            .map(|row| (row.skill_type_id, row.event_type_id, row.absolute_points, row.log_date))
            .collect::<Vec<_>>() == before_history);

        super::super::remove(&identity, &test_db, &world, &sde, expert_type_id, |_| Ok(())).await.unwrap();
        let reverted = effective::snapshot_at(&test_db, &sde, fixture.character, crate::client::filetime_now()).await.unwrap();
        assert_eq!(reverted.level(skill), 0);
        assert!(training::current(&test_db, fixture.character).await.unwrap() == before_skills);
        assert!(history::list(&test_db, fixture.character, 50).await.unwrap().into_iter()
            .map(|row| (row.skill_type_id, row.event_type_id, row.absolute_points, row.log_date))
            .collect::<Vec<_>>() == before_history);
        assert!(crate::dao::skills::expert_systems::all(&test_db, fixture.character).await.unwrap().is_empty());
        Ok::<_, crate::services::CallError>(())
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id = $1", &[&format!("{}:{}", crate::services::skills::completion::KIND, fixture.character)]).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id = $1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap().unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn lifecycle_install_timestamp_is_captured_after_character_lock() {
    use std::{sync::Arc, time::Duration};
    use crate::{db::outbox::Notification, services::identity::Identity, space::World};

    let db = connected().await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let definition = sde.expert_systems.iter()
        .find(|definition| !definition.hidden && !definition.retired)
        .expect("installable expert-system definition");
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let cfg = config::tests::test_database();
    let (mut lock_client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let lock_tx = lock_client.transaction().await.unwrap();
    crate::dao::characters::selection::lock_character(&lock_tx, fixture.character).await.unwrap();

    let operation_db = db.clone();
    let operation_sde = Arc::clone(&sde);
    let operation_type_id = definition.type_id;
    let operation = tokio::spawn(async move {
        let world = World::new(operation_db.clone(), operation_sde.clone(), 1);
        let identity = Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
        let mut events = Vec::new();
        let result = super::super::gm(&identity, &operation_db, &world, &operation_sde, true, operation_type_id, |event| {
            events.push(event);
            Ok(())
        }).await;
        (result, events)
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    let t_release = crate::client::filetime_now();
    lock_tx.commit().await.unwrap();
    let (result, events) = operation.await.unwrap();
    result.unwrap();

    let rows = expert::all(&db, fixture.character).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].installed_at >= t_release, "installation timestamp predates character-lock release");
    let Notification::Pilot { event, args, .. } = events.iter()
        .find(|event| matches!(event, Notification::Pilot { event, .. } if event == "OnExpertSystemsUpdated"))
        .expect("missing expert-system update notification") else {
        panic!("expert update must be a pilot notification");
    };
    assert_eq!(event, "OnExpertSystemsUpdated");
    let marshal::Value::Dict(map) = &args[0] else { panic!("expert update map must be a dict") };
    let marshal::Value::Tuple(times) = &map[0].1 else { panic!("expert update value must be a tuple") };
    assert!(matches!(times.first(), Some(marshal::Value::Int(installed_at)) if *installed_at >= t_release));

    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn expert_install_and_expiry_do_not_emit_skill_state_changed() {
    use std::sync::Arc;
    use crate::{dao::skills::{queue, training}, lua::events::GameEvent, services::{identity::Identity, tests::fixture}, space::World};
    let db = connected().await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let definition = sde.expert_systems.iter().find(|d| !d.hidden && !d.retired).expect("installable expert system");
    let skill = definition.skills.first().expect("expert skill").0;
    let owner = fixture::create(&db).await.unwrap();
    training::starting_skill(&db, owner.character, skill, 0, 0).await.unwrap();
    queue::insert(&db, owner.character, &queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: 100, start_time: Some(1), end_time: Some(2) }).await.unwrap();
    let identity = Identity { char_id: owner.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let world = World::new(db.clone(), sde.clone(), 1);
    super::super::gm(&identity, &db, &world, &sde, true, definition.type_id, |_| Ok(())).await.unwrap();
    let trained = crate::db::outbox::tests::events(&db, "SkillTrained", owner.character).await;
    assert!(trained.iter().any(|event| matches!(event, GameEvent::SkillTrained { char_id, type_id, level: 1 } if *char_id == owner.character && *type_id == skill)));
    assert!(crate::db::outbox::tests::events(&db, "SkillStateChanged", owner.character).await.is_empty());
    let expiry = expert::all(&db, owner.character).await.unwrap()[0].expires_at;
    super::super::advance_expired(&db, &world, &sde, owner.character, expiry, |_| Ok(())).await.unwrap();
    assert!(expert::all(&db, owner.character).await.unwrap().is_empty());
    assert!(crate::db::outbox::tests::events(&db, "SkillStateChanged", owner.character).await.is_empty());
    assert_eq!(crate::db::outbox::tests::events(&db, "SkillTrained", owner.character).await.len(), trained.len());
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", crate::services::skills::completion::KIND, owner.character)]).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
}
